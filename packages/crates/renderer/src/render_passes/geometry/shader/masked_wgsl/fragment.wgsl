// Shared masking-alpha helpers (material loads, UV reconstruction, pool sample,
// custom alpha-only wrapper, and `mask_alpha_at`). Also used by the masked
// shadow fragment so the cutout test is byte-identical across both passes.
{% include "shared_wgsl/masked_alpha.wgsl" %}

// Barycentric at a sub-pixel offset (pixel-space, center origin), via the
// screen-space derivatives of the interpolated barycentric.
fn mask_bary_at(b: vec2<f32>, dbx: vec2<f32>, dby: vec2<f32>, ox: f32, oy: f32) -> vec3<f32> {
    let p = b + dbx * ox + dby * oy;
    return vec3<f32>(p.x, p.y, 1.0 - p.x - p.y);
}

// ── Fragment I/O (identical to the plain geometry pass — masked meshes write
// the SAME visibility buffer; we only add the cutoff discard up front). ──────
struct FragmentInput {
    @location(0) @interpolate(flat) triangle_index: u32,
    // Centroid → texel-UV reconstruction (on-surface, clamp-safe at seams). See vertex.wgsl.
    @location(1) @interpolate(perspective, centroid) barycentric: vec2<f32>,
    // Center → source for ALL dpdx/dpdy here: texture-LOD gradients AND the
    // per-sample cutout sub-offsets below. Keeps cutout coverage + LOD identical
    // to the pre-centroid behaviour (derivatives of a centroid varying are undefined).
    @location(6) @interpolate(perspective, center) barycentric_center: vec2<f32>,
    // Centroid-sampled to match the (shared) geometry vertex output qualifier —
    // the masked variant reuses geometry_wgsl/vertex.wgsl, so a mismatch here is
    // a pipeline-creation validation error. See that vertex struct for the why
    // (silhouette normals stay on-surface under MSAA).
    @location(2) @interpolate(perspective, centroid) world_normal: vec3<f32>,
    @location(3) @interpolate(perspective, centroid) world_tangent: vec4<f32>,
    @location(4) @interpolate(flat) instance_id: u32,
    @location(5) @interpolate(flat) material_mesh_meta_offset: u32,
    // See the plain geometry fragment: flip the normal for back faces so
    // double-sided masked meshes (e.g. foliage) shade two-sided correctly —
    // including diffuse-transmission undersides.
    @builtin(front_facing) front_facing: bool,
}

struct FragmentOutput {
    @location(0) visibility_data_out: vec4<u32>,
    @location(1) barycentric: vec4<u32>,
    @location(2) normal_tangent: vec4<f32>,
    @location(3) barycentric_derivatives: vec4<f32>,
    {% if msaa_sample_count > 1 %}
    // Per-sample cutout coverage. Fractional at the cutout boundary so the
    // covered samples write the surface while the rest stay the cleared skybox
    // sentinel — the MSAA edge-resolve then blends them (anti-aliased cutout).
    @builtin(sample_mask) coverage_mask: u32,
    {% endif %}
}

