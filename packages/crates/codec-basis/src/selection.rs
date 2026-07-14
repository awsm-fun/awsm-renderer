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

/// The 12-byte KTX2 file identifier.
const KTX2_IDENTIFIER: [u8; 12] = [
    0xAB, b'K', b'T', b'X', b' ', b'2', b'0', 0xBB, 0x0D, 0x0A, 0x1A, 0x0A,
];

/// Everything transcode-target selection needs from a KTX2 header, read
/// without decoding the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ktx2Sniff {
    pub codec: SourceCodec,
    pub width: u32,
    pub height: u32,
    /// Whether the container carries an alpha channel. Opaque ETC1S drops to
    /// the 0.5 B/px rungs (BC1 / ETC2-RGB) — half the VRAM of the RGBA-capable
    /// ones — so this bit gates the 8× texture win. ETC1S: read from the
    /// BasisLZ global-data image descriptor (an opaque source ships no alpha
    /// slice — basisu's `check_for_alpha`). UASTC and anything we can't cheaply
    /// prove: conservatively `true` (keeps the RGBA-capable rung, so we never
    /// silently drop real alpha; UASTC's ASTC/BC7 targets are 1 B/px regardless
    /// of alpha, so opaque detection there would win no VRAM anyway).
    pub has_alpha: bool,
}

/// Cheap header sniff of a (possibly Basis-supercompressed) KTX2 file —
/// everything target selection needs before handing the container to the
/// transcoder worker. `None` when the bytes aren't KTX2 or aren't Basis-encoded
/// (a native/uncompressed KTX2 belongs on a different path).
///
/// Codec rule (KTX2 spec): Basis containers have `vkFormat == 0`
/// (`VK_FORMAT_UNDEFINED`); ETC1S is exactly `supercompressionScheme ==
/// BasisLZ (1)`, while UASTC rides Zstd (2) or no supercompression.
pub fn sniff_basis_ktx2(bytes: &[u8]) -> Option<Ktx2Sniff> {
    if bytes.len() < 48 || bytes[0..12] != KTX2_IDENTIFIER {
        return None;
    }
    let u32_at = |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let vk_format = u32_at(12);
    let width = u32_at(20);
    let height = u32_at(24);
    let scheme = u32_at(44);
    if vk_format != 0 {
        // A concrete Vulkan format = native KTX2, not Basis.
        return None;
    }
    let codec = if scheme == 1 {
        SourceCodec::Etc1s
    } else {
        SourceCodec::Uastc
    };
    // Alpha matters only for the ETC1S opaque rung (see `Ktx2Sniff::has_alpha`);
    // an unreadable SGD is treated as "has alpha" so we stay on the safe rung.
    let has_alpha = match codec {
        SourceCodec::Etc1s => etc1s_has_alpha(bytes).unwrap_or(true),
        SourceCodec::Uastc => true,
    };
    Some(Ktx2Sniff {
        codec,
        width,
        height,
        has_alpha,
    })
}

/// Read alpha-slice presence from an ETC1S (BasisLZ) KTX2's supercompression
/// global data. The first image descriptor's `alphaSliceByteLength` is non-zero
/// exactly when the encoder wrote an alpha slice (opaque sources ship none).
/// `None` when the SGD can't be located/read — the caller treats that as
/// "assume alpha" (never wrongly picks the opaque rung).
fn etc1s_has_alpha(bytes: &[u8]) -> Option<bool> {
    // KTX2 index: `sgdByteOffset` is a u64 at byte 64. The BasisLZ global
    // header is 20 bytes; `imageDesc[0]` follows, and its
    // `alphaSliceByteLength` is the 5th u32 (byte +16 within the descriptor).
    let sgd_offset = u64::from_le_bytes(bytes.get(64..72)?.try_into().ok()?) as usize;
    let alpha_len_at = sgd_offset.checked_add(20 + 16)?;
    let alpha_len = u32::from_le_bytes(bytes.get(alpha_len_at..alpha_len_at + 4)?.try_into().ok()?);
    Some(alpha_len != 0)
}

