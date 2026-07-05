{% match mipmap %}
    {% when MipmapMode::Gradient %}
        struct TextureTransformUvs {
            uv: vec2<f32>,
            derivs: UvDerivs,
        }

        fn apply_texture_transform(
            uv: vec2<f32>,
            derivs: UvDerivs,
            tex_info: TextureInfo
        ) -> TextureTransformUvs {
            // CPU assigns index to identity if needed, no special branch required.
            let t = texture_transforms[tex_info.uv_transform_index];

            let m00 = t.m.x;
            let m01 = t.m.y;
            let m10 = t.m.z;
            let m11 = t.m.w;
            let B   = t.b;

            let uv_transformed = vec2<f32>(
                m00 * uv.x + m01 * uv.y,
                m10 * uv.x + m11 * uv.y
            ) + B;

            let ddx_transformed = vec2<f32>(
                m00 * derivs.ddx.x + m01 * derivs.ddx.y,
                m10 * derivs.ddx.x + m11 * derivs.ddx.y
            );

            let ddy_transformed = vec2<f32>(
                m00 * derivs.ddy.x + m01 * derivs.ddy.y,
                m10 * derivs.ddy.x + m11 * derivs.ddy.y
            );

            let derivs_transformed = UvDerivs(ddx_transformed, ddy_transformed);

            return TextureTransformUvs(
                uv_transformed,
                derivs_transformed,
            );
        }

    {% when MipmapMode::None %}
        struct TextureTransformUvs {
            uv: vec2<f32>,
        }

        fn apply_texture_transform(
            uv: vec2<f32>,
            tex_info: TextureInfo
        ) -> TextureTransformUvs {
            let uv_transformed = texture_transform_uvs(uv, tex_info);

            return TextureTransformUvs(
                uv_transformed,
            );
        }

{% endmatch %}


fn texture_uv(attribute_data_offset: u32, triangle_indices: vec3<u32>, barycentric: vec3<f32>, tex_info: TextureInfo, vertex_attribute_stride: u32, uv_sets_index: u32) -> vec2<f32> {
{% if prep_present %}
    // Accessor branches on the per-thread PrepReadContext mode. INTERIOR pixels
    // (PRIMARY) read the prep-materialized UV array — free, since prep computed it
    // once for every interior pixel (parity-exact: same barycentric + fp32 interp +
    // visibility sample 0). Clamp the set index to the cap; sets beyond it are rare.
    //
    // EDGE samples fall through to the geometry-pool recompute below — and that is a
    // DELIBERATE decision, not a missing optimization. The edge arm already holds
    // this sample's triangle + barycentric in-register, so the lerp here is a few
    // reads; giving edges a prep buffer would mean recomputing the same thing in
    // cs_prep_edge, writing it, reading it back, plus ~tens of MB of VRAM, to evict
    // ~10 lines of code. Same call as world-position (also recomputed, never
    // prepped). Shadows DO get an edge buffer because that evicts ~50 KB. See the
    // PREP-VS-RECOMPUTE RULE in material_prep/buffers.rs + docs/SHADER_GUIDELINES.md.
    if (g_prep_ctx.mode == PREP_MODE_PRIMARY) {
        return textureLoad(prep_uv, g_prep_ctx.coords, i32(min(tex_info.uv_set_index, {{ max_prep_uv_sets }}u - 1u)), 0).xy;
    }
{% endif %}{% if !prep_drops_recompute %}
    let uv0 = _texture_uv_per_vertex(attribute_data_offset, tex_info.uv_set_index, triangle_indices.x, vertex_attribute_stride, uv_sets_index);
    let uv1 = _texture_uv_per_vertex(attribute_data_offset, tex_info.uv_set_index, triangle_indices.y, vertex_attribute_stride, uv_sets_index);
    let uv2 = _texture_uv_per_vertex(attribute_data_offset, tex_info.uv_set_index, triangle_indices.z, vertex_attribute_stride, uv_sets_index);

    let interpolated_uv = barycentric.x * uv0 + barycentric.y * uv1 + barycentric.z * uv2;

    return interpolated_uv;
{% else %}
    // no-MSAA+prep: the PRIMARY return above always fires; this is unreachable
    // but WGSL requires a return on all paths.
    return vec2<f32>(0.0);
{% endif %}
}

