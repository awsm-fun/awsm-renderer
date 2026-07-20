//! Shared integer size math for viewport-derived render targets.
//!
//! The single home for "how big is the half-res version of this viewport"
//! so that a size computed at texture-allocation time and the same size
//! reconstructed at dispatch/compare time land on the *identical* number.
//! Hand-rolling the arithmetic at each site is how the bloom per-frame
//! rebuild storm crept in: `BloomTexture::new` allocated at `view / 2`
//! (floor) while `ensure_size` reconstructed the viewport as `base * 2`,
//! so any odd viewport dimension made the equality check perpetually
//! false — rebuilding the pyramid AND re-firing `TextureViewRecreate`
//! (which recreates every texture-view-dependent bind group) every frame.
//!
//! Rule: whenever a render target is a *fraction* of the viewport, derive
//! its extent through these helpers on BOTH the allocation side and every
//! reconstruction/dispatch side. Never reverse the math (`base * 2`) to
//! recover the viewport — halving is lossy, the round trip is not exact.
//!
//! Mip-chain extents (`base >> level`) are a *different*, WebGPU-mandated
//! (floor) convention and live in
//! [`awsm_renderer_core::texture::mipmap::get_mipmap_size_for_level`];
//! use that for mip levels, these helpers for half-res targets.

/// Half of a viewport extent, **rounded up**, clamped to ≥ 1.
///
/// Ceil (not floor) so the half-res buffer fully covers the viewport when
/// it is bilinearly upsampled back to full-res: `half_extent(705) == 353`
/// and `353 * 2 = 706 ≥ 705`, so the last full-res column always has a
/// source texel. Floor would leave `704 < 705`, dropping the edge column.
/// This is the convention SSR's half-res trace target already relied on;
/// bloom's prefilter downsample shares it (a blur is indifferent to the
/// ±1 texel, but a single convention keeps the round trip exact).
#[inline]
pub fn half_extent(full: u32) -> u32 {
    full.div_ceil(2).max(1)
}

/// A viewport extent scaled by the supersampling `render_scale`,
/// **rounded to nearest**, clamped to >= 1. The single home for the
/// canvas->render-resolution mapping (same rule as the half-res helpers
/// above: derive on BOTH the allocation side and every dispatch/compare
/// side; never invert the math to recover the canvas size).
pub fn scale_extent(extent: u32, render_scale: f32) -> u32 {
    ((extent as f64 * render_scale as f64).round() as u32).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_extent_is_idempotent_round_trip() {
        // The bloom churn bug: allocate at `half_extent(view)`, then compare
        // by re-deriving `half_extent(view)`. Feeding the SAME viewport must
        // yield the SAME base for EVERY dimension (odd included), or the
        // equality check that gates the rebuild is perpetually false.
        for view in 0..=4097u32 {
            assert_eq!(half_extent(view), half_extent(view));
        }
    }

    #[test]
    fn half_extent_covers_on_upsample() {
        // Ceil guarantees `2 * half >= view`, so a bilinear upsample of the
        // half-res buffer always has a source texel for the last full-res
        // row/column (SSR relies on this; floor would drop the edge).
        for view in 1..=4097u32 {
            assert!(2 * half_extent(view) >= view, "view={view}");
        }
    }

    #[test]
    fn half_extent_clamps_to_one() {
        assert_eq!(half_extent(0), 1);
        assert_eq!(half_extent(1), 1);
        assert_eq!(half_extent(2), 1);
        assert_eq!(half_extent(3), 2);
        assert_eq!(half_extent(705), 353);
    }
}
