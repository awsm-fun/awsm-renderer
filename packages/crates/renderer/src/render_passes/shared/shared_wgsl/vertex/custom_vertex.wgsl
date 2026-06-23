// Custom vertex-displacement hook (gated by `has_custom_vertex`). Mirrors the
// fragment `custom_shade_dynamic` machinery: the agent's WGSL body is wrapped
// into `custom_displace_vertex`, which runs in the post-morph LOCAL frame
// (before skin) so skinned + rigid meshes deform consistently. The hook owns
// the returned surface frame (normal/tangent) — see docs/plans/todo.md §6.
struct VertexDisplaceInput {
    position: vec3<f32>,    // post-morph LOCAL position
    normal: vec3<f32>,      // post-morph LOCAL normal
    tangent: vec4<f32>,     // LOCAL tangent (w = handedness)
    uv: vec2<f32>,          // uv0 (per-mesh UV buffer; (0,0) if the mesh has none)
    vertex_index: u32,
    instance_id: u32,       // INSTANCE_ATTR_NONE (u32::MAX) when non-instanced
    material: MaterialData, // the SAME auto-generated struct as the fragment hook
    globals: FrameGlobals,  // time + camera, for animated displacement
};
struct VertexDisplaceOutput {
    position: vec3<f32>,
    normal: vec3<f32>,
    tangent: vec4<f32>,
};
fn custom_displace_vertex(input: VertexDisplaceInput) -> VertexDisplaceOutput {
{{ dynamic_wgsl_vertex|safe }}
}
