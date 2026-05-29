// Light culling compute shader (two-level / clustered).
//
// Two @compute entry points run as consecutive dispatches in one compute
// pass:
//
//   cs_tile  (Stage A): one workgroup per 2D screen tile (tile_x, tile_y).
//     Tests each punctual light's bounding sphere against the tile
//     column's four side planes (which are Z-independent) and
//     atomic-appends the survivors into the tile's slice of `tile_lights`.
//     The expensive side-plane test runs once per (tile, light) here
//     instead of once per (froxel, light) — a ~SLICE_COUNT× reduction in
//     side-plane work.
//
//   cs_main  (Stage B): one workgroup per froxel (tile_x, tile_y, z_slice).
//     Reads only its tile's candidate list from `tile_lights` and applies
//     the cheap per-slice Z-test, atomic-appending survivors into the
//     froxel's slice of `lights_storage`. The output is identical to the
//     old single-pass cull (same per-froxel lists) because the side
//     planes don't depend on Z — so the tile candidate set is exactly the
//     union over the column's froxels. `overflow_counter` + the runtime
//     `max_per_froxel_capacity` auto-grow behave exactly as before.
//
// WebGPU inserts a memory barrier between the two dispatches (cs_main
// reads `tile_lights` that cs_tile wrote), so a single compute pass
// suffices.
//
// The per-froxel slice base in `lights_storage` is
// `cull_params.mesh_indices_capacity_u32 + froxel_idx * stride`, where
// `stride = cull_params.max_per_froxel_capacity + 1` (slot 0 = atomic
// count, slots 1.. = light indices). The head region
// `[0..mesh_indices_capacity_u32)` is the CPU-written per-mesh slice and
// is left untouched here.

const TILE_PIXEL_SIZE: u32 = 16u;
const SLICE_COUNT: u32 = {{ slice_count }}u;
const WORKGROUP_SIZE_LIGHTS: u32 = 64u;
// Per-2D-tile candidate budget. Matches `MAX_PUNCTUAL_LIGHTS` (the cull
// can never see more lights than exist), so a tile slice can't overflow
// and Stage B needs no fallback. Kept in lockstep with the
// `TILE_LIGHT_CAPACITY` constant in `light_culling/buffers.rs`.
const TILE_LIGHT_CAPACITY: u32 = {{ max_punctual_lights }}u;
const TILE_LIGHT_STRIDE: u32 = TILE_LIGHT_CAPACITY + 1u;

// Project an NDC corner (z = 0 → near plane) to a normalized view-space
// ray direction emanating from the camera origin.
fn ndc_to_view_dir(ndc: vec2<f32>) -> vec3<f32> {
    let clip = vec4<f32>(ndc, 0.0, 1.0);
    let view = camera_raw.inv_proj * clip;
    return normalize(view.xyz / view.w);
}

// Distance from sphere center to the plane `dot(normal, p) = 0` passing
// through the origin. Positive when the center is on the inward side.
fn signed_dist_through_origin(normal: vec3<f32>, p: vec3<f32>) -> f32 {
    return dot(normal, p);
}

// The four inward side-plane normals of a screen tile's view-space
// frustum column. Z-independent, so every froxel in the column shares
// them — which is exactly why Stage A can compute the side test once.
struct SidePlanes {
    left: vec3<f32>,
    right: vec3<f32>,
    top: vec3<f32>,
    bottom: vec3<f32>,
};

fn tile_side_planes(tile_x: u32, tile_y: u32) -> SidePlanes {
    let viewport_f = vec2<f32>(f32(cull_params.viewport_w), f32(cull_params.viewport_h));
    let tile_pixel_min = vec2<f32>(f32(tile_x), f32(tile_y)) * f32(TILE_PIXEL_SIZE);
    let tile_pixel_max = min(tile_pixel_min + vec2<f32>(f32(TILE_PIXEL_SIZE)), viewport_f);

    // WebGPU screen-space: top-left origin, Y down. NDC: +Y up.
    let ndc_x_min = tile_pixel_min.x / viewport_f.x * 2.0 - 1.0;
    let ndc_x_max = tile_pixel_max.x / viewport_f.x * 2.0 - 1.0;
    let ndc_y_min = 1.0 - tile_pixel_max.y / viewport_f.y * 2.0;
    let ndc_y_max = 1.0 - tile_pixel_min.y / viewport_f.y * 2.0;

    let bl = ndc_to_view_dir(vec2<f32>(ndc_x_min, ndc_y_min));
    let br = ndc_to_view_dir(vec2<f32>(ndc_x_max, ndc_y_min));
    let tl = ndc_to_view_dir(vec2<f32>(ndc_x_min, ndc_y_max));
    let tr = ndc_to_view_dir(vec2<f32>(ndc_x_max, ndc_y_max));

    // Right-handed cross products oriented so dot(normal, interior_ray) > 0.
    var planes: SidePlanes;
    planes.left = normalize(cross(tl, bl));
    planes.right = normalize(cross(br, tr));
    planes.top = normalize(cross(tr, tl));
    planes.bottom = normalize(cross(bl, br));
    return planes;
}

