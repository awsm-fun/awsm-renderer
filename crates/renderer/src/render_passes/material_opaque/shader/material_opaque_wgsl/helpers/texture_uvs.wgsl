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
    let uv0 = _texture_uv_per_vertex(attribute_data_offset, tex_info.uv_set_index, triangle_indices.x, vertex_attribute_stride, uv_sets_index);
    let uv1 = _texture_uv_per_vertex(attribute_data_offset, tex_info.uv_set_index, triangle_indices.y, vertex_attribute_stride, uv_sets_index);
    let uv2 = _texture_uv_per_vertex(attribute_data_offset, tex_info.uv_set_index, triangle_indices.z, vertex_attribute_stride, uv_sets_index);

    let interpolated_uv = barycentric.x * uv0 + barycentric.y * uv1 + barycentric.z * uv2;

    return interpolated_uv;
}

fn _texture_uv_per_vertex(attribute_data_offset: u32, set_index: u32, vertex_index: u32, vertex_attribute_stride: u32, uv_sets_index: u32) -> vec2<f32> {
    // First get to the right vertex, THEN to the right UV set within that vertex
    let vertex_start = attribute_data_offset + (vertex_index * vertex_attribute_stride);
    // `uv_sets_index` points to the beginning of TEXCOORD_0 inside the packed stream.
    // Each additional UV set contributes two more floats per vertex.
    let uv_offset = uv_sets_index + (set_index * 2u);
    let index = vertex_start + uv_offset;
    // §16.E2: attribute_data lives in the merged geometry pool aliased
    // here by `visibility_data` (binding 5).
    let uv = vec2<f32>(visibility_data[index], visibility_data[index + 1]);

    return uv;
}


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
