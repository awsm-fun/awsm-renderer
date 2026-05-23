{% include "shared_wgsl/math.wgsl" %}

// Fragment input from vertex shader
struct FragmentInput {
    @location(0) @interpolate(flat) triangle_index: u32,
    @location(1) barycentric: vec2<f32>,  // Full barycentric coordinates
    @location(2) world_normal: vec3<f32>,     // Transformed world-space normal
    @location(3) world_tangent: vec4<f32>,    // Transformed world-space tangent (w = handedness)
    @location(4) @interpolate(flat) instance_id: u32, // U32_MAX for non-instanced draws
    // Forwarded from vertex so the fragment doesn't have to
    // re-read `geometry_mesh_meta`. The
    // non-instanced storage-array path populates the vertex's
    // `var<private> geometry_mesh_meta` at vertex entry, which
    // doesn't propagate to fragment (private storage is
    // per-shader-stage). Without this flat varying the fragment
    // reads byte offset 0 for every pixel and routes every mesh's
    // shading through material slot 0.
    @location(5) @interpolate(flat) material_mesh_meta_offset: u32,
}

struct FragmentOutput {
    // RGBA16uint
    @location(0) visibility_data: vec4<u32>,    // triangle_index and material_offset (each as packed 32)
    // RGBA16uint
    // RG: barycentric.xy as u16 fixed-point (clamp(bary, 0, 1) * 65535).
    // BA: instance_id as packed u32 (split16 → B=lo, A=hi via join32 convention).
    @location(1) barycentric: vec4<u32>,
    // RGBA16float
    @location(2) normal_tangent: vec4<f32>,
    // RGBA16float
    @location(3) barycentric_derivatives: vec4<f32>,
}

@fragment
fn fs_main(input: FragmentInput) -> FragmentOutput {
    var out: FragmentOutput;

    // Pack visibility buffer data
    let t = split16(input.triangle_index);
    // This is NOT the material's `material_offset` — it's the byte
    // offset of the per-mesh entry in the `material_mesh_meta` storage
    // buffer (which in turn contains the material_offset). Sourced
    // from the flat varying so non-instanced meshes route through the
    // right slot even though the fragment stage's
    // `var<private> geometry_mesh_meta` is uninitialised.
    let m = split16(input.material_mesh_meta_offset);
    // it's 16 bits, not u32, but we store as u32 for simplicity
    out.visibility_data = vec4<u32>(
        t.x,t.y,
        m.x,m.y
    );

    // z = 1.0 - x - y. Pack as u16 fixed-point so we can use the BA channels
    // for instance_id (kept lossless via `join32`).
    let bary = clamp(input.barycentric, vec2<f32>(0.0), vec2<f32>(1.0));
    let bary_x_u16 = u32(bary.x * 65535.0 + 0.5);
    let bary_y_u16 = u32(bary.y * 65535.0 + 0.5);
    let iid = split16(input.instance_id);
    out.barycentric = vec4<u32>(bary_x_u16, bary_y_u16, iid.x, iid.y);

    // Pack normal and tangent into a single vec4 (RGBA16Float)
    // octahedral normal (2 channels) + tangent angle (1 channel) + handedness sign (1 channel)
    let N = normalize(input.world_normal);
    let T = normalize(input.world_tangent.xyz);
    let s = input.world_tangent.w; // handedness: +1 or -1
    out.normal_tangent = pack_normal_tangent(N, T, s);

    // perspective-correct barycentrics by default:
    let ddx = dpdx(input.barycentric);          // (db1/dx, db2/dx)
    let ddy = dpdy(input.barycentric);          // (db1/dy, db2/dy)

    out.barycentric_derivatives = vec4<f32>(ddx.x, ddy.x, ddx.y, ddy.y);

    return out;
}