@fragment
fn fs_main(input: FragmentInput) -> FragmentOutput {
    let mm = material_mesh_metas[input.material_mesh_meta_offset / META_SIZE_IN_BYTES];
    let material_offset = mm.material_offset;
    let vertex_attribute_stride = mm.vertex_attribute_stride / 4u;
    let attribute_indices_offset = mm.vertex_attribute_indices_offset / 4u;
    let attribute_data_offset = mm.vertex_attribute_data_offset / 4u;
    let uv_sets_index = mm.uv_sets_index;
    let color_sets_index = mm.color_sets_index;

    let base_triangle_index = attribute_indices_offset + (input.triangle_index * 3u);
    let triangle_indices = vec3<u32>(
        bitcast<u32>(visibility_data[base_triangle_index]),
        bitcast<u32>(visibility_data[base_triangle_index + 1u]),
        bitcast<u32>(visibility_data[base_triangle_index + 2u]),
    );
    // ── Cutout: keep/discard the pixel, and (under MSAA) per-sample coverage ──
    {% if msaa_sample_count > 1 %}
    // A single center alpha test is enough to KEEP/DROP the pixel — but it's
    // all-or-nothing, so the cutout edge aliases. To ANTI-ALIAS it we need the
    // sub-pixel coverage: evaluate the masking alpha at each of the 4 MSAA
    // sample sub-positions (offset from pixel center via the barycentric
    // screen-space derivatives) and set that sample's coverage bit when it
    // passes the cutoff. The covered samples write the surface; the rest stay
    // the cleared background, and the existing MSAA edge-resolve blends them.
    // True per-sample sampling works for ANY alpha — smooth (texture) OR binary
    // (procedural `select`), where an fwidth-of-alpha gradient carries nothing.
    // Standard 4x sample offsets (the exact positions only nudge spatial
    // placement; the resolve blends by covered-sample COUNT, all carrying the
    // shared center data).
    let dbx = dpdx(input.barycentric_center);
    let dby = dpdy(input.barycentric_center);
    let cut = mm.alpha_cutoff;
    var coverage_mask = 0u;
    if mask_alpha_at(mask_bary_at(input.barycentric_center, dbx, dby, -0.125, -0.375), triangle_indices, attribute_data_offset, vertex_attribute_stride, uv_sets_index, color_sets_index, material_offset) >= cut { coverage_mask |= 1u; }
    if mask_alpha_at(mask_bary_at(input.barycentric_center, dbx, dby,  0.375, -0.125), triangle_indices, attribute_data_offset, vertex_attribute_stride, uv_sets_index, color_sets_index, material_offset) >= cut { coverage_mask |= 2u; }
    if mask_alpha_at(mask_bary_at(input.barycentric_center, dbx, dby, -0.375,  0.125), triangle_indices, attribute_data_offset, vertex_attribute_stride, uv_sets_index, color_sets_index, material_offset) >= cut { coverage_mask |= 4u; }
    if mask_alpha_at(mask_bary_at(input.barycentric_center, dbx, dby,  0.125,  0.375), triangle_indices, attribute_data_offset, vertex_attribute_stride, uv_sets_index, color_sets_index, material_offset) >= cut { coverage_mask |= 8u; }
    if coverage_mask == 0u {
        discard;
    }
    {% else %}
    let bary = vec3<f32>(input.barycentric.x, input.barycentric.y, 1.0 - input.barycentric.x - input.barycentric.y);
    let alpha = mask_alpha_at(bary, triangle_indices, attribute_data_offset, vertex_attribute_stride, uv_sets_index, color_sets_index, material_offset);
    if alpha < mm.alpha_cutoff {
        discard;
    }
    {% endif %}

    // ── Write the visibility buffer exactly like the plain geometry pass ──
    var out: FragmentOutput;
    {% if msaa_sample_count > 1 %}
    out.coverage_mask = coverage_mask;
    {% endif %}
    let t = split16(input.triangle_index);
    let m = split16(input.material_mesh_meta_offset);
    out.visibility_data_out = vec4<u32>(t.x, t.y, m.x, m.y);

    let bary_xy = clamp(input.barycentric, vec2<f32>(0.0), vec2<f32>(1.0));
    let bary_x_u16 = u32(bary_xy.x * 65535.0 + 0.5);
    let bary_y_u16 = u32(bary_xy.y * 65535.0 + 0.5);
    let iid = split16(input.instance_id);
    out.barycentric = vec4<u32>(bary_x_u16, bary_y_u16, iid.x, iid.y);

    var N = normalize(input.world_normal);
    let T = normalize(input.world_tangent.xyz);
    let s = input.world_tangent.w;
    if !input.front_facing {
        N = -N;
    }
    out.normal_tangent = pack_normal_tangent(N, T, s);

    // fp16-overflow clamp — see the plain geometry fragment for the rationale
    // (sub-pixel skinny triangles overflow the Rgba16float target → Inf →
    // zeroed gradients → LOD-0 sparkle).
    const BARY_DERIV_LIMIT: f32 = 6.0e4;
    let ddx = clamp(dpdx(input.barycentric_center),
        vec2<f32>(-BARY_DERIV_LIMIT), vec2<f32>(BARY_DERIV_LIMIT));
    let ddy = clamp(dpdy(input.barycentric_center),
        vec2<f32>(-BARY_DERIV_LIMIT), vec2<f32>(BARY_DERIV_LIMIT));
    out.barycentric_derivatives = vec4<f32>(ddx.x, ddy.x, ddx.y, ddy.y);

    return out;
}
