//! Encoding one cube face of linear RGB f32 into the on-disk texel format.

use half::f16;

use crate::ktx2_write::Container;

/// Encode a face of linear RGB (3 floats per texel, row-major, tightly packed)
/// into `container`'s texel format.
pub fn encode_face(container: Container, rgb: &[f32], width: u32, height: u32) -> Vec<u8> {
    debug_assert_eq!(rgb.len(), (width * height * 3) as usize);
    match container {
        Container::Rg11b10 => encode_rg11b10(rgb),
        Container::Bc6h => encode_bc6h(rgb, width, height),
    }
}

/// Pack to `B10G11R11_UFLOAT_PACK32`: R in bits [0,11), G in [11,22),
/// B in [22,32).
fn encode_rg11b10(rgb: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for texel in rgb.chunks_exact(3) {
        let r = f32_to_ufloat(texel[0], 6) as u32;
        let g = f32_to_ufloat(texel[1], 6) as u32;
        let b = f32_to_ufloat(texel[2], 5) as u32;
        out.extend_from_slice(&(r | (g << 11) | (b << 22)).to_le_bytes());
    }
    out
}

/// Convert to an unsigned float with 5 exponent bits and `mant_bits` mantissa
/// bits — the 11-bit (6 mantissa) and 10-bit (5 mantissa) formats used by
/// `B10G11R11_UFLOAT_PACK32`.
///
/// Both share f16's exponent layout (5 bits, bias 15), so this routes through
/// f16 and then requantizes the mantissa, rather than re-deriving exponent
/// handling from f32. Negatives and NaN clamp to 0 (the format is unsigned);
/// infinities and overflow clamp to the largest finite value, which keeps a
/// blown-out sun bright instead of wrapping it to black.
fn f32_to_ufloat(v: f32, mant_bits: u32) -> u16 {
    // Unsigned format: negatives and NaN have no representation.
    if v.is_nan() || v <= 0.0 {
        return 0;
    }

    let h = f16::from_f32(v);
    let bits = h.to_bits();
    let exp = (bits >> 10) & 0x1f;
    let mant = bits & 0x3ff;

    let max = ((30 << mant_bits) | ((1 << mant_bits) - 1)) as u16;

    // f16 inf/NaN (exp == 31) — clamp to the largest finite value.
    if exp == 31 {
        return max;
    }

    let drop = 10 - mant_bits;
    let half = 1u32 << (drop - 1);
    let m = mant as u32;
    // Round to nearest, ties to even.
    let mut rounded = (m + half - 1 + ((m >> drop) & 1)) >> drop;
    let mut e = exp as u32;
    // Mantissa overflow carries into the exponent.
    if rounded >> mant_bits != 0 {
        rounded = 0;
        e += 1;
        if e >= 31 {
            return max;
        }
    }
    ((e << mant_bits) | rounded) as u16
}

/// f16's largest finite value.
///
/// Anything above this converts to infinity, which BC6H cannot represent and
/// which makes the block compressor emit garbage — and it is not a corner
/// case: a real outdoor HDRI's sun routinely exceeds it (PolyHaven's
/// `kloofendal_43d_clear_puresky` peaks at 114176). Clamping keeps the sun at
/// maximum representable brightness instead of blowing the block apart.
const F16_MAX: f32 = 65504.0;

/// Convert to half precision for the BC6H encoder, saturating rather than
/// overflowing to infinity. Negatives and NaN go to 0 — BC6H UFLOAT is
/// unsigned, and negative radiance is meaningless.
fn to_f16_saturating(v: f32) -> f16 {
    if v.is_nan() || v <= 0.0 {
        return f16::from_f32(0.0);
    }
    f16::from_f32(v.min(F16_MAX))
}