/// WebGPU requires a block-compressed texture's BASE dimensions to be
/// multiples of the block size (4×4 for every target we pick). Textures that
/// fail this must fall back to RGBA8 at runtime (or WebP-lossless at encode
/// time). Mip tails may be non-multiples; only the base level matters.
pub fn dims_block_compatible(width: u32, height: u32) -> bool {
    width % 4 == 0 && height % 4 == 0 && width > 0 && height > 0
}

/// Pick the transcode target for a texture, by caps + source codec + alpha.
/// `dims_block_compatible` must be checked by the caller first (or pass the
/// dims through [`select_transcode_target_checked`]).
///
/// `has_alpha` is the container's own alpha flag (from [`sniff_basis_ktx2`]).
/// An opaque ETC1S texture transcodes to the 0.5 B/px opaque-only rungs
/// (ETC2-RGB / BC1) — half the VRAM of the RGBA-capable rungs, the 8× texture
/// reduction. It can never drop real alpha: the opaque rung is picked only when
/// the encoder proved there was no alpha to begin with. UASTC ignores it (its
/// ASTC/BC7 targets are 1 B/px regardless of alpha).
pub fn select_transcode_target(
    caps: TranscodeCaps,
    codec: SourceCodec,
    has_alpha: bool,
) -> TranscodeTarget {
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
            if !has_alpha {
                // Opaque: the 0.5 B/px rungs. ETC2-RGB and BC1 are opaque-only;
                // ASTC-4x4 has no sub-1 B/px mode, so astc-only devices stay on
                // the RGBA ladder below (correct, just no VRAM win).
                if caps.etc2 {
                    return TranscodeTarget::Etc1Rgb;
                } else if caps.bc {
                    return TranscodeTarget::Bc1;
                }
            }
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
    has_alpha: bool,
    width: u32,
    height: u32,
) -> TranscodeTarget {
    if !dims_block_compatible(width, height) {
        return TranscodeTarget::Rgba32;
    }
    select_transcode_target(caps, codec, has_alpha)
}

/// Pick the transcode target for a TWO-CHANNEL-packed normal map (X→RGB,
/// Y→A at encode — docs/plans/compression.md F3): BC5 on BC hardware,
/// EAC-RG11 on ETC2 hardware (both two-plane formats the Basis transcoder
/// fills from the packed RGB+A planes), else the regular full-RGBA ladder —
/// the packed layout survives there (X in RGB, Y in A), the shader's
/// Z-reconstruct just reads Y from `.a` instead of `.g`.
pub fn select_normal_transcode_target(caps: TranscodeCaps, codec: SourceCodec) -> TranscodeTarget {
    if caps.bc {
        TranscodeTarget::Bc5
    } else if caps.etc2 {
        TranscodeTarget::EacRg11
    } else {
        // Packed normals keep both channels (X in RGB, Y in A), so they ride
        // the full-RGBA rung — never the opaque one.
        select_transcode_target(caps, codec, true)
    }
}

/// [`select_normal_transcode_target`] with the multiple-of-4 guard folded in.
pub fn select_normal_transcode_target_checked(
    caps: TranscodeCaps,
    codec: SourceCodec,
    width: u32,
    height: u32,
) -> TranscodeTarget {
    if !dims_block_compatible(width, height) {
        return TranscodeTarget::Rgba32;
    }
    select_normal_transcode_target(caps, codec)
}

