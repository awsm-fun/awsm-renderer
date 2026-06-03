struct vs_out { @builtin(position) pos: vec4f, @location(0) uv: vec2f }

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> vs_out {
  // Fullscreen triangle
  var p = array<vec2f,3>(
    vec2f(-1.0, -3.0), vec2f(-1.0, 1.0), vec2f(3.0, 1.0)
  );
  var uv = (p[vid] * 0.5 + vec2f(0.5,0.5));
  return vs_out(vec4f(p[vid], 0.0, 1.0), uv);
}

fn radical_inverse_vd_c(bits_in: u32) -> f32 {
  var bits = bits_in;
  bits = (bits << 16u) | (bits >> 16u);
  bits = ((bits & 0x55555555u) << 1u) | ((bits & 0xAAAAAAAAu) >> 1u);
  bits = ((bits & 0x33333333u) << 2u) | ((bits & 0xCCCCCCCCu) >> 2u);
  bits = ((bits & 0x0F0F0F0Fu) << 4u) | ((bits & 0xF0F0F0F0u) >> 4u);
  bits = ((bits & 0x00FF00FFu) << 8u) | ((bits & 0xFF00FF00u) >> 8u);
  return f32(bits) * 2.3283064365386963e-10;
}
fn hammersley(i: u32, n: u32) -> vec2f {
  return vec2f(f32(i) / f32(n), radical_inverse_vd_c(i));
}
fn importance_sample_ggx(xi: vec2f, a: f32) -> vec3f {
  let a2 = a * a;
  let phi = 6.28318530718 * xi.x;
  let cos_theta = sqrt((1.0 - xi.y) / (1.0 + (a2 - 1.0) * xi.y));
  let sin_theta = sqrt(max(0.0, 1.0 - cos_theta * cos_theta));
  let h = vec3f(cos(phi) * sin_theta, sin(phi) * sin_theta, cos_theta);
  // N = (0,0,1), so tangent-to-world is identity
  return h;
}
fn geometry_schlick_ggx(ndot_v: f32, alpha: f32) -> f32 {
  let a = max(alpha, 0.001);
  let k = ((a + 1.0) * (a + 1.0)) * 0.125; // (alpha+1)^2 / 8 - must match brdf.wgsl
  return ndot_v / (ndot_v * (1.0 - k) + k);
}
fn geometry_smith(ndot_v: f32, ndot_l: f32, alpha: f32) -> f32 {
  return geometry_schlick_ggx(ndot_v, alpha) * geometry_schlick_ggx(ndot_l, alpha);
}

@fragment
fn fs_main(@location(0) uv: vec2f) -> @location(0) vec4f {
  let no_v = clamp(uv.x, 1e-3, 1.0 - 1e-3);
  let roughness = clamp(uv.y, 1e-3, 1.0 - 1e-3);

  let v = vec3f(sqrt(max(0.0, 1.0 - no_v*no_v)), 0.0, no_v);
  let n = vec3f(0.0, 0.0, 1.0);

  let sample_count: u32 = 1024u;
  var a: f32 = 0.0;
  var b: f32 = 0.0;

  let alpha = roughness * roughness;

  for (var i: u32 = 0u; i < sample_count; i = i + 1u) {
    let xi = hammersley(i, sample_count);
    let h = importance_sample_ggx(xi, alpha);
    let l = normalize(2.0 * dot(v, h) * h - v);

    let no_l = max(l.z, 0.0);
    let no_h = max(h.z, 0.0);
    let vo_h = max(dot(v, h), 0.0);
    let no_v_ = max(v.z, 0.0);

    if (no_l > 0.0) {
      let g = geometry_smith(no_v_, no_l, alpha);
      let g_vis = (g * vo_h) / max(no_h * no_v_, 1e-4);
      let fc = pow(1.0 - vo_h, 5.0);
      a = a + (1.0 - fc) * g_vis;
      b = b + fc * g_vis;
    }
  }

  a = a / f32(sample_count);
  b = b / f32(sample_count);
  return vec4f(a, b, 0.0, 1.0);
}
