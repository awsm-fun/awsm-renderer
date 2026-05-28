// Light culling compute shader.
//
// One workgroup per froxel (tile_x, tile_y, z_slice). Threads inside the
// workgroup chunk the punctual-light list and atomically append the lights
// whose world-space bounding sphere overlaps the froxel's view-space frustum
// into `froxel_indices` at `froxel_idx * MAX_PER_FROXEL_CAPACITY + slot`.
//
// Saturation handling: `atomicAdd(&froxel_counts[i], 1u)` keeps incrementing
// even past `MAX_PER_FROXEL_CAPACITY`. When the returned slot is ≥ capacity,
// the index write is skipped and `overflow_counter` is bumped. Consumer
// shaders clamp `count` to `MAX_PER_FROXEL_CAPACITY` at read time. The CPU's
// `mapAsync` readback of `overflow_counter` drives the auto-grow path.

const TILE_PIXEL_SIZE: u32 = 16u;
const SLICE_COUNT: u32 = {{ slice_count }}u;
const MAX_PER_FROXEL_CAPACITY: u32 = {{ max_per_froxel_capacity }}u;
// Stride per froxel in `froxel_storage`: one u32 for the count + capacity for the indices.
const FROXEL_STRIDE: u32 = MAX_PER_FROXEL_CAPACITY + 1u;
const WORKGROUP_SIZE_LIGHTS: u32 = 64u;

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

