//! Transcode-target selection: device caps + source codec + slot color-space
//! → the concrete Basis transcode target and WebGPU block format.
//!
//! Ladders (docs/plans/compression.md):
//! - **UASTC**:      ASTC-4x4 → BC7 → ETC2-RGBA → RGBA8
//! - **ETC1S color**: ETC2-RGBA → BC7 → ASTC-4x4 → RGBA8
//!
//! ETC1S prefers the ETC2 family (same block layout, cheapest transcode);
//! UASTC prefers ASTC (native quality). BC7 covers desktop for both. Every
//! rung is an RGBA-capable format so alpha never gates the choice; RGBA8 is
//! the universal last resort (software adapters, mostly). Normal maps ride
//! the same ladder as full-RGB first cut (two-channel BC5/EAC-RG is the
//! Phase-6 optimization).

use crate::TranscodeTarget;
use web_sys::GpuTextureFormat;

/// Which block-compressed families the device supports. Mirrors
/// `renderer-core`'s `TextureCompressionSupport` (constructed from it by
/// callers; this crate stays independent of renderer-core).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TranscodeCaps {
    pub bc: bool,
    pub etc2: bool,
    pub astc: bool,
}

/// The codec a Basis-supercompressed KTX2 was encoded with (from the KTX2
/// container / `isUastc` in the transcode reply).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceCodec {
    /// Small, color textures (base color / roughness-metallic / emissive).
    Etc1s,
    /// Higher quality; our normal-map default.
    Uastc,
}

/// WebGPU requires a block-compressed texture's BASE dimensions to be
/// multiples of the block size (4×4 for every target we pick). Textures that
/// fail this must fall back to RGBA8 at runtime (or WebP-lossless at encode
/// time). Mip tails may be non-multiples; only the base level matters.
pub fn dims_block_compatible(width: u32, height: u32) -> bool {
    width % 4 == 0 && height % 4 == 0 && width > 0 && height > 0
}

/// Pick the transcode target for a texture, by caps + source codec.
/// `dims_block_compatible` must be checked by the caller first (or pass the
/// dims through [`select_transcode_target_checked`]).
pub fn select_transcode_target(caps: TranscodeCaps, codec: SourceCodec) -> TranscodeTarget {
    match codec {
        SourceCodec::Uastc => {
            if caps.astc {
                TranscodeTarget::Astc4x4
            } else if caps.bc {
                TranscodeTarget::Bc7
            } else if caps.etc2 {
                TranscodeTarget::Etc2Rgba
            } else {
                TranscodeTarget::Rgba32
            }
        }
        SourceCodec::Etc1s => {
            if caps.etc2 {
                TranscodeTarget::Etc2Rgba
            } else if caps.bc {
                TranscodeTarget::Bc7
            } else if caps.astc {
                TranscodeTarget::Astc4x4
            } else {
                TranscodeTarget::Rgba32
            }
        }
    }
}

/// [`select_transcode_target`] with the multiple-of-4 guard folded in:
/// non-block-compatible dimensions always yield the RGBA8 fallback.
pub fn select_transcode_target_checked(
    caps: TranscodeCaps,
    codec: SourceCodec,
    width: u32,
    height: u32,
) -> TranscodeTarget {
    if !dims_block_compatible(width, height) {
        return TranscodeTarget::Rgba32;
    }
    select_transcode_target(caps, codec)
}

