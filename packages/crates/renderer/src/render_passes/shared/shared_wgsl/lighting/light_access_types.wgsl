// light_access_types.wgsl — light-data STRUCTS shared by every lit shading model.
//
// These are ALWAYS included, regardless of `inc.light_access`, because they are
// part of the bind-group ABI (`bind_groups.wgsl` declares
// `lights_info: LightsInfoPacked` and `lights: array<LightPacked>`) and they
// appear in shading-function signatures (`LightsInfo` / `LightSample`). The
// accessor FUNCTIONS that read these (get_lights_info / get_light / light_sample
// / …) live in `light_access.wgsl` and ARE gated on `inc.light_access`, so a
// material or scene that declares no lighting drops the accessors entirely while
// the binding-ABI types remain. See docs/plans/material-optimizations.md Phase 4.

// `data`: x = n_lights, y = prefiltered-env mip count, z = irradiance mip
// count, w = n_directional (count of directional lights this frame, ≤ 8).
// `directional`: packed-array indices of the (≤ 8) directional lights.
// The shading paths use these to walk *only* the directionals in
// O(n_directional) instead of scanning all `n_lights` per pixel — the
// latter is catastrophic when a scene has hundreds/thousands of punctuals
// (each pixel would skip over every punctual just to find the sun).
struct LightsInfoPacked {
    data: vec4<u32>,
    directional: array<vec4<u32>, 2>,
    // Box-projected reflection probe (bytes 48..80 of the info uniform):
    // xyz = box center, w = enabled (1.0 / 0.0); xyz = half-extents, w = pad.
    // Zeroed = disabled = classic direction-only env sampling.
    probe_center_enabled: vec4<f32>,
    probe_half_pad: vec4<f32>,
}

struct LightsInfo {
    n_lights: u32,
    ibl: IblInfo
}

struct IblInfo {
    prefiltered_env_mip_count: u32,
    irradiance_mip_count: u32,
    // Reflection-probe box for parallax-corrected specular env sampling
    // (see box_project_env_dir in shared_wgsl/math.wgsl). center_enabled.w
    // gates the correction at runtime — NOT a template axis.
    probe_center_enabled: vec4<f32>,
    probe_half: vec3<f32>,
}

struct LightPacked {
  // pos.xyz + range
  pos_range: vec4<f32>,
  // dir.xyz + inner_cone
  dir_inner: vec4<f32>,
  // color.rgb + intensity
  color_intensity: vec4<f32>,
  // kind (as uint) + outer_cone + shadow_index (bit-cast u32) + 1 pad
  kind_outer_pad: vec4<f32>,
};

struct Light {
    kind: u32,
    color: vec3<f32>,
    intensity: f32,
    position: vec3<f32>,
    range: f32,
    direction: vec3<f32>,
    inner_cone: f32,
    outer_cone: f32,
    // Index into `shadow_descriptors`. `0xFFFFFFFF` = no shadow.
    shadow_index: u32,
};

// The result of sampling one light at a surface point — the generic,
// shading-model-agnostic lighting primitive. `light_sample()` (in
// light_access.wgsl) computes it for any light kind with NO PBR/BRDF math, so
// custom materials can do Lambert / Phong / toon / whatever. The PBR path
// (`brdf_direct`) consumes the same struct; it's just one consumer.
//   - light_dir : normalized surface->light direction
//   - radiance  : color * intensity * attenuation (spot/range already applied)
//   - n_dot_l   : saturate(dot(normal, light_dir)) — the Lambert term
struct LightSample {
    normal: vec3<f32>,
    n_dot_l: f32,
    light_dir: vec3<f32>,
    radiance: vec3<f32>,
};
