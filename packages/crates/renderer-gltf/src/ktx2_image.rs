//! `KHR_texture_basisu` image decode for glTF import: a Basis-supercompressed
//! KTX2 payload transcodes (in the Basis worker, off the main thread) to the
//! device's block format and lands as [`ImageData::Compressed`].
//!
//! The transcode result is stored under the LINEAR format variant — the
//! compressed bytes are sRGB-agnostic (only sampler-decode semantics differ
//! between the `*Unorm`/`*UnormSrgb` pair), and the material walk swaps in
//! the sRGB sibling at bind time for color slots (see
//! `populate::material`).

use std::sync::Arc;

use awsm_renderer_codec_basis::selection::{
    select_transcode_target_checked, sniff_basis_ktx2, texture_format_for_target, TranscodeCaps,
};
use awsm_renderer_core::image::{CompressedImage, ImageData};

/// Transcode one KTX2 payload to the device's block format (RGBA8 last
/// resort), returning it sRGB-agnostic under the linear format variant.
pub(crate) async fn transcode_ktx2_image(bytes: &[u8]) -> anyhow::Result<ImageData> {
    let sniff = sniff_basis_ktx2(bytes).ok_or_else(|| {
        anyhow::anyhow!("image is not a Basis-supercompressed KTX2 (native KTX2 unsupported here)")
    })?;

    // Machine-constant snapshot; see `latest_texture_compression` docs. Reads
    // all-false before any device exists → RGBA8 fallback, never wrong.
    let support = awsm_renderer_core::renderer::latest_texture_compression();
    let caps = TranscodeCaps {
        bc: support.bc,
        etc2: support.etc2,
        astc: support.astc,
    };
    // Opaque ETC1S drops to the 0.5 B/px opaque rung (BC1 / ETC2-RGB).
    let target = select_transcode_target_checked(
        caps,
        sniff.codec,
        sniff.has_alpha,
        sniff.width,
        sniff.height,
    );

    // Per-thread client, built from the frontend's `configure(...)` URLs (crate
    // hardcodes none). Unconfigured → hard error here (import can't silently drop
    // a texture the way the player's optional slot does).
    let client = awsm_renderer_codec_basis::client().ok_or_else(|| {
        anyhow::anyhow!(
            "Basis codec not configured — call awsm_renderer_codec_basis::configure(...) at startup"
        )
    })?;
    let tex = client
        .transcode(bytes, target)
        .await
        .map_err(|e| anyhow::anyhow!("KTX2 transcode failed: {e}"))?;

    // Linear variant; bind time picks the sRGB sibling for color slots.
    let format = texture_format_for_target(target, false)
        .ok_or_else(|| anyhow::anyhow!("no linear upload format for {target:?}"))?;
    tracing::info!(
        "gltf import: KTX2 image ({:?} {}x{} alpha={}) transcoded → {target:?}, {} mips",
        sniff.codec,
        sniff.width,
        sniff.height,
        sniff.has_alpha,
        tex.levels.len()
    );

    let compressed = CompressedImage {
        format,
        width: tex.width,
        height: tex.height,
        levels: tex.levels.into_iter().map(|l| l.data).collect(),
    };
    Ok(ImageData::Compressed(Arc::new(compressed)))
}
