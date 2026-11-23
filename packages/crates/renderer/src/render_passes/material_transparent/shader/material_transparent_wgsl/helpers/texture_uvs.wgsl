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

      // single-exit-switch: assign in the cases, return after the switch.
      // NEVER `return` from inside one here. Measured on an M1 Max in Chrome
      // 158: with returns inside these switches, this shader on one full-screen
      // quad took 0.77 ms at 1 sample and 8.9 ms at 4; written this way, 0.44
      // and 0.47. In the game it was 12 ms of MSAA for 40 small blended draws.
      // Why the compiler does this is not known; the spelling is what was
      // proven to trigger and to cure it. See `switches_have_a_single_exit` in
      // wgsl_validation.rs.
      var color = vec4<f32>(0.0);
      switch info.array_index {
          {% for i in 0..texture_pool_arrays_len %}
              case {{ i }}u: {
                  color = _texture_pool_sample(info, pool_tex_{{ i }}, transformed_uv);
              }
          {% endfor %}
          default: {}
      }
      return color;
  }

  fn _texture_pool_sample(
      info: TextureInfo,
      tex: texture_2d_array<f32>,
      uv: vec2<f32>
  ) -> vec4<f32> {
      // single-exit-switch (see texture_pool_sample).
      var color = vec4<f32>(0.0);
      switch info.sampler_index {
          {% for i in 0..texture_pool_samplers_len %}
              case {{ i }}u: {
                  // textureSample uses automatic derivatives - much simpler than compute!
                  color = textureSample(
                      tex,
                      pool_sampler_{{ i }},
                      uv,
                      i32(info.layer_index)
                  );
              }
          {% endfor %}
          default: {}
      }
      return color;
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