// ── Stage A: per-2D-tile side-plane cull ──────────────────────────────
@compute @workgroup_size(WORKGROUP_SIZE_LIGHTS)
fn cs_tile(
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
) {
    let tile_x = wid.x;
    let tile_y = wid.y;
    if (tile_x >= cull_params.tiles_x || tile_y >= cull_params.tiles_y) {
        return;
    }

    let tile_idx = tile_y * cull_params.tiles_x + tile_x;
    let tile_base = tile_idx * TILE_LIGHT_STRIDE;

    // Thread 0 resets the per-tile candidate count.
    if (lid.x == 0u) {
        atomicStore(&tile_lights[tile_base], 0u);
    }

    let planes = tile_side_planes(tile_x, tile_y);

    // Sync so all threads see the zeroed count before appending.
    workgroupBarrier();

    let total_lights = lights_info.data.x;  // n_lights
    var li = lid.x;
    loop {
        if (li >= total_lights) { break; }

        let p = lights[li];
        let kind = u32(p.kind_outer_pad.x);
        // Skip directional lights — infinite extent; they live in the
        // shading shaders' global-prefix loop.
        if (kind != 1u) {
            let pos_world = p.pos_range.xyz;
            let range = p.pos_range.w;
            let pos_view = (camera_raw.view * vec4<f32>(pos_world, 1.0)).xyz;

            // Side-plane test only (no Z): a sphere straddling or inside
            // every side plane is a candidate for some froxel in this
            // column. Stage B applies the Z-slice test.
            let l_ok = signed_dist_through_origin(planes.left, pos_view) >= -range;
            let r_ok = signed_dist_through_origin(planes.right, pos_view) >= -range;
            let t_ok = signed_dist_through_origin(planes.top, pos_view) >= -range;
            let b_ok = signed_dist_through_origin(planes.bottom, pos_view) >= -range;

            if (l_ok && r_ok && t_ok && b_ok) {
                let slot = atomicAdd(&tile_lights[tile_base], 1u);
                // Capacity == MAX_PUNCTUAL_LIGHTS, so `slot` is always in
                // range; the guard is defense-in-depth.
                if (slot < TILE_LIGHT_CAPACITY) {
                    atomicStore(&tile_lights[tile_base + 1u + slot], li);
                }
            }
        }

        li = li + WORKGROUP_SIZE_LIGHTS;
    }
}

// ── Stage B: per-froxel Z-slice refine ────────────────────────────────
@compute @workgroup_size(WORKGROUP_SIZE_LIGHTS)
fn cs_main(
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
) {
    let tile_x = wid.x;
    let tile_y = wid.y;
    let z_slice = wid.z;

    if (tile_x >= cull_params.tiles_x || tile_y >= cull_params.tiles_y || z_slice >= SLICE_COUNT) {
        return;
    }

    let tiles_per_layer = cull_params.tiles_x * cull_params.tiles_y;
    let froxel_idx = z_slice * tiles_per_layer + tile_y * cull_params.tiles_x + tile_x;
    let froxel_stride = cull_params.max_per_froxel_capacity + 1u;
    let froxel_base = cull_params.mesh_indices_capacity_u32 + froxel_idx * froxel_stride;

    // Thread 0 resets the per-froxel count.
    if (lid.x == 0u) {
        atomicStore(&lights_storage[froxel_base], 0u);
    }

    // Z range for this slice (exponential mapping). Positive forward.
    let s_lo = f32(z_slice) / f32(SLICE_COUNT);
    let s_hi = f32(z_slice + 1u) / f32(SLICE_COUNT);
    let z_lo = cull_params.z_near * exp(cull_params.log_far_over_near * s_lo);
    let z_hi = cull_params.z_near * exp(cull_params.log_far_over_near * s_hi);

    // Candidates from this froxel's 2D tile (written by cs_tile). All are
    // non-directional and already passed the side planes.
    let tile_idx = tile_y * cull_params.tiles_x + tile_x;
    let tile_base = tile_idx * TILE_LIGHT_STRIDE;
    let cand_count = min(atomicLoad(&tile_lights[tile_base]), TILE_LIGHT_CAPACITY);

    // Sync so all threads see the zeroed froxel count before appending.
    workgroupBarrier();

    var ci = lid.x;
    loop {
        if (ci >= cand_count) { break; }

        let light_index = atomicLoad(&tile_lights[tile_base + 1u + ci]);
        let p = lights[light_index];
        let pos_world = p.pos_range.xyz;
        let range = p.pos_range.w;
        let view_z = -(camera_raw.view * vec4<f32>(pos_world, 1.0)).z;

        // Z-plane test: sphere span [view_z - range, view_z + range] must
        // overlap [z_lo, z_hi]. The side planes already passed in Stage A.
        let z_ok = (view_z + range >= z_lo) && (view_z - range <= z_hi);

        if (z_ok) {
            let slot = atomicAdd(&lights_storage[froxel_base], 1u);
            if (slot < cull_params.max_per_froxel_capacity) {
                atomicStore(&lights_storage[froxel_base + 1u + slot], light_index);
            } else {
                // Past capacity: the index is dropped this frame (consumers
                // clamp `count` to `max_per_froxel_capacity`); the CPU's
                // overflow readback raises the budget next frame.
                atomicAdd(&overflow_counter, 1u);
            }
        }

        ci = ci + WORKGROUP_SIZE_LIGHTS;
    }
}
