// Get the UV coordinates for a given texture based on its UV set index
// UVs are already interpolated by hardware and available in FragmentInput
fn texture_uv(tex_info: TextureInfo, fragment_input: FragmentInput) -> vec2<f32> {
    // Select the appropriate UV set based on tex_info.uv_set_index
    {% for i in 0..uv_sets %}
        if tex_info.uv_set_index == {{ i }}u {
            return fragment_input.uv_{{ i }};
        }
    {% endfor %}
    // No UV sets available
    return vec2<f32>(0.0);
}

fn texture_pool_sample(info: TextureInfo, uv: vec2<f32>) -> vec4<f32> {
      // Apply texture transform
      let transformed_uv = texture_transform_uvs(uv, info);

      switch info.array_index {
          {% for i in 0..texture_pool_arrays_len %}
              case {{ i }}u: {
                  return _texture_pool_sample(info, pool_tex_{{ i }}, transformed_uv);
              }
          {% endfor %}
          default: {
              return vec4<f32>(0.0);
          }
      }
  }

  fn _texture_pool_sample(
      info: TextureInfo,
      tex: texture_2d_array<f32>,
      uv: vec2<f32>
  ) -> vec4<f32> {
      switch info.sampler_index {
          {% for i in 0..texture_pool_samplers_len %}
              case {{ i }}u: {
                  // textureSample uses automatic derivatives - much simpler than compute!
                  return textureSample(
                      tex,
                      pool_sampler_{{ i }},
                      uv,
                      i32(info.layer_index)
                  );
              }
          {% endfor %}
          default: {
              return vec4<f32>(0.0);
          }
      }
  }
