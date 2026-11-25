// Rounds a non-negative colour to the nearest value an rgba16float texel can
// hold, so the `textureStore` that follows is exact.
//
// A compute-shader store converts f32 to binary16 with whatever rounding the
// device has, and on Apple GPUs that is truncation: every store loses half a
// step on average, always downwards. One store does not show. The TAA history
// is stored again every frame, so the loss accumulates until the blend's pull
// back up balances it, and a converged image sat a fraction of an 8-bit level
// dark: one level too dark on 7-16% of the pixels of a flat-shaded scene
// (measured; docs/TAA.md §7). Rounding here makes the result the same on
// every device.
//
// binary16 keeps 10 mantissa bits of f32's 23: add half of the 13 dropped
// bits and clear them (nearest, ties up; a carry out of the mantissa bumps
// the exponent, which is the right answer). Exact for values from 2^-14 up;
// below that binary16 is subnormal and coarser than this, an error under
// 6e-8. Infinity stays infinity. A NaN is not handled (`max(NaN, 0)` is
// unspecified and the mask can turn a NaN into infinity): callers pass
// finite colours only, which the resolve's finite guard ensures.
fn taa_round_f16(x: vec3<f32>) -> vec3<f32> {
    let bits = bitcast<vec3<u32>>(max(x, vec3<f32>(0.0)));
    return bitcast<vec3<f32>>((bits + vec3<u32>(0x1000u)) & vec3<u32>(0xFFFFE000u));
}
