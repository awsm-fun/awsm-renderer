// Custom vertex-displacement hook (gated by `has_custom_vertex`). Mirrors the
// fragment `custom_shade_dynamic` machinery: the agent's WGSL body is wrapped
// into `custom_displace_vertex`, which runs in the post-morph LOCAL frame
// (before skin) so skinned + rigid meshes deform consistently. The hook owns
// the returned surface frame (normal/tangent) — see
// docs/dynamic-materials/contract-vertex.md (normal ownership).
struct VertexDisplaceInput {
    position: vec3<f32>,    // post-morph LOCAL position
    normal: vec3<f32>,      // post-morph LOCAL normal
    tangent: vec4<f32>,     // LOCAL tangent (w = handedness)
    // ALL of the mesh's UV sets, read per-vertex (parity with the fragment
    // hook's multi-UV access). Unused sets are (0,0). The number of valid sets
    // is `uv_count` (<= 4); index it as `input.uv[set]` — e.g. `input.uv[0]`
    // is the classic TEXCOORD_0. Pair with `material_sample_<name>(input.material, input.uv[i])`
    // to sample a declared texture in the vertex stage.
    uv: array<vec2<f32>, 4>,
    uv_count: u32,          // number of valid UV sets in `uv` (0..=4)
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

// Recompute a surface normal from three height samples (central position +
// neighbours one step `eps` along the tangent (du) and bitangent (dv)). Useful
// for heightmap / displacement-driven materials: displace `position` along `n`
// by a height read from a texture, then call this to derive the matching normal
// so lighting follows the bumps. `n` / `t` are the (LOCAL) surface frame;
// `t.w` is the handedness used to build the bitangent. `strength` scales the
// perturbation (0 = unchanged normal). Returns a normalized vector.
fn recompute_normal_from_height(n: vec3<f32>, t: vec4<f32>, h_center: f32, h_du: f32, h_dv: f32, eps: f32, strength: f32) -> vec3<f32> {
    let bitangent = cross(n, t.xyz) * t.w;
    let ddu = (h_du - h_center) / eps;
    let ddv = (h_dv - h_center) / eps;
    return normalize(n - (t.xyz * ddu + bitangent * ddv) * strength);
}

fn custom_displace_vertex(input: VertexDisplaceInput) -> VertexDisplaceOutput {
{{ dynamic_wgsl_vertex|safe }}
}
