{% include "shared_wgsl/math.wgsl" %}

// Per-pixel mesh-coverage tally.
//
// One thread per pixel. For each pixel that holds a real material
// (skybox / cleared pixels are skipped), extract the
// `material_mesh_meta_offset` from `visibility_data.zw` (`join32`
// recovers the per-fragment u32), divide by the meta stride to get
// the mesh slot, and atomicAdd 1 into `mesh_pixel_counts[slot]`.
//
// The slot indexing matches the per-mesh drawIndirect args buffer —
// `mesh_meta_offset / 256 = slot`. The CPU reads the
// counts back next frame and routes them through
// `MeshCoverage::ingest` so downstream consumers (skinning skip,
// material LOD) can branch on last-frame visibility.
//
// MSAA path samples index 0 only — a "did this mesh contribute any
// pixels" signal doesn't need per-sample tallies, and sampling all
// 4 would quadruple atomic traffic for sub-pixel resolution that
// the consumers can't act on.

{% if multisampled %}
@group(0) @binding(0) var visibility_data: texture_multisampled_2d<u32>;
{% else %}
@group(0) @binding(0) var visibility_data: texture_2d<u32>;
{% endif %}
@group(0) @binding(1) var<storage, read_write> mesh_pixel_counts: array<atomic<u32>>;

// Must match `MaterialMeshMeta` / `GeometryMeshMeta` slot alignment.
// The meta_offset field in visibility_data is a byte offset into the
// material meta buffer; both metas use the same 256 B stride so the
// resulting slot index is shared with the drawIndirect args buffer.
const MESH_META_STRIDE_BYTES: u32 = 256u;

@compute @workgroup_size(8, 8)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(visibility_data);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    {% if multisampled %}
    let vis = textureLoad(visibility_data, vec2<i32>(i32(gid.x), i32(gid.y)), 0);
    {% else %}
    let vis = textureLoad(visibility_data, vec2<i32>(i32(gid.x), i32(gid.y)), 0);
    {% endif %}
    let tri = join32(vis.x, vis.y);
    if (tri == U32_MAX) {
        return;
    }
    let meta_offset = join32(vis.z, vis.w);
    let slot = meta_offset / MESH_META_STRIDE_BYTES;
    let cap = arrayLength(&mesh_pixel_counts);
    if (slot >= cap) {
        return;
    }
    atomicAdd(&mesh_pixel_counts[slot], 1u);
}
