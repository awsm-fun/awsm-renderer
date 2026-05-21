// Occlusion cull compute.
//
// One workgroup_size(64) thread per instance. For each instance:
//   1. Frustum-test the world AABB against the 6 view-proj-derived
//      planes; if fully outside any plane, write 0u and exit.
//   2. Project the 8 AABB corners through `view_proj`, take the
//      screen-space (NDC → 0..1 UV) min/max, plus the min `clip.z/clip.w`
//      depth — the "closest" depth of the AABB.
//   3. Pick the appropriate HZB mip from the screen-space extent
//      (`mip = ceil(log2(max(width_px, height_px)))`).
//   4. Sample HZB at that mip's `textureLoad` (no sampler-based
//      reads; compute can't derive gradients). The HZB stores max
//      depth per tile. If our closest depth is greater (i.e. the
//      AABB sits farther than every fragment in the region), mark
//      occluded.
//
// `visible_this_frame[i]` ends as 1u (visible) or 0u (culled).
// Consumed by the compaction pass to gate `drawIndirect.instance_count`.

{% include "shared_wgsl/camera.wgsl" %}

struct OcclusionInstance {
    world_aabb_min: vec3<f32>,
    _pad0: u32,
    world_aabb_max: vec3<f32>,
    _pad1: u32,
    mesh_meta_offset: u32,
    instance_attr_base: u32,
    // Carried into the compaction shader so it can write the
    // full IndirectDrawArgs slot (static fields + atomicAdded
    // instance_count). This breaks the previous race where CPU
    // `queue.writeBuffer` overwrote the args slot before the
    // submitted command buffer's geometry pass executed.
    index_count: u32,
    _pad2: u32,
};

struct OcclusionParams {
    active_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(1) var hzb_tex: texture_2d<f32>;
@group(0) @binding(2) var<storage, read> instances: array<OcclusionInstance>;
@group(0) @binding(3) var<storage, read_write> visible_this_frame: array<u32>;
@group(0) @binding(4) var<uniform> params: OcclusionParams;

// Extract the 6 frustum planes (left, right, bottom, top, near, far)
// in world-space from a row-major view_proj. WGSL `mat4x4<f32>` is
// stored column-major, so we read the rows via component access.
fn frustum_plane(view_proj: mat4x4<f32>, row_idx: u32) -> vec4<f32> {
    // Row i of `view_proj` is the vector `(view_proj[0][i], view_proj[1][i], view_proj[2][i], view_proj[3][i])`.
    return vec4<f32>(
        view_proj[0][row_idx],
        view_proj[1][row_idx],
        view_proj[2][row_idx],
        view_proj[3][row_idx],
    );
}

fn extract_planes(view_proj: mat4x4<f32>) -> array<vec4<f32>, 6> {
    let row0 = frustum_plane(view_proj, 0u);
    let row1 = frustum_plane(view_proj, 1u);
    let row2 = frustum_plane(view_proj, 2u);
    let row3 = frustum_plane(view_proj, 3u);
    // Gribb-Hartmann plane extraction:
    //   left   = row3 + row0
    //   right  = row3 - row0
    //   bottom = row3 + row1
    //   top    = row3 - row1
    //   near   = row3 + row2   (WebGPU's [0,1] depth → near is `row2`-positive)
    //   far    = row3 - row2
    return array<vec4<f32>, 6>(
        row3 + row0,
        row3 - row0,
        row3 + row1,
        row3 - row1,
        row2,        // near (D = 0 in NDC)
        row3 - row2, // far
    );
}

// True if the AABB lies fully on the negative side of `plane`.
fn aabb_outside_plane(
    plane: vec4<f32>,
    aabb_min: vec3<f32>,
    aabb_max: vec3<f32>,
) -> bool {
    // Pick the AABB's "positive" vertex along the plane normal —
    // the corner most likely to be inside. If even that one is
    // outside, the whole AABB is outside.
    let positive = vec3<f32>(
        select(aabb_min.x, aabb_max.x, plane.x >= 0.0),
        select(aabb_min.y, aabb_max.y, plane.y >= 0.0),
        select(aabb_min.z, aabb_max.z, plane.z >= 0.0),
    );
    return dot(plane.xyz, positive) + plane.w < 0.0;
}

struct ScreenAabb {
    uv_min: vec2<f32>,
    uv_max: vec2<f32>,
    depth_min: f32,
    visible: bool,
};

fn project_corner(view_proj: mat4x4<f32>, corner: vec3<f32>) -> vec4<f32> {
    return view_proj * vec4<f32>(corner, 1.0);
}

fn aabb_to_screen(
    view_proj: mat4x4<f32>,
    aabb_min: vec3<f32>,
    aabb_max: vec3<f32>,
) -> ScreenAabb {
    var uv_min = vec2<f32>(1.0, 1.0);
    var uv_max = vec2<f32>(0.0, 0.0);
    var depth_min: f32 = 1.0;
    var any_in_front = false;

    let xs = array<f32, 2>(aabb_min.x, aabb_max.x);
    let ys = array<f32, 2>(aabb_min.y, aabb_max.y);
    let zs = array<f32, 2>(aabb_min.z, aabb_max.z);

    for (var i = 0u; i < 8u; i++) {
        let corner = vec3<f32>(xs[i & 1u], ys[(i >> 1u) & 1u], zs[(i >> 2u) & 1u]);
        let clip = project_corner(view_proj, corner);
        // If any corner sits behind the near plane (w <= 0) we
        // conservatively treat the AABB as visible — the HZB test
        // doesn't apply cleanly to clipped corners.
        if (clip.w <= 0.0) {
            return ScreenAabb(vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0), 0.0, true);
        }
        let inv_w = 1.0 / clip.w;
        let ndc = clip.xyz * inv_w;
        let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        uv_min = min(uv_min, uv);
        uv_max = max(uv_max, uv);
        depth_min = min(depth_min, ndc.z);
        any_in_front = true;
    }

