@group(0) @binding(0) var composite_texture: texture_2d<f32>;

// 16-byte per-frame display uniform.
//   exposure_scale: linear pre-tonemap multiplier (exp2(EV)).
//   scale_x/scale_y: composite_size / swap-chain_size — the supersample
//                    variant's downsample ratio (1.0 when scale is off).
//   _pad_2: explicit padding so the WGSL layout matches the 16-byte
//           Rust upload exactly.
struct DisplayUniform {
    exposure_scale: f32,
    scale_x: f32,
    scale_y: f32,
    _pad_2: f32,
};
@group(0) @binding(1) var<uniform> display_uniform: DisplayUniform;
