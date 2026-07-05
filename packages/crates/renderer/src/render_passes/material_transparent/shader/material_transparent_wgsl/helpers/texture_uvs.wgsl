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

// Uniformity-safe pool sample for DYNAMIC-material helpers. Their
// `MaterialData` reaches the fragment through the author-facing input
// struct, which mixes interpolated varyings — naga taints the whole value,
// so branching on `info.array_index` around `textureSample` (implicit
// derivatives) fails WGSL uniformity validation ("must only be called from
// uniform control flow"). Sample every pool array/sampler combination in
// straight-line code and select the match — non-uniform *arguments* (the
// layer index) are fine, only non-uniform *control flow* is not. First-party
// PBR keeps the cheap switch above: its material data loads straight from
// the per-draw uniform offset and stays uniform.
fn texture_pool_sample_nu(info: TextureInfo, uv: vec2<f32>) -> vec4<f32> {
    let transformed_uv = texture_transform_uvs(uv, info);
    var color = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    {% for i in 0..texture_pool_arrays_len %}
    {% for j in 0..texture_pool_samplers_len %}
    {
        let s = textureSample(pool_tex_{{ i }}, pool_sampler_{{ j }}, transformed_uv, i32(info.layer_index));
        color = select(color, s, info.array_index == {{ i }}u && info.sampler_index == {{ j }}u);
    }
    {% endfor %}
    {% endfor %}
    return color;
}