    return ScreenAabb(uv_min, uv_max, depth_min, any_in_front);
}

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    // `arrayLength(&instances)` returns the binding's *capacity*
    // (the OcclusionBuffers `INITIAL_CAPACITY` × grow-by-2 sizing),
    // not this frame's active count. The CPU dispatches
    // `ceil(active/64)` workgroups, so tail threads in the rounded
    // dispatch land inside arrayLength and would otherwise process
    // stale `instances[i]` left over from prior frames. Bound by
    // `params.active_count` to keep them inert.
    let count = params.active_count;
    let i = gid.x;
    if (i >= count) {
        return;
    }
    let instance = instances[i];

    let planes = extract_planes(camera_raw.view_proj);
    for (var p = 0u; p < 6u; p++) {
        if (aabb_outside_plane(planes[p], instance.world_aabb_min, instance.world_aabb_max)) {
            visible_this_frame[i] = 0u;
            return;
        }
    }

    let screen = aabb_to_screen(
        camera_raw.view_proj,
        instance.world_aabb_min,
        instance.world_aabb_max,
    );
    if (!screen.visible) {
        visible_this_frame[i] = 0u;
        return;
    }

    // HZB lookup. The HZB stores max depth per tile. Compute the
    // mip level whose texel fully covers our screen-space AABB:
    //
    //   mip = ceil(log2(max(width_px, height_px)))
    //
    // Clamp to the HZB's mip count.
    let hzb_dims_mip0 = vec2<f32>(textureDimensions(hzb_tex, 0));
    let screen_size_px = (screen.uv_max - screen.uv_min) * hzb_dims_mip0;
    let extent_px = max(screen_size_px.x, screen_size_px.y);
    let mip_f = max(0.0, ceil(log2(max(extent_px, 1.0))));
    let mip_count = i32(textureNumLevels(hzb_tex));
    let mip = clamp(i32(mip_f), 0, mip_count - 1);

    // Sample the HZB at the AABB's center texel in the chosen mip.
    let center_uv = (screen.uv_min + screen.uv_max) * 0.5;
    let mip_dims = vec2<f32>(textureDimensions(hzb_tex, mip));
    let coord = vec2<i32>(
        clamp(i32(center_uv.x * mip_dims.x), 0, i32(mip_dims.x) - 1),
        clamp(i32(center_uv.y * mip_dims.y), 0, i32(mip_dims.y) - 1),
    );
    let hzb_depth = textureLoad(hzb_tex, coord, mip).x;

    // Occlusion test: our closest depth must be ≤ hzb max depth to
    // possibly be visible. WebGPU depth is `[0, 1]` with 1 = far.
    if (screen.depth_min > hzb_depth) {
        visible_this_frame[i] = 0u;
        return;
    }

    visible_this_frame[i] = 1u;
}