@compute @workgroup_size(WORKGROUP_SIZE_LIGHTS)
fn cs_main(
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
) {
    let tile_x = wid.x;
    let tile_y = wid.y;
    let z_slice = wid.z;

    // Padded dispatch: workgroups past the valid tile/slice extents
    // early-return (we don't size the dispatch with arithmetic in the
    // shader because the host already does the ceil-div).
    if (tile_x >= cull_params.tiles_x || tile_y >= cull_params.tiles_y || z_slice >= SLICE_COUNT) {
        return;
    }

    let tiles_per_layer = cull_params.tiles_x * cull_params.tiles_y;
    let froxel_idx = z_slice * tiles_per_layer + tile_y * cull_params.tiles_x + tile_x;
    // Base offset of this froxel inside the merged count+indices storage.
    let froxel_base = froxel_idx * FROXEL_STRIDE;

    // Thread 0 of each workgroup resets the per-froxel count. The atomic
    // store + workgroupBarrier guarantees other threads in this workgroup
    // see 0 before issuing their own atomicAdds. Cross-workgroup synchro-
    // nisation isn't required — each workgroup owns a distinct froxel_idx.
    if (lid.x == 0u) {
        atomicStore(&froxel_storage[froxel_base], 0u);
    }

    // ── Reconstruct the froxel's view-space frustum ───────────────
    // Side planes from the four corner NDC rays of the screen tile;
    // near/far Z planes from the exponential slice mapping.
    let viewport_f = vec2<f32>(f32(cull_params.viewport_w), f32(cull_params.viewport_h));
    let tile_pixel_min = vec2<f32>(f32(tile_x), f32(tile_y)) * f32(TILE_PIXEL_SIZE);
    let tile_pixel_max = min(tile_pixel_min + vec2<f32>(f32(TILE_PIXEL_SIZE)), viewport_f);

    // WebGPU screen-space: top-left origin, Y down. NDC: +Y up. The Y flip
    // in the conversion keeps the tile's NDC bottom edge at the screen's
    // TOP pixel row — but we only care about coverage, not orientation,
    // so the cross-product orientation below is what matters.
    let ndc_x_min = tile_pixel_min.x / viewport_f.x * 2.0 - 1.0;
    let ndc_x_max = tile_pixel_max.x / viewport_f.x * 2.0 - 1.0;
    let ndc_y_min = 1.0 - tile_pixel_max.y / viewport_f.y * 2.0;
    let ndc_y_max = 1.0 - tile_pixel_min.y / viewport_f.y * 2.0;

    let bl = ndc_to_view_dir(vec2<f32>(ndc_x_min, ndc_y_min));
    let br = ndc_to_view_dir(vec2<f32>(ndc_x_max, ndc_y_min));
    let tl = ndc_to_view_dir(vec2<f32>(ndc_x_min, ndc_y_max));
    let tr = ndc_to_view_dir(vec2<f32>(ndc_x_max, ndc_y_max));

    // Side-plane inward normals. Right-handed cross products oriented so
    // dot(normal, frustum_interior_ray) > 0. We pick `(top_corner × bot_corner)`
    // for the left plane (and the mirror for right) — this produces a
    // normal pointing into the frustum given a right-handed coordinate
    // system (+Y up, looking -Z).
    let left_normal = normalize(cross(tl, bl));
    let right_normal = normalize(cross(br, tr));
    let top_normal = normalize(cross(tr, tl));
    let bottom_normal = normalize(cross(bl, br));

    // Z range for this slice (exponential mapping). Both bounds are
    // positive view-space depths (the rest of the renderer represents
    // `view_z` as `-(view * world).z`, positive forward).
    let s_lo = f32(z_slice) / f32(SLICE_COUNT);
    let s_hi = f32(z_slice + 1u) / f32(SLICE_COUNT);
    let z_lo = cull_params.z_near * exp(cull_params.log_far_over_near * s_lo);
    let z_hi = cull_params.z_near * exp(cull_params.log_far_over_near * s_hi);

    // Sync before the atomic-append loop so all threads see the zeroed
    // counter.
    workgroupBarrier();

    // ── Per-thread light chunk ───────────────────────────────────
    let total_lights = lights_info.data.x;  // n_lights — same field the shared lights.wgsl reads.
    let lid_x = lid.x;

    // Strided iteration: thread i handles lights i, i + WG, i + 2*WG, ...
    // Strided is friendlier to the GPU's coalesced uniform-buffer reads
    // than block-contiguous chunks.
    var li = lid_x;
    loop {
        if (li >= total_lights) { break; }

        let p = lights[li];
        let kind = u32(p.kind_outer_pad.x);
        // Skip directional lights — they have infinite extent and live
        // in the shading shaders' global-prefix loop instead.
        if (kind != 1u) {
            let pos_world = p.pos_range.xyz;
            let range = p.pos_range.w;

            // World → view-space. We treat the standard "looking down -Z"
            // convention: view_z = -(view * world).z (positive forward).
            let pos_view4 = camera_raw.view * vec4<f32>(pos_world, 1.0);
            let pos_view = pos_view4.xyz;
            let view_z = -pos_view.z;

            // Z-plane test: sphere span [view_z - range, view_z + range]
            // must overlap [z_lo, z_hi]. Reject if disjoint.
            let z_ok = (view_z + range >= z_lo) && (view_z - range <= z_hi);

            // Side-plane test: signed distance from each plane to the
            // sphere center must be ≥ -range (inside, or partially
            // straddling). Reject if any plane fully excludes the sphere.
            // For spot lights we use the same conservative sphere test —
            // the actual cone is tighter but a false-positive at the
            // cull stage just means the shading shader does the cone
            // test per pixel (which it does anyway).
            let l_ok = signed_dist_through_origin(left_normal, pos_view) >= -range;
            let r_ok = signed_dist_through_origin(right_normal, pos_view) >= -range;
            let t_ok = signed_dist_through_origin(top_normal, pos_view) >= -range;
            let b_ok = signed_dist_through_origin(bottom_normal, pos_view) >= -range;

            if (z_ok && l_ok && r_ok && t_ok && b_ok) {
                // Atomic-append into this froxel's slice. Slot 0 of the
                // stride holds the count; light indices live at slots
                // 1..1+MAX_PER_FROXEL_CAPACITY.
                let slot = atomicAdd(&froxel_storage[froxel_base], 1u);
                if (slot < MAX_PER_FROXEL_CAPACITY) {
                    atomicStore(&froxel_storage[froxel_base + 1u + slot], li);
                } else {
                    // We bumped the count past capacity. Tell the CPU.
                    // The consumer reads `min(count, MAX_PER_FROXEL_CAPACITY)`
                    // so the out-of-budget lights are silently dropped this
                    // frame; the auto-grow path raises the budget next.
                    atomicAdd(&overflow_counter, 1u);
                }
            }
        }

        li = li + WORKGROUP_SIZE_LIGHTS;
    }
}
