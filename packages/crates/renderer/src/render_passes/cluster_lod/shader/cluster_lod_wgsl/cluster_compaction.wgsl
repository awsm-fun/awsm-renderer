// Cluster-LOD compaction (Phase B, B.2).
//
// Consumes the cut's `selected[]` and packs the chosen clusters' index pages
// into one contiguous `compacted_indices` stream, bumping the
// `drawIndexedIndirect` `index_count`. One workgroup_size(64) thread per cluster
// page:
//   if selected[i] == 1:
//     base = atomicAdd(&draw_args.index_count, page.index_count)   // reserve
//     copy source_indices[page.first_index .. +index_count] → compacted_indices[base ..]
//
// The host clears `draw_args` to {index_count:0, instance_count:1, 0,0,0} before
// the dispatch, so after it the args drive a single
// `drawIndexedIndirect(compacted_indices)` of exactly the cut's triangles. Order
// within the stream is nondeterministic (atomic race) but irrelevant — a triangle
// soup.

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

// 5×u32 drawIndexedIndirect args; index_count is atomic (the append cursor).
struct DrawArgs {
    index_count: atomic<u32>,
    instance_count: u32,
    first_index: u32,
    base_vertex: u32,
    first_instance: u32,
};

// Reuses the cut's 96-B ClusterCutParams uniform; only `cluster_count` is read.
struct ClusterCutParams {
    instance_world: mat4x4<f32>,
    camera_pos: vec3<f32>,
    tan_half_fov_y: f32,
    viewport_h: f32,
    pixel_budget: f32,
    world_scale: f32,
    cluster_count: u32,
};

@group(0) @binding(0) var<storage, read> pages: array<ClusterPage>;
@group(0) @binding(1) var<storage, read> selected: array<u32>;
@group(0) @binding(2) var<storage, read> source_indices: array<u32>;
@group(0) @binding(3) var<storage, read_write> compacted_indices: array<u32>;
@group(0) @binding(4) var<storage, read_write> draw_args: DrawArgs;
@group(0) @binding(5) var<uniform> params: ClusterCutParams;

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.cluster_count) {
        return;
    }
    if (selected[i] == 0u) {
        return;
    }
    let page = pages[i];
    let n = page.index_count;
    let base = atomicAdd(&draw_args.index_count, n);
    for (var k = 0u; k < n; k = k + 1u) {
        compacted_indices[base + k] = source_indices[page.first_index + k];
    }
}
