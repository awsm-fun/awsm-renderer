fn vertex_color(attribute_data_offset: u32, triangle_indices: vec3<u32>, barycentric: vec3<f32>, color_info: VertexColorInfo, vertex_attribute_stride: u32, color_sets_index: u32) -> vec4<f32> {
{% if prep_present %}
    // INTERIOR pixels (PRIMARY) read the prep-materialized vertex-color array — free
    // (prep computed it once per interior pixel; parity-exact: same barycentric +
    // fp32 interp + visibility sample 0). Clamp the set index to the cap. EDGE
    // samples DELIBERATELY recompute below (the edge arm already holds this sample's
    // triangle + barycentric, so the lerp is cheaper than a per-edge-sample buffer's
    // write+read+VRAM, and there's no bulky code to evict — unlike shadows). Same
    // call as world-position. See the PREP-VS-RECOMPUTE RULE in
    // material_prep/buffers.rs + docs/SHADER_GUIDELINES.md.
    if (g_prep_ctx.mode == PREP_MODE_PRIMARY) {
        return textureLoad(prep_vcolor, g_prep_ctx.coords, i32(min(color_info.set_index, {{ max_prep_color_sets }}u - 1u)), 0);
    }
{% endif %}{% if !prep_drops_recompute %}
    let color0 = _vertex_color_per_vertex(attribute_data_offset, color_info.set_index, triangle_indices.x, vertex_attribute_stride, color_sets_index);
    let color1 = _vertex_color_per_vertex(attribute_data_offset, color_info.set_index, triangle_indices.y, vertex_attribute_stride, color_sets_index);
    let color2 = _vertex_color_per_vertex(attribute_data_offset, color_info.set_index, triangle_indices.z, vertex_attribute_stride, color_sets_index);

    let interpolated_color = barycentric.x * color0 + barycentric.y * color1 + barycentric.z * color2;

    return interpolated_color;
{% else %}
    // no-MSAA+prep: the PRIMARY return above always fires; unreachable fallback.
    return vec4<f32>(0.0);
{% endif %}
}

{# `prep_drops_recompute` (no-MSAA+prep, no cs_edge) routes `vertex_color` to
   the prep array and never runs the recompute body. Under MSAA+prep the helper
   STAYS (cs_edge=RECOMPUTE inlines the recompute body, which calls it). The only
   other caller is the custom `material_vertex_color` accessor (emitted only for
   `base == Custom` + `inc.vertex_color`). Keep the helper when any still need it. #}
{% if !prep_drops_recompute || (base == ShadingBase::Custom && inc.vertex_color) %}
fn _vertex_color_per_vertex(attribute_data_offset: u32, set_index: u32, vertex_index: u32, vertex_attribute_stride: u32, color_sets_index: u32) -> vec4<f32> {
    // First get to the right vertex, THEN to the right color set within that vertex.
    let vertex_start = attribute_data_offset + (vertex_index * vertex_attribute_stride);
    // `color_sets_index` is the float offset to COLOR_0 within the per-vertex
    // block (from `material_mesh_meta`) — colours pack *after* UVs, not at 0.
    // Each additional color set contributes 4 more floats per vertex.
    let color_offset = color_sets_index + (set_index * 4u);
    let index = vertex_start + color_offset;
    // attribute_data lives in the merged geometry pool aliased
    // here by `visibility_data` (binding 5).
    let color = vec4<f32>(visibility_data[index], visibility_data[index + 1], visibility_data[index + 2], visibility_data[index + 3]);

    return color;
}
{% endif %}{# keep recompute helper #}