{# Drop the geometry-pool recompute helper only when nothing references it:
   `prep_drops_recompute` (no-MSAA+prep) routes `texture_uv` to the prep array
   and has no cs_edge, so the recompute body never runs. Under MSAA+prep the
   helper STAYS (cs_edge=RECOMPUTE inlines the recompute body, which calls it).
   The gradient mipmap path (`get_uv_derivatives`) still needs raw per-vertex
   UVs (UV gradients are recomputed, never materialized — Plan B decision #2),
   and the custom `material_uv` accessor (emitted only for `base == Custom` +
   `inc.textures`) calls it directly. Keep it emitted in any of those cases. #}
{% if !prep_drops_recompute || mipmap.is_gradient() || (base == ShadingBase::Custom && inc.textures) %}
fn _texture_uv_per_vertex(attribute_data_offset: u32, set_index: u32, vertex_index: u32, vertex_attribute_stride: u32, uv_sets_index: u32) -> vec2<f32> {
    // First get to the right vertex, THEN to the right UV set within that vertex
    let vertex_start = attribute_data_offset + (vertex_index * vertex_attribute_stride);
    // `uv_sets_index` points to the beginning of TEXCOORD_0 inside the packed stream.
    // Each additional UV set contributes two more floats per vertex.
    let uv_offset = uv_sets_index + (set_index * 2u);
    let index = vertex_start + uv_offset;
    // attribute_data lives in the merged geometry pool aliased
    // here by `visibility_data` (binding 5).
    let uv = vec2<f32>(visibility_data[index], visibility_data[index + 1]);

    return uv;
}
{% endif %}


{% match mipmap %}
    {% when MipmapMode::Gradient %}
        // Sampling with explicit gradients for anisotropic filtering support in compute shaders
        fn texture_pool_sample_grad(info: TextureInfo, attribute_uv: vec2<f32>, uv_derivs: UvDerivs) -> vec4<f32> {
            let transformed_uvs = apply_texture_transform(
                attribute_uv,
                uv_derivs,
                info,
            );

            switch info.array_index {
                {% for i in 0..texture_pool_arrays_len %}
                    case {{ i }}u: {
                        return _texture_pool_sample_grad(info, pool_tex_{{ i }}, transformed_uvs.uv, transformed_uvs.derivs);
                    }
                {% endfor %}
                default: {
                    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
                }
            }
        }


        fn _texture_pool_sample_grad(
            info: TextureInfo,
            tex: texture_2d_array<f32>,
            attribute_uv: vec2<f32>,
            uv_derivs: UvDerivs
        ) -> vec4<f32> {
            var color: vec4<f32>;


            switch info.sampler_index {
                {% for i in 0..texture_pool_samplers_len %}
                    case {{ i }}u: {
                        color = textureSampleGrad(
                            tex,
                            pool_sampler_{{ i }},
                            attribute_uv,
                            i32(info.layer_index),
                            uv_derivs.ddx,
                            uv_derivs.ddy,
                        );
                    }
                {% endfor %}
                default: {
                    color = vec4<f32>(0.0, 0.0, 0.0, 0.0);
                }
            }

            return color;
        }


    {% when MipmapMode::None %}
        // Mode-internal alias so first-party kernels in `None` mode keep calling
        // `texture_pool_sample_no_mips` (this arm) while custom-material authors
        // call the always-emitted `texture_pool_sample` below.
        // Sampling helpers for the mega-texture atlas. Every fetch receives an explicit LOD so the compute
        // pass can emulate hardware derivative selection.
        fn texture_pool_sample_no_mips(info: TextureInfo, attribute_uv: vec2<f32>) -> vec4<f32> {
            let transformed_uvs = apply_texture_transform(
                attribute_uv,
                info,
            );
            switch info.array_index {
                {% for i in 0..texture_pool_arrays_len %}
                    case {{ i }}u: {
                        return _texture_pool_sample_no_mips(info, pool_tex_{{ i }}, transformed_uvs.uv);
                    }
                {% endfor %}
                default: {
                    // If we somehow reference an out-of-range sampler (should not happen), return black to
                    // avoid propagating NaNs that could poison later colour math.
                    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
                }
            }
        }

        fn _texture_pool_sample_no_mips(
            info: TextureInfo,
            tex: texture_2d_array<f32>,
            uv: vec2<f32>,
        ) -> vec4<f32> {
            var color: vec4<f32>;
            switch info.sampler_index {
                {% for i in 0..texture_pool_samplers_len %}
                    case {{ i }}u: {
                        color = textureSampleLevel(
                            tex,
                            pool_sampler_{{ i }},
                            uv,
                            i32(info.layer_index),
                            0
                        );
                    }
                {% endfor %}
                default: {
                    color = vec4<f32>(0.0, 0.0, 0.0, 0.0);
                }
            }

            return color;
        }

{% endmatch %}


// ─── Variant-agnostic sampler for CUSTOM (dynamic-WGSL) materials ───────────
// `texture_pool_sample_grad` / `texture_pool_sample_no_mips` above are each
// emitted in ONLY one mipmap variant, but a custom material's fragment is
// compiled into ALL of them (mipmaps on AND off, MSAA on AND off) — so calling
// either from author WGSL fails to resolve in the other variant. This LOD-0
// sampler is emitted unconditionally (both mipmap modes, both the primary opaque
// + edge-resolve kernels), so authors have one stable, always-present entry
// point. Compute shaders have no automatic derivatives, so LOD 0 is the correct
// behavior regardless of the pipeline's mipmap mode.
fn texture_pool_sample(info: TextureInfo, attribute_uv: vec2<f32>) -> vec4<f32> {
    let uv = texture_transform_uvs(attribute_uv, info);
    switch info.array_index {
        {% for i in 0..texture_pool_arrays_len %}
            case {{ i }}u: {
                return _texture_pool_sample_lod0(info, pool_tex_{{ i }}, uv);
            }
        {% endfor %}
        default: {
            return vec4<f32>(0.0, 0.0, 0.0, 0.0);
        }
    }
}

fn _texture_pool_sample_lod0(
    info: TextureInfo,
    tex: texture_2d_array<f32>,
    uv: vec2<f32>,
) -> vec4<f32> {
    var color = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    switch info.sampler_index {
        {% for i in 0..texture_pool_samplers_len %}
            case {{ i }}u: {
                color = textureSampleLevel(tex, pool_sampler_{{ i }}, uv, i32(info.layer_index), 0);
            }
        {% endfor %}
        default: {}
    }
    return color;
}

// Compute-kernel alias for the dynamic-material helpers' uniformity-safe
// entry point (see the transparent pass's texture_pool_sample_nu): explicit
// LOD sampling has no uniform-control-flow requirement, so the plain path
// serves directly.
fn texture_pool_sample_nu(info: TextureInfo, attribute_uv: vec2<f32>) -> vec4<f32> {
    return texture_pool_sample(info, attribute_uv);
}
