// Cluster-LOD cut compute (Phase B, B.2).
//
// One workgroup_size(64) thread per cluster page. Evaluates the SAME per-cluster
// rule as the CPU reference `cluster_lod::select_cut_per_cluster`: project this
// cluster's `lod_error` against its group `lod_bounds` sphere and its
// `parent_error` against its group `parent_bounds` sphere, then select the
// cluster when its own projected error fits the pixel budget but its parent's
// does not:
//
//     proj_lod <= pixel_budget < proj_parent   ⇒   selected[i] = 1
//
// Because each cluster uses ITS OWN distance, detail varies within one mesh
// (near clusters fine, far clusters coarse); because the projection spheres are
// group-shared, adjacent clusters of a group flip together ⇒ crack-free.
//
// `selected[i]` ends 1u (draw this cluster's index page) or 0u. A later
// compaction pass appends the selected pages into one compacted index stream +
// `drawIndexedIndirect` args (B.2 continued).

// Mirror of `cluster_lod::ClusterPage` GPU layout — see `CLUSTER_PAGE_GPU_STRIDE`
// (64 B std430: each vec3 aligns to 16 and the trailing f32 fills the slot).
struct ClusterPage {
    center: vec3<f32>,
    radius: f32,
    lod_bounds_center: vec3<f32>,
    lod_bounds_radius: f32,
    parent_bounds_center: vec3<f32>,
    parent_bounds_radius: f32,
    lod_error: f32,
    parent_error: f32,
    first_index: u32,
    index_count: u32,
};

struct ClusterCutParams {
    // Object→world transform of this instance (column-major, as WGSL stores it).
    instance_world: mat4x4<f32>,
    camera_pos: vec3<f32>,
    tan_half_fov_y: f32,
    // pixels = error * world_scale * (viewport_h/2) / (dist * tan_half_fov_y)
    viewport_h: f32,
    pixel_budget: f32,
    world_scale: f32,
    // Bound the workgroup-rounded dispatch to the real page count (arrayLength
    // returns the buffer capacity, not this mesh's cluster count).
    cluster_count: u32,
};

@group(0) @binding(0) var<storage, read> pages: array<ClusterPage>;
@group(0) @binding(1) var<storage, read_write> selected: array<u32>;
@group(0) @binding(2) var<uniform> params: ClusterCutParams;

// Project an object-space error at `world_center` to screen pixels. Returns a
// huge value for a degenerate distance/FOV (matches the CPU `+inf`), so a
// cluster at the camera is never culled by its own bound.
fn projected_error(error: f32, world_center: vec3<f32>) -> f32 {
    let dist = length(world_center - params.camera_pos);
    if (dist <= 1e-6 || params.tan_half_fov_y <= 1e-6) {
        return 3.4e38; // ~f32::MAX, stands in for +inf
    }
    return error * params.world_scale * (params.viewport_h * 0.5)
        / (dist * params.tan_half_fov_y);
}

fn to_world(p: vec3<f32>) -> vec3<f32> {
    return (params.instance_world * vec4<f32>(p, 1.0)).xyz;
}

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.cluster_count) {
        return;
    }
    let page = pages[i];

    let proj_lod = projected_error(page.lod_error, to_world(page.lod_bounds_center));
    // `parent_error` carries the root sentinel (f32::INFINITY on the CPU); in the
    // uploaded bytes that is +inf, so `proj_parent` is +inf for roots and the
    // upper bound always holds.
    let proj_parent = projected_error(page.parent_error, to_world(page.parent_bounds_center));

    if (proj_lod <= params.pixel_budget && params.pixel_budget < proj_parent) {
        selected[i] = 1u;
    } else {
        selected[i] = 0u;
    }
}
