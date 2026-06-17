// Material prep compute pass (Plan B). Runs once per pixel over the visibility
// buffer, after classify and before per-material shading, materializing the
// material-INDEPENDENT geometry-pool attributes (UV0 + vertex color) so the slim
// per-material kernel reads them instead of recomputing. World position is NOT
// written here (the slim shader keeps the cheap depth-unprojection); shadow
// visibility (stage 3) + the compact edge buffer (stage 5) land later.
//
// `join32` / `U32_MAX` come from math.wgsl; `MaterialMeshMeta` /
// `META_SIZE_IN_BYTES` come from material_mesh_meta.wgsl (included by
// bind_groups.wgsl, concatenated before this).
{% include "shared_wgsl/math.wgsl" %}

// One vertex's UV set 0, read from the geometry pool. Mirrors
// texture_uvs.wgsl::_texture_uv_per_vertex (set_index 0). TODO(parity): factor
// the per-vertex attr fetch into a shared include consumed by both kernels.
fn prep_uv0_at(data_off: u32, vert: u32, stride: u32, uv_sets_index: u32) -> vec2<f32> {
    let o = data_off + vert * stride + uv_sets_index;
    return vec2<f32>(visibility_data[o], visibility_data[o + 1u]);
}

// One vertex's COLOR set 0. Mirrors vertex_color_attrib.wgsl::_vertex_color_per_vertex.
fn prep_vcolor0_at(data_off: u32, vert: u32, stride: u32, color_sets_index: u32) -> vec4<f32> {
    let o = data_off + vert * stride + color_sets_index;
    return vec4<f32>(visibility_data[o], visibility_data[o + 1u], visibility_data[o + 2u], visibility_data[o + 3u]);
}

@compute @workgroup_size(8, 8)
fn cs_prep(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(uv_out);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    let coords = vec2<i32>(i32(gid.x), i32(gid.y));

    // Visibility: triangle id + per-mesh meta offset (split u32 via join32).
    let vis = textureLoad(visibility_data_tex, coords, 0);
    let triangle_index = join32(vis.x, vis.y);
    let material_meta_offset = join32(vis.z, vis.w);
    if (triangle_index == U32_MAX) {
        // Sky / no geometry — leave attrs cleared.
        textureStore(uv_out, coords, vec4<f32>(0.0));
        textureStore(vcolor_out, coords, vec4<f32>(0.0));
        return;
    }

    let mesh_meta = material_mesh_metas[material_meta_offset / META_SIZE_IN_BYTES];
    let stride = mesh_meta.vertex_attribute_stride / 4u;
    let idx_off = mesh_meta.vertex_attribute_indices_offset / 4u;
    let data_off = mesh_meta.vertex_attribute_data_offset / 4u;
    let uv_sets_index = mesh_meta.uv_sets_index;
    let color_sets_index = mesh_meta.color_sets_index;

    // Triangle vertex indices (bitcast f32 words → u32).
    let base_tri = idx_off + triangle_index * 3u;
    let ti = vec3<u32>(
        bitcast<u32>(visibility_data[base_tri]),
        bitcast<u32>(visibility_data[base_tri + 1u]),
        bitcast<u32>(visibility_data[base_tri + 2u]),
    );

    // Barycentric weights (same unpack as cs_opaque).
    let bary_raw = textureLoad(barycentric_tex, coords, 0);
    let bary_xy = vec2<f32>(f32(bary_raw.x), f32(bary_raw.y)) / 65535.0;
    let bary = vec3<f32>(bary_xy.x, bary_xy.y, 1.0 - bary_xy.x - bary_xy.y);

    // UV0 (stage 1a: set 0; multi-set materialization is a follow-up).
    let uv0 = prep_uv0_at(data_off, ti.x, stride, uv_sets_index);
    let uv1 = prep_uv0_at(data_off, ti.y, stride, uv_sets_index);
    let uv2 = prep_uv0_at(data_off, ti.z, stride, uv_sets_index);
    let uv = bary.x * uv0 + bary.y * uv1 + bary.z * uv2;
    textureStore(uv_out, coords, vec4<f32>(uv, 0.0, 0.0));

    // Vertex color set 0.
    let c0 = prep_vcolor0_at(data_off, ti.x, stride, color_sets_index);
    let c1 = prep_vcolor0_at(data_off, ti.y, stride, color_sets_index);
    let c2 = prep_vcolor0_at(data_off, ti.z, stride, color_sets_index);
    let vc = bary.x * c0 + bary.y * c1 + bary.z * c2;
    textureStore(vcolor_out, coords, vc);
}
