@group(0) @binding(0) var composite_texture: texture_2d<f32>;

// 16-byte per-frame display uniform.
//   exposure_scale: linear pre-tonemap multiplier (exp2(EV)).
//   _pad_*: explicit padding so the WGSL layout matches the 16-byte
//           Rust upload exactly.
struct DisplayUniform {
    exposure_scale: f32,
    _pad_0: f32,
    _pad_1: f32,
    _pad_2: f32,
};
@group(0) @binding(1) var<uniform> display_uniform: DisplayUniform;
