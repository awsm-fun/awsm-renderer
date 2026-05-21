// GPU instance compaction — §16.7 Phase 2 / §16.8 infrastructure.
//
// One thread per occlusion instance. For each instance, if the
// cull's `visible_this_frame[i]` is 1, atomicAdd 1 to the matching
// per-mesh `IndirectDrawArgs.instance_count`. The per-mesh slot
// index comes from `instances[i].mesh_meta_offset / META_SIZE` —
// matching MaterialMeshMeta's per-mesh stride.
//
// v1 (this Phase 2 + §16.8 *infrastructure* landing): no consumer
// yet. The geometry pass still records per-mesh `draw_indexed`
// calls. The compaction's args buffer is correctly populated and
// observable; a future session swaps the geometry draw loop to
// `drawIndirect` against it once the per-mesh-meta lookup migrates
// from dynamic-offset uniform to a storage-array indexed by
// `@builtin(instance_index)`.

struct OcclusionInstance {
    world_aabb_min: vec3<f32>,
    _pad0: u32,
    world_aabb_max: vec3<f32>,
    _pad1: u32,
    mesh_meta_offset: u32,
    instance_attr_base: u32,
    last_frame_visible: u32,
    _pad2: u32,
};

// IndirectDrawArgs slot (32 B): the leading 5 u32s are the WebGPU
// `drawIndexedIndirect` layout — `(index_count, instance_count,
// first_index, base_vertex, first_instance)`. The trailing 3 u32s
// are padding for nice alignment.
struct IndirectDrawArgs {
    index_count: u32,
    instance_count: atomic<u32>,
    first_index: u32,
    base_vertex: u32,
    first_instance: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<storage, read> instances: array<OcclusionInstance>;
@group(0) @binding(1) var<storage, read> visible_this_frame: array<u32>;
@group(0) @binding(2) var<storage, read_write> indirect_args: array<IndirectDrawArgs>;

// Must match `MATERIAL_MESH_META_BYTE_ALIGNMENT` (256 B). The cull
// stages a mesh_meta_offset in bytes; we divide to get the per-mesh
// slot index.
const MESH_META_STRIDE_BYTES: u32 = 256u;

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    let count = arrayLength(&instances);
    if (i >= count) {
        return;
    }
    let visible = visible_this_frame[i];
    if (visible == 0u) {
        return;
    }
    let mesh_slot = instances[i].mesh_meta_offset / MESH_META_STRIDE_BYTES;
    let args_capacity = arrayLength(&indirect_args);
    if (mesh_slot >= args_capacity) {
        return;
    }
    atomicAdd(&indirect_args[mesh_slot].instance_count, 1u);
}