/// The WebGPU texture format a transcode target uploads as. `srgb` comes from
/// the material slot (base color / emissive = true; normal / MR / occlusion =
/// false) — on compressed formats the sRGB decode rides HERE, in the format,
/// because the `srgb_to_linear` compute pass can't run on block data.
///
/// Returns `None` for target/color-space combinations that don't exist
/// (sRGB two-channel formats).
pub fn texture_format_for_target(target: TranscodeTarget, srgb: bool) -> Option<GpuTextureFormat> {
    Some(match (target, srgb) {
        (TranscodeTarget::Astc4x4, false) => GpuTextureFormat::Astc4x4Unorm,
        (TranscodeTarget::Astc4x4, true) => GpuTextureFormat::Astc4x4UnormSrgb,
        (TranscodeTarget::Bc7, false) => GpuTextureFormat::Bc7RgbaUnorm,
        (TranscodeTarget::Bc7, true) => GpuTextureFormat::Bc7RgbaUnormSrgb,
        (TranscodeTarget::Etc2Rgba, false) => GpuTextureFormat::Etc2Rgba8unorm,
        (TranscodeTarget::Etc2Rgba, true) => GpuTextureFormat::Etc2Rgba8unormSrgb,
        (TranscodeTarget::Bc3, false) => GpuTextureFormat::Bc3RgbaUnorm,
        (TranscodeTarget::Bc3, true) => GpuTextureFormat::Bc3RgbaUnormSrgb,
        (TranscodeTarget::Bc1, false) => GpuTextureFormat::Bc1RgbaUnorm,
        (TranscodeTarget::Bc1, true) => GpuTextureFormat::Bc1RgbaUnormSrgb,
        // ETC1 payloads are valid ETC2-RGB blocks (superset codec).
        (TranscodeTarget::Etc1Rgb, false) => GpuTextureFormat::Etc2Rgb8unorm,
        (TranscodeTarget::Etc1Rgb, true) => GpuTextureFormat::Etc2Rgb8unormSrgb,
        // Two-channel targets are linear-only (normal/data slots).
        (TranscodeTarget::Bc5, false) => GpuTextureFormat::Bc5RgUnorm,
        (TranscodeTarget::EacRg11, false) => GpuTextureFormat::EacRg11unorm,
        (TranscodeTarget::Bc5 | TranscodeTarget::EacRg11, true) => return None,
        (TranscodeTarget::Rgba32, false) => GpuTextureFormat::Rgba8unorm,
        (TranscodeTarget::Rgba32, true) => GpuTextureFormat::Rgba8unormSrgb,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESKTOP: TranscodeCaps = TranscodeCaps {
        bc: true,
        etc2: false,
        astc: false,
    };
    const MOBILE: TranscodeCaps = TranscodeCaps {
        bc: false,
        etc2: true,
        astc: true,
    };
    const APPLE: TranscodeCaps = TranscodeCaps {
        bc: true,
        etc2: true,
        astc: true,
    };
    const NONE: TranscodeCaps = TranscodeCaps {
        bc: false,
        etc2: false,
        astc: false,
    };

    #[test]
    fn ladders() {
        use SourceCodec::*;
        use TranscodeTarget::*;
        // Desktop BC-only: everything lands on BC7.
        assert_eq!(select_transcode_target(DESKTOP, Uastc), Bc7);
        assert_eq!(select_transcode_target(DESKTOP, Etc1s), Bc7);
        // Mobile: UASTC → ASTC, ETC1S stays in-family on ETC2.
        assert_eq!(select_transcode_target(MOBILE, Uastc), Astc4x4);
        assert_eq!(select_transcode_target(MOBILE, Etc1s), Etc2Rgba);
        // Apple Silicon (all three): same picks as mobile.
        assert_eq!(select_transcode_target(APPLE, Uastc), Astc4x4);
        assert_eq!(select_transcode_target(APPLE, Etc1s), Etc2Rgba);
        // No caps: RGBA8 last resort.
        assert_eq!(select_transcode_target(NONE, Uastc), Rgba32);
        assert_eq!(select_transcode_target(NONE, Etc1s), Rgba32);
    }

    #[test]
    fn multiple_of_four_guard() {
        assert!(dims_block_compatible(1024, 256));
        assert!(!dims_block_compatible(1023, 256));
        assert!(!dims_block_compatible(1024, 2));
        assert!(!dims_block_compatible(0, 0));
        assert_eq!(
            select_transcode_target_checked(APPLE, SourceCodec::Uastc, 100, 30),
            TranscodeTarget::Rgba32
        );
        assert_eq!(
            select_transcode_target_checked(APPLE, SourceCodec::Uastc, 100, 32),
            TranscodeTarget::Astc4x4
        );
    }

    #[test]
    fn formats_carry_srgb_in_the_format() {
        assert_eq!(
            texture_format_for_target(TranscodeTarget::Bc7, true),
            Some(GpuTextureFormat::Bc7RgbaUnormSrgb)
        );
        assert_eq!(
            texture_format_for_target(TranscodeTarget::Bc7, false),
            Some(GpuTextureFormat::Bc7RgbaUnorm)
        );
        assert_eq!(
            texture_format_for_target(TranscodeTarget::Rgba32, true),
            Some(GpuTextureFormat::Rgba8unormSrgb)
        );
        // Two-channel formats have no sRGB variant.
        assert_eq!(texture_format_for_target(TranscodeTarget::Bc5, true), None);
        assert_eq!(
            texture_format_for_target(TranscodeTarget::Bc5, false),
            Some(GpuTextureFormat::Bc5RgUnorm)
        );
        // ETC1 uploads as ETC2-RGB (superset).
        assert_eq!(
            texture_format_for_target(TranscodeTarget::Etc1Rgb, true),
            Some(GpuTextureFormat::Etc2Rgb8unormSrgb)
        );
    }
}