/// Encode to BC6H unsigned-float blocks (16 bytes per 4x4 block).
///
/// `intel_tex_2` takes an f16 RGBA surface, so the face is widened to RGBA
/// (alpha unused by BC6H) and converted to half precision first. Faces whose
/// dimensions are not a multiple of 4 are padded by edge-replication — but
/// every cmgen face size we bake is already block-aligned, so in practice this
/// never fires.
fn encode_bc6h(rgb: &[f32], width: u32, height: u32) -> Vec<u8> {
    let bw = width.div_ceil(4) * 4;
    let bh = height.div_ceil(4) * 4;

    let mut rgba = Vec::with_capacity((bw * bh * 4) as usize);
    for y in 0..bh {
        let sy = y.min(height - 1);
        for x in 0..bw {
            let sx = x.min(width - 1);
            let i = ((sy * width + sx) * 3) as usize;
            rgba.push(to_f16_saturating(rgb[i]));
            rgba.push(to_f16_saturating(rgb[i + 1]));
            rgba.push(to_f16_saturating(rgb[i + 2]));
            rgba.push(f16::from_f32(1.0));
        }
    }

    let bytes: Vec<u8> = rgba
        .iter()
        .flat_map(|h| h.to_bits().to_le_bytes())
        .collect();
    let surface = intel_tex_2::RgbaSurface {
        width: bw,
        height: bh,
        stride: bw * 4 * 2, // 4 channels, 2 bytes each
        data: &bytes,
    };
    intel_tex_2::bc6h::compress_blocks(&intel_tex_2::bc6h::very_slow_settings(), &surface)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode an 11/10-bit ufloat back to f32, for round-trip checks.
    fn ufloat_to_f32(bits: u16, mant_bits: u32) -> f32 {
        let exp = (bits >> mant_bits) as i32;
        let mant = (bits & ((1 << mant_bits) - 1)) as f32 / (1 << mant_bits) as f32;
        if exp == 0 {
            mant * 2f32.powi(-14)
        } else {
            (1.0 + mant) * 2f32.powi(exp - 15)
        }
    }

    #[test]
    fn ufloat_clamps_non_positive() {
        for v in [0.0, -1.0, -0.0, f32::NAN, f32::NEG_INFINITY] {
            assert_eq!(f32_to_ufloat(v, 6), 0, "{v} should encode to 0");
            assert_eq!(f32_to_ufloat(v, 5), 0, "{v} should encode to 0");
        }
    }

    #[test]
    fn ufloat_clamps_overflow_to_max_finite() {
        // The sun must stay bright, not wrap to black.
        let max11 = ((30 << 6) | 63) as u16;
        let max10 = ((30 << 5) | 31) as u16;
        assert_eq!(f32_to_ufloat(f32::INFINITY, 6), max11);
        assert_eq!(f32_to_ufloat(1.0e30, 6), max11);
        assert_eq!(f32_to_ufloat(f32::INFINITY, 5), max10);
    }

    #[test]
    fn ufloat_round_trips_within_quantization() {
        // HDR range that matters for IBL: dim interior through blown-out sky.
        for v in [0.001f32, 0.5, 1.0, 4.0, 100.0, 5000.0] {
            for mant_bits in [5u32, 6] {
                let back = ufloat_to_f32(f32_to_ufloat(v, mant_bits), mant_bits);
                let rel = (back - v).abs() / v;
                let tolerance = 1.0 / (1 << mant_bits) as f32;
                assert!(
                    rel <= tolerance,
                    "{v} -> {back} (rel {rel}) exceeds {tolerance} at {mant_bits} mantissa bits"
                );
            }
        }
    }

    #[test]
    fn ufloat_preserves_values_above_one() {
        // The whole reason these assets are not LDR: values > 1 must survive.
        assert!(ufloat_to_f32(f32_to_ufloat(50.0, 6), 6) > 45.0);
    }

    #[test]
    fn rg11b10_is_four_bytes_per_texel() {
        let rgb = vec![0.5f32; 4 * 4 * 3];
        assert_eq!(encode_rg11b10(&rgb).len(), 4 * 4 * 4);
    }

    #[test]
    fn f16_conversion_saturates_instead_of_overflowing() {
        // A real HDRI's sun exceeds f16 range; infinity would wreck the block.
        assert!(to_f16_saturating(114176.0).is_finite());
        assert_eq!(to_f16_saturating(114176.0).to_f32(), F16_MAX);
        assert_eq!(to_f16_saturating(f32::INFINITY).to_f32(), F16_MAX);
        // Below the limit, values pass through untouched.
        assert_eq!(to_f16_saturating(1000.0).to_f32(), 1000.0);
        // Unsigned: negatives and NaN floor at 0.
        assert_eq!(to_f16_saturating(-5.0).to_f32(), 0.0);
        assert_eq!(to_f16_saturating(f32::NAN).to_f32(), 0.0);
    }

    #[test]
    fn bc6h_encodes_over_range_suns_without_infinities() {
        // Whole-pipeline guard: a face containing an over-f16 sun must still
        // produce well-formed blocks, and must stay distinguishable from a
        // merely-bright face (i.e. it is not silently clamped to LDR).
        let sun = vec![114176.0f32; 4 * 4 * 3];
        let bright = vec![100.0f32; 4 * 4 * 3];
        let sun_blocks = encode_bc6h(&sun, 4, 4);
        let bright_blocks = encode_bc6h(&bright, 4, 4);
        assert_eq!(sun_blocks.len(), 16);
        assert_ne!(
            sun_blocks, bright_blocks,
            "an over-range sun must not collapse onto an ordinary bright value"
        );
    }

    #[test]
    fn bc6h_preserves_hdr_separation_above_one() {
        // The LDR-clipping check: values above 1.0 must stay distinct from each
        // other. An LDR codec would encode all of these identically.
        let encode_at = |v: f32| encode_bc6h(&[v; 4 * 4 * 3], 4, 4);
        let levels: Vec<Vec<u8>> = [1.0f32, 4.0, 64.0, 1000.0]
            .iter()
            .map(|&v| encode_at(v))
            .collect();
        for i in 0..levels.len() {
            for j in (i + 1)..levels.len() {
                assert_ne!(levels[i], levels[j], "HDR levels {i} and {j} must differ");
            }
        }
    }

    #[test]
    fn bc6h_is_sixteen_bytes_per_block() {
        let rgb = vec![0.5f32; 8 * 8 * 3];
        // 8x8 = 2x2 blocks = 4 blocks
        assert_eq!(encode_bc6h(&rgb, 8, 8).len(), 4 * 16);
    }

    #[test]
    fn bc6h_matches_container_face_bytes() {
        // The loader validates each level against this exact size.
        for (w, h) in [(4u32, 4u32), (16, 16), (64, 64)] {
            let rgb = vec![1.0f32; (w * h * 3) as usize];
            assert_eq!(
                encode_bc6h(&rgb, w, h).len(),
                Container::Bc6h.face_bytes(w, h)
            );
        }
    }
}