/// True when a transcode target delivers a two-channel-packed normal's X/Y in
/// `.rg` (the dedicated two-plane formats); false = the packed RGBA layout
/// survives verbatim (X in `.rgb`, Y in `.a`). Drives the per-material shader
/// flag's channel-layout bit.
pub fn target_is_two_plane(target: TranscodeTarget) -> bool {
    matches!(target, TranscodeTarget::Bc5 | TranscodeTarget::EacRg11)
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
        // With alpha (the RGBA-capable ladders).
        // Desktop BC-only: everything lands on BC7.
        assert_eq!(select_transcode_target(DESKTOP, Uastc, true), Bc7);
        assert_eq!(select_transcode_target(DESKTOP, Etc1s, true), Bc7);
        // Mobile: UASTC → ASTC, ETC1S stays in-family on ETC2.
        assert_eq!(select_transcode_target(MOBILE, Uastc, true), Astc4x4);
        assert_eq!(select_transcode_target(MOBILE, Etc1s, true), Etc2Rgba);
        // Apple Silicon (all three): same picks as mobile.
        assert_eq!(select_transcode_target(APPLE, Uastc, true), Astc4x4);
        assert_eq!(select_transcode_target(APPLE, Etc1s, true), Etc2Rgba);
        // No caps: RGBA8 last resort.
        assert_eq!(select_transcode_target(NONE, Uastc, true), Rgba32);
        assert_eq!(select_transcode_target(NONE, Etc1s, true), Rgba32);
    }

    /// Opaque ETC1S drops to the 0.5 B/px rungs (the 8× texture win); opaque
    /// UASTC is unaffected (ASTC/BC7 are 1 B/px regardless), and an opaque
    /// texture can never land somewhere an alpha one couldn't have — it just
    /// halves the VRAM.
    #[test]
    fn opaque_etc1s_takes_the_half_rate_rung() {
        use SourceCodec::*;
        use TranscodeTarget::*;
        // Desktop BC-only: BC1 (0.5 B/px) instead of BC7 (1 B/px).
        assert_eq!(select_transcode_target(DESKTOP, Etc1s, false), Bc1);
        // Mobile / Apple (ETC2): ETC2-RGB instead of ETC2-RGBA.
        assert_eq!(select_transcode_target(MOBILE, Etc1s, false), Etc1Rgb);
        assert_eq!(select_transcode_target(APPLE, Etc1s, false), Etc1Rgb);
        // ASTC-only has no sub-1 B/px mode → stays on the RGBA ladder.
        let astc_only = TranscodeCaps {
            bc: false,
            etc2: false,
            astc: true,
        };
        assert_eq!(select_transcode_target(astc_only, Etc1s, false), Astc4x4);
        // No caps → RGBA8 last resort regardless of alpha.
        assert_eq!(select_transcode_target(NONE, Etc1s, false), Rgba32);
        // UASTC ignores the opaque flag entirely.
        assert_eq!(select_transcode_target(DESKTOP, Uastc, false), Bc7);
        assert_eq!(select_transcode_target(MOBILE, Uastc, false), Astc4x4);
        // The opaque rungs are the half-rate 0.5 B/px formats.
        assert_eq!(
            texture_format_for_target(Bc1, true),
            Some(GpuTextureFormat::Bc1RgbaUnormSrgb)
        );
        assert_eq!(
            texture_format_for_target(Etc1Rgb, true),
            Some(GpuTextureFormat::Etc2Rgb8unormSrgb)
        );
    }

    /// Two-channel normals: BC5 on BC hardware, EAC-RG11 on ETC2, and the
    /// regular RGBA ladder (packed layout intact) everywhere else.
    #[test]
    fn normal_ladder() {
        use SourceCodec::*;
        use TranscodeTarget::*;
        assert_eq!(select_normal_transcode_target(DESKTOP, Uastc), Bc5);
        assert_eq!(select_normal_transcode_target(APPLE, Uastc), Bc5);
        assert_eq!(select_normal_transcode_target(MOBILE, Uastc), EacRg11);
        // No two-plane caps: falls back to the full-RGBA ladder.
        assert_eq!(select_normal_transcode_target(NONE, Uastc), Rgba32);
        let astc_only = TranscodeCaps {
            bc: false,
            etc2: false,
            astc: true,
        };
        assert_eq!(select_normal_transcode_target(astc_only, Uastc), Astc4x4);
        // Non-multiple-of-4 dims: RGBA8, like the color path.
        assert_eq!(
            select_normal_transcode_target_checked(DESKTOP, Uastc, 100, 30),
            Rgba32
        );
        assert!(target_is_two_plane(Bc5));
        assert!(target_is_two_plane(EacRg11));
        assert!(!target_is_two_plane(Astc4x4));
        assert!(!target_is_two_plane(Rgba32));
    }

    #[test]
    fn sniffs_basis_ktx2_headers() {
        let mut header = vec![0u8; 48];
        header[0..12].copy_from_slice(&KTX2_IDENTIFIER);
        header[20..24].copy_from_slice(&1024u32.to_le_bytes());
        header[24..28].copy_from_slice(&512u32.to_le_bytes());

        // vkFormat=0 + BasisLZ → ETC1S. No SGD in this stub header ⇒ alpha
        // unknown ⇒ conservatively `has_alpha: true`.
        header[44..48].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(
            sniff_basis_ktx2(&header),
            Some(Ktx2Sniff {
                codec: SourceCodec::Etc1s,
                width: 1024,
                height: 512,
                has_alpha: true,
            })
        );
        // vkFormat=0 + Zstd → UASTC (always reported has_alpha: true).
        header[44..48].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            sniff_basis_ktx2(&header),
            Some(Ktx2Sniff {
                codec: SourceCodec::Uastc,
                width: 1024,
                height: 512,
                has_alpha: true,
            })
        );
        // Concrete vkFormat → native KTX2, not Basis.
        header[12..16].copy_from_slice(&37u32.to_le_bytes()); // VK_FORMAT_R8G8B8A8_UNORM
        assert_eq!(sniff_basis_ktx2(&header), None);
        // Not KTX2 at all.
        assert_eq!(sniff_basis_ktx2(b"glTF whatever"), None);
    }

    /// ETC1S alpha comes from the BasisLZ global-data image descriptor:
    /// `imageDesc[0].alphaSliceByteLength` (byte `sgdOffset + 36`), non-zero
    /// exactly when the encoder wrote an alpha slice.
    #[test]
    fn sniffs_etc1s_alpha_from_the_sgd() {
        // Header (80 bytes) + level index (unused here) + SGD placed at 200.
        let sgd_offset = 200usize;
        let mut buf = vec![0u8; sgd_offset + 40];
        buf[0..12].copy_from_slice(&KTX2_IDENTIFIER);
        buf[20..24].copy_from_slice(&8u32.to_le_bytes()); // width
        buf[24..28].copy_from_slice(&8u32.to_le_bytes()); // height
        buf[44..48].copy_from_slice(&1u32.to_le_bytes()); // BasisLZ → ETC1S
        buf[64..72].copy_from_slice(&(sgd_offset as u64).to_le_bytes()); // sgdByteOffset

        // alphaSliceByteLength (imageDesc[0] +16 = sgdOffset + 36) = 0 → opaque.
        buf[sgd_offset + 36..sgd_offset + 40].copy_from_slice(&0u32.to_le_bytes());
        assert!(!sniff_basis_ktx2(&buf).unwrap().has_alpha);

        // Non-zero → has alpha.
        buf[sgd_offset + 36..sgd_offset + 40].copy_from_slice(&4772u32.to_le_bytes());
        assert!(sniff_basis_ktx2(&buf).unwrap().has_alpha);

        // Truncated before the SGD field → unknown → conservatively has alpha.
        let truncated = buf[..sgd_offset + 10].to_vec();
        assert!(sniff_basis_ktx2(&truncated).unwrap().has_alpha);
    }

    #[test]
    fn multiple_of_four_guard() {
        assert!(dims_block_compatible(1024, 256));
        assert!(!dims_block_compatible(1023, 256));
        assert!(!dims_block_compatible(1024, 2));
        assert!(!dims_block_compatible(0, 0));
        assert_eq!(
            select_transcode_target_checked(APPLE, SourceCodec::Uastc, true, 100, 30),
            TranscodeTarget::Rgba32
        );
        assert_eq!(
            select_transcode_target_checked(APPLE, SourceCodec::Uastc, true, 100, 32),
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
