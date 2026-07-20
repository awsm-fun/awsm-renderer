// ============================================================================
// SMAA pass 3 — neighborhood blending. Faithful port of the reference
// `SMAANeighborhoodBlendingPS` (Jimenez et al.).
// ============================================================================
//
// Consumes the blend-weights texture produced by the SMAA pre-pass
// (render_passes/smaa: reference edge detection → AreaTex/SearchTex weight
// calculation) and revectorizes each edge by blending along the dominant
// direction with the reference's dual-tap scheme.
//
// HDR-safe: color blending happens in COMPRESSED space (t = s/(1+s), matching
// the MSAA edge resolve and the pre-pass's edge detection), then expands —
// a linear-space blend of hot emissive against background saturates the
// tonemapper and erases the AA. The composite is bound unfilterable, so the
// fractional-offset fetches do a manual 2-tap lerp along the blend axis.

fn smaa_compress(c: vec3<f32>) -> vec3<f32> {
    return c / (vec3<f32>(1.0) + c);
}
fn smaa_expand(c: vec3<f32>) -> vec3<f32> {
    let t = clamp(c, vec3<f32>(0.0), vec3<f32>(0.9995));
    return t / (vec3<f32>(1.0) - t);
}

// Compressed-space composite fetch at a fractional pixel offset along ONE
// axis (manual bilinear — exactly what the reference's linear sampler does
// for its blending coordinates).
fn smaa_fetch_offset(coords: vec2<i32>, dims: vec2<i32>, offset: vec2<f32>) -> vec3<f32> {
    // Standard bilinear at pixel-space sample point (coords + 0.5 + offset),
    // correct in both offset directions.
    let base = vec2<f32>(coords) + offset;   // == sample point - 0.5
    let i0 = vec2<i32>(floor(base));
    let f = base - floor(base);
    let c00 = clamp(i0, vec2<i32>(0), dims - 1);
    let c10 = clamp(i0 + vec2<i32>(1, 0), vec2<i32>(0), dims - 1);
    let c01 = clamp(i0 + vec2<i32>(0, 1), vec2<i32>(0), dims - 1);
    let c11 = clamp(i0 + vec2<i32>(1, 1), vec2<i32>(0), dims - 1);
    let s00 = smaa_compress(textureLoad(composite_tex, c00, 0).rgb);
    let s10 = smaa_compress(textureLoad(composite_tex, c10, 0).rgb);
    let s01 = smaa_compress(textureLoad(composite_tex, c01, 0).rgb);
    let s11 = smaa_compress(textureLoad(composite_tex, c11, 0).rgb);
    return mix(mix(s00, s10, f.x), mix(s01, s11, f.x), f.y);
}

fn apply_smaa(color: vec4<f32>, coords: vec2<i32>) -> vec4<f32> {
    let dims_u = textureDimensions(composite_tex);
    let dims = vec2<i32>(i32(dims_u.x), i32(dims_u.y));

    // Reference gather:
    //   a.x = right-neighbor's .a (blend rightward across our east border)
    //   a.y = bottom-neighbor's .g (blend downward across our south border)
    //   a.w = own .r (blend upward), a.z = own .b (blend leftward)
    let cr = clamp(coords + vec2<i32>(1, 0), vec2<i32>(0), dims - 1);
    let cb = clamp(coords + vec2<i32>(0, 1), vec2<i32>(0), dims - 1);
    var a = vec4<f32>(0.0);
    a.x = textureLoad(smaa_weights_tex, cr, 0).a;
    a.y = textureLoad(smaa_weights_tex, cb, 0).g;
    let own = textureLoad(smaa_weights_tex, coords, 0);
    a.w = own.r;
    a.z = own.b;

    // Is there any blending weight with a value greater than 0.0?
    if (dot(a, vec4<f32>(1.0)) < 1e-5) {
        return color;
    }

    // Max of horizontal vs vertical weights decides the blend direction:
    let h = max(a.x, a.z) > max(a.y, a.w);
    var blending_offset: vec4<f32>;
    var blending_weight: vec2<f32>;
    if (h) {
        blending_offset = vec4<f32>(a.x, 0.0, a.z, 0.0);
        blending_weight = vec2<f32>(a.x, a.z);
    } else {
        blending_offset = vec4<f32>(0.0, a.y, 0.0, a.w);
        blending_weight = vec2<f32>(a.y, a.w);
    }
    blending_weight = blending_weight / dot(blending_weight, vec2<f32>(1.0));

    // Blend along the reconstructed edge, dual-tap:
    var c1: vec3<f32>;
    var c2: vec3<f32>;
    if (h) {
        c1 = smaa_fetch_offset(coords, dims, vec2<f32>(blending_offset.x, 0.0));
        c2 = smaa_fetch_offset(coords, dims, vec2<f32>(-blending_offset.z, 0.0));
    } else {
        c1 = smaa_fetch_offset(coords, dims, vec2<f32>(0.0, blending_offset.y));
        c2 = smaa_fetch_offset(coords, dims, vec2<f32>(0.0, -blending_offset.w));
    }
    var blended = blending_weight.x * c1 + blending_weight.y * c2;

    // Ridge guard — beyond the reference. Morphological AA treats a 1-pixel
    // bright line (distant neon tube) as two opposing edges and blends the
    // line's OWN pixels toward the background: cores dim unevenly and the
    // line goes lumpy/perforated. A line core is a local luma maximum across
    // the blend axis; real shape edges are not (their inside neighbor matches
    // them), so this only suppresses blending on isolated ridges. Neighbors
    // still blend, which is where the actual stairstep smoothing happens.
    let lum = vec3<f32>(0.2126, 0.7152, 0.0722);
    let axis = select(vec2<i32>(0, 1), vec2<i32>(1, 0), h);
    let n1 = textureLoad(composite_tex, clamp(coords + axis, vec2<i32>(0), dims - 1), 0).rgb;
    let n2 = textureLoad(composite_tex, clamp(coords - axis, vec2<i32>(0), dims - 1), 0).rgb;
    let center_c = smaa_compress(color.rgb);
    let ridge_amt = dot(center_c, lum)
        - max(dot(smaa_compress(n1), lum), dot(smaa_compress(n2), lum));
    let ridge = smoothstep(0.02, 0.10, ridge_amt);
    blended = mix(blended, center_c, ridge);

    return vec4<f32>(smaa_expand(blended), color.a);
}
