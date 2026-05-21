// GPU instance compaction.
//
// One thread per occlusion instance. For each instance, if the
// cull's `visible_this_frame[i]` is 1, atomicAdd 1 to the matching
// per-mesh `IndirectDrawArgs.instance_count`. The per-mesh slot
// index comes from `instances[i].mesh_meta_offset / META_SIZE` —
// matching MaterialMeshMeta's per-mesh stride.

struct OcclusionInstance {
    world_aabb_min: vec3<f32>,
    _pad0: u32,
    world_aabb_max: vec3<f32>,
    _pad1: u32,
    mesh_meta_offset: u32,
    instance_attr_base: u32,
    // See cull.wgsl — repurposed slot, written into the
    // IndirectDrawArgs by this shader so the CPU is no longer
    // a writer of `args_buffer`.
    index_count: u32,
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

struct OcclusionParams {
    active_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<storage, read> instances: array<OcclusionInstance>;
@group(0) @binding(1) var<storage, read> visible_this_frame: array<u32>;
@group(0) @binding(2) var<storage, read_write> indirect_args: array<IndirectDrawArgs>;
@group(0) @binding(3) var<uniform> params: OcclusionParams;

// Must match `MATERIAL_MESH_META_BYTE_ALIGNMENT` (256 B). The cull
// stages a mesh_meta_offset in bytes; we divide to get the per-mesh
// slot index.
const MESH_META_STRIDE_BYTES: u32 = 256u;

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    // Bound by the active instance count, not `arrayLength` (which
    // returns capacity). Tail threads in the workgroup-rounded
    // dispatch would otherwise read `visible_this_frame[i]` from
    // slots that the cull's matching `if (i >= count) return` left
    // untouched — i.e. last frame's value — and double-count phantom
    // mesh instances. See cull.wgsl for the matched comment.
    let count = params.active_count;
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
    // Write the static drawIndirect fields here rather than from the
    // CPU. `queue.writeBuffer` would have overwritten them BEFORE the
    // already-recorded geometry pass executed (queue order ≠ command
    // order), so a CPU-side prep zeroed `instance_count` ahead of the
    // earlier-recorded `draw_indexed_indirect` consumer. Doing the
    // writes here keeps args_buffer GPU-owned: every update happens
    // in command order strictly after the geometry pass's read.
    //
    // first_index / base_vertex stay at zero — the args_buffer was
    // cleared by `command_encoder.clear_buffer` between geometry and
    // cull (see render.rs), so we don't need to re-emit them. For
    // non-instanced meshes (the only path through drawIndirect),
    // each mesh_slot is touched by at most one thread, so the
    // non-atomic write of `index_count` and `first_instance` has no
    // races; instance_count is still atomicAdded since the cull may
    // mark multiple instances of one mesh visible under future
    // instancing extensions.
    indirect_args[mesh_slot].index_count = instances[i].index_count;
    indirect_args[mesh_slot].first_instance = mesh_slot;
    atomicAdd(&indirect_args[mesh_slot].instance_count, 1u);
}
