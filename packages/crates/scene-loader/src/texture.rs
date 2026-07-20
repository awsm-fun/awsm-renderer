//! Player-side texture loading: decode a bundle's `assets/<id>.<ext>` and bind
//! it to a renderer material slot. The extension + decode path come from each
//! asset's recorded [`TextureEncoding`](awsm_renderer_scene::TextureEncoding)
//! (seeded into [`TextureCache`] from the scene's asset table), so the bundle can
//! ship PNG / JPEG / WebP without the loader guessing.
//!
//! The shared [`material`](crate::material) conversion is texture-LESS by design
//! (the editor and player resolve textures differently). The editor resolves
//! against its session pool (procedural / GPU-readback); the player has the raw
//! image bytes the bundle exported, so it decodes them here — mirroring the glTF
//! loader's embedded-image path (`bitmap::load_u8` → `ImageData::Bitmap` →
//! `textures.add_image`) and the editor's `sampler_for` / `apply_textures`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use awsm_renderer::materials::MaterialTexture;
use awsm_renderer::textures::{SamplerCacheKey, SamplerKey, TextureKey};
use awsm_renderer::AwsmRenderer;
use awsm_renderer_codec_basis::selection::{
    select_normal_transcode_target_checked, select_transcode_target_checked, sniff_basis_ktx2,
    target_is_two_plane, texture_format_for_target, TranscodeCaps,
};
use awsm_renderer_codec_basis::{TranscodeTarget, TranscodedLevel};
use awsm_renderer_core::image::{
    ColorSpaceConversion, CompressedImage, ImageBitmapOptions, ImageData, PremultiplyAlpha,
};
use awsm_renderer_core::sampler::{AddressMode, FilterMode, MipmapFilterMode};
use awsm_renderer_core::texture::mipmap::MipmapTextureKind;
use awsm_renderer_core::texture::texture_pool::TextureColorInfo;
use awsm_renderer_scene::{
    AssetId, Scene, TextureEncoding, TextureFilter, TextureRef, TextureSampler, TextureWrap,
    ASSETS_DIR,
};

use crate::assets::SceneAssets;

/// How many texture fetch+decode futures the prefetch drives concurrently.
/// Fetches overlap on the network (HTTP/2 multiplexes; HTTP/1.1 pools ~6 per
/// origin) and `createImageBitmap` decodes on browser-internal threads, so
/// this bounds resource pressure, not parallelism opportunity.
const PREFETCH_CONCURRENCY: usize = 8;

/// Per-load texture state: decoded images plus pool-entry dedupe.
///
/// Before this cache, every material SLOT independently fetched, decoded, and
/// staged its texture — a bundle whose 12 unique images are shared across
/// materials and variants uploaded 44 pool textures. Now:
///
/// * `decoded` holds one fetched+decoded image per asset — seeded concurrently
///   by [`Self::prefetch`] (the loader's `FetchingTextures` phase), filled on
///   demand for anything the prefetch collector missed. Failures are cached so
///   a missing asset warns once, not once per referencing slot.
/// * `bound` dedupes pool entries by `(asset, srgb, mipmap kind)` — the only
///   inputs the pooled texture itself depends on. Sampler / UV set /
///   `KHR_texture_transform` stay per-slot (they're cheap bindings, not
///   uploads).
#[derive(Default)]
pub struct TextureCache {
    decoded: HashMap<AssetId, Option<DecodedImage>>,
    bound: HashMap<(AssetId, bool, MipmapTextureKind), Option<TextureKey>>,
    /// Per-asset image encoding, seeded once from the scene's asset table so
    /// every fetch derives its extension + decode path from data — never a
    /// hardcoded `.png`. Assets the bake left unset (legacy bundles, procedural
    /// PNGs) resolve to `Png` on lookup, so old bundles load unchanged.
    encodings: HashMap<AssetId, TextureEncoding>,
    /// Which block-compressed families the device supports — drives the KTX2
    /// transcode-target ladder. Captured once at cache construction.
    caps: TranscodeCaps,
    /// Assets the bake flagged as TWO-CHANNEL-packed normal maps (X→RGB,
    /// Y→A — docs/plans/compression.md F3). They transcode down the
    /// two-plane ladder (BC5 / EAC-RG11) and set the material's Z-reconstruct
    /// flag via [`Self::normal_packing`].
    two_channel: HashSet<AssetId>,
}

/// A fetched + decoded bundle image, not yet staged in the pool.
enum DecodedImage {
    Bitmap {
        image_data: ImageData,
    },
    /// KTX2/Basis, already transcoded (in the Basis worker, off the main
    /// thread) to the device's block target. Kept sRGB-agnostic here: the
    /// slot's color-space picks the concrete `*Unorm`/`*UnormSrgb` format at
    /// bind time in [`load_texture`], since one asset can serve both a color
    /// slot and a data slot.
    Compressed {
        target: TranscodeTarget,
        width: u32,
        height: u32,
        levels: Vec<TranscodedLevel>,
    },
}

impl TextureCache {
    /// Seed the cache with every texture asset's [`TextureEncoding`] from the
    /// scene's asset table. Assets with no recorded encoding resolve to `Png`
    /// (the legacy default) on lookup, so old `assets/<id>.png` bundles — which
    /// predate the field — keep loading unchanged.
    pub fn new(scene: &Scene, renderer: &AwsmRenderer) -> Self {
        let encodings = scene
            .assets
            .entries
            .iter()
            .filter_map(|(id, entry)| entry.texture_encoding.map(|enc| (*id, enc)))
            .collect();
        let two_channel = scene
            .assets
            .entries
            .iter()
            .filter(|(_, entry)| entry.texture_two_channel_normal)
            .map(|(id, _)| *id)
            .collect();
        // Device caps drive the KTX2/Basis transcode-target ladder.
        let support = renderer.gpu.texture_compression();
        Self {
            encodings,
            two_channel,
            caps: TranscodeCaps {
                bc: support.bc,
                etc2: support.etc2,
                astc: support.astc,
            },
            ..Default::default()
        }
    }

    /// The recorded encoding for `asset`, or `Png` if the bundle didn't record
    /// one (legacy bundle / procedural PNG).
    fn encoding(&self, asset: AssetId) -> TextureEncoding {
        self.encodings.get(&asset).copied().unwrap_or_default()
    }

    /// The two-channel-normal shader packing for `asset`, valid after it
    /// decoded: `0` = regular full-RGB normal, `1` = X/Y in `.rg` (a
    /// two-plane BC5 / EAC-RG11 transcode), `2` = the packed RGBA layout
    /// survived (X in `.rgb`, Y in `.a` — ASTC/RGBA8 fallback rungs).
    /// Feeds the per-material Z-reconstruct flag
    /// (docs/plans/compression.md F3).
    pub fn normal_packing(&self, asset: AssetId) -> u32 {
        if !self.two_channel.contains(&asset) {
            return 0;
        }
        match self.decoded.get(&asset) {
            Some(Some(DecodedImage::Compressed { target, .. })) => {
                if target_is_two_plane(*target) {
                    1
                } else {
                    2
                }
            }
            // Flagged but decoded as a raster: the bake only flags KTX2
            // artifacts, so this shouldn't happen — but the packed layout
            // would still read via .r/.a.
            Some(Some(DecodedImage::Bitmap { .. })) => 2,
            _ => 0,
        }
    }

    /// Concurrently fetch + decode `ids` (deduped by the caller), reporting
    /// `(done, total)` per completion. Assets already decoded are skipped, so
    /// calling this twice (or after on-demand fills) never refetches.
    pub async fn prefetch(
        &mut self,
        assets: &impl SceneAssets,
        ids: Vec<AssetId>,
        mut on_progress: impl FnMut(usize, usize),
    ) {
        use futures::StreamExt;
        // Resolve each pending asset's encoding up front so the fetch futures
        // capture a plain `TextureEncoding` (not `&self`).
        let caps = self.caps;
        let pending: Vec<(AssetId, TextureEncoding, bool)> = ids
            .into_iter()
            .filter(|id| !self.decoded.contains_key(id))
            .map(|id| (id, self.encoding(id), self.two_channel.contains(&id)))
            .collect();
        let total = pending.len();
        if total == 0 {
            return;
        }
        on_progress(0, total);
        let mut stream =
            futures::stream::iter(pending.into_iter().map(|(id, enc, two_ch)| async move {
                (id, fetch_decode(assets, id, enc, caps, two_ch).await)
            }))
            .buffer_unordered(PREFETCH_CONCURRENCY);
        let mut done = 0;
        while let Some((id, decoded)) = stream.next().await {
            self.decoded.insert(id, decoded);
            done += 1;
            on_progress(done, total);
        }
    }
}

/// Fetch `assets/<id>.<ext>` and decode it to an `ImageBitmap` — the pure
/// (renderer-free) half of texture loading, so it can run concurrently. The
/// extension AND the decode route come from `encoding` (the bundle's recorded
/// [`TextureEncoding`], resolved via [`TextureCache::encoding`]), never a
/// hardcoded `.png`: a browser-decodable raster takes the zero-copy URL path
/// when the source exposes one; a GPU-compressed container always transits wasm.
async fn fetch_decode(
    assets: &impl SceneAssets,
    asset: AssetId,
    encoding: TextureEncoding,
    caps: TranscodeCaps,
    two_channel: bool,
) -> Option<DecodedImage> {
    let path = format!("{ASSETS_DIR}/{asset}.{}", encoding.ext());

    // Same decode options the glTF loader uses for embedded images.
    let options = Some(
        ImageBitmapOptions::new()
            .with_premultiply_alpha(PremultiplyAlpha::None)
            .with_color_space_conversion(ColorSpaceConversion::Default),
    );

    // Zero-copy fast path needs BOTH halves of the discriminator: the source can
    // serve a URL, AND the browser can decode this encoding. A URL to a format
    // only our wasm transcoder understands (KTX2/basis) is useless here, so
    // non-browser-decodable encodings never take it — they fall through to the
    // byte path below.
    let fast_url = if encoding.browser_decodable() {
        assets.asset_url(&path)
    } else {
        None
    };

    let image = if let Some(url) = fast_url {
        // Decode straight from the network response — the compressed image never
        // enters wasm memory.
        match awsm_renderer_core::image::bitmap::load(url, options.clone()).await {
            Ok(image) => image,
            Err(e) => {
                // Covers "bundle didn't ship it" (404 / SPA-fallback HTML that
                // won't decode) and a genuinely corrupt image. Loud because an
                // unbound slot is silent-wrong otherwise (e.g. an extension
                // factor with no mask → applied everywhere).
                tracing::warn!(
                    "scene-loader: texture `{path}` missing or undecodable — slot left unbound ({e:?})"
                );
                return None;
            }
        }
    } else {
        let Ok(bytes) = assets.fetch(&path).await else {
            tracing::warn!("scene-loader: bundle missing texture `{path}` — slot left unbound");
            return None;
        };
        // Byte path: the source has no URL (in-memory / CAS), or the encoding
        // isn't browser-decodable. Decode / transcode in wasm, by encoding.
        match encoding {
            TextureEncoding::Png | TextureEncoding::Jpeg | TextureEncoding::Webp => {
                awsm_renderer_core::image::bitmap::load_u8(&bytes, encoding.mime(), options.clone())
                    .await
                    .ok()?
            }
            TextureEncoding::Ktx2 => {
                // Basis-supercompressed KTX2: sniff codec + dims from the
                // header, pick the device's block target, transcode in the
                // Basis worker (off the main thread). Kept sRGB-agnostic —
                // the binding slot picks the *Unorm/*UnormSrgb variant.
                let Some(sniff) = sniff_basis_ktx2(&bytes) else {
                    tracing::warn!(
                        "scene-loader: texture `{path}` is not a Basis KTX2 (native/uncompressed KTX2 unsupported for materials) — slot left unbound"
                    );
                    return None;
                };
                let (codec, width, height) = (sniff.codec, sniff.width, sniff.height);
                // Two-channel-packed normals ride the two-plane ladder
                // (BC5 / EAC-RG11); everything else the full-RGBA one — or, for
                // opaque ETC1S, the 0.5 B/px opaque rung (BC1 / ETC2-RGB).
                let target = if two_channel {
                    select_normal_transcode_target_checked(caps, codec, width, height)
                } else {
                    select_transcode_target_checked(caps, codec, sniff.has_alpha, width, height)
                };
                // Per-thread client, built from the frontend's `configure(...)`
                // URLs (crate hardcodes none — a blob-worker player needs an
                // absolute URL only the app knows). Unconfigured → leave unbound.
                let Some(client) = awsm_renderer_codec_basis::client() else {
                    tracing::warn!(
                        "scene-loader: texture `{path}` KTX2 skipped — Basis codec not configured (call awsm_renderer_codec_basis::configure at startup)"
                    );
                    return None;
                };
                match client.transcode(&bytes, target).await {
                    Ok(tex) => {
                        tracing::info!(
                            "scene-loader: texture `{path}` ({codec:?} {width}x{height} alpha={}) transcoded → {target:?}, {} mips",
                            sniff.has_alpha,
                            tex.levels.len()
                        );
                        return Some(DecodedImage::Compressed {
                            target,
                            width: tex.width,
                            height: tex.height,
                            levels: tex.levels,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(
                            "scene-loader: texture `{path}` KTX2 transcode failed — slot left unbound ({e})"
                        );
                        return None;
                    }
                }
            }
        }
    };

    Some(DecodedImage::Bitmap {
        image_data: ImageData::Bitmap { image, options },
    })
}

/// Authored sampler config → a pooled renderer `SamplerKey` (mirrors the editor
/// bridge's `sampler_for`). `None` = the glTF default (repeat / linear).
fn sampler_key_for(
    renderer: &mut AwsmRenderer,
    sampler: &Option<TextureSampler>,
) -> Option<SamplerKey> {
    let s = sampler.unwrap_or_default();
    let addr = |w: TextureWrap| match w {
        TextureWrap::Repeat => AddressMode::Repeat,
        TextureWrap::ClampToEdge => AddressMode::ClampToEdge,
        TextureWrap::MirroredRepeat => AddressMode::MirrorRepeat,
    };
    let filt = |f: TextureFilter| match f {
        TextureFilter::Linear => FilterMode::Linear,
        TextureFilter::Nearest => FilterMode::Nearest,
    };
    let mip = |f: TextureFilter| match f {
        TextureFilter::Linear => MipmapFilterMode::Linear,
        TextureFilter::Nearest => MipmapFilterMode::Nearest,
    };
    let key = SamplerCacheKey {
        address_mode_u: Some(addr(s.wrap_u)),
        address_mode_v: Some(addr(s.wrap_v)),
        address_mode_w: Some(AddressMode::Repeat),
        mag_filter: Some(filt(s.mag_filter)),
        min_filter: Some(filt(s.min_filter)),
        mipmap_filter: Some(mip(s.mipmap_filter)),
        ..Default::default()
    }
    .with_anisotropy_policy(s.anisotropy);
    renderer.textures.get_sampler_key(&renderer.gpu, key).ok()
}

/// Resolve a [`TextureRef`] to a [`MaterialTexture`]: pull the decoded image
/// from `cache` (prefetched, or fetched+decoded on demand), stage it in the
/// pool ONCE per `(asset, srgb, mipmap kind)` (the caller's
/// `finalize_gpu_textures` commits), and bind the per-slot sampler + UV set +
/// `KHR_texture_transform`. `srgb` is true for base-color/emissive (color
/// data), false for normal/metallic-roughness/occlusion (linear data). `None`
/// if the texture isn't in the bundle or fails to decode.
pub async fn load_texture(
    renderer: &mut AwsmRenderer,
    cache: &mut TextureCache,
    assets: &impl SceneAssets,
    tref: &TextureRef,
    srgb: bool,
    mipmap_kind: MipmapTextureKind,
) -> Option<MaterialTexture> {
    let sampler_key = sampler_key_for(renderer, &tref.sampler)?;
    // The sampler must be pooled before `materials.insert` packs the slot's
    // uniform word (an unpooled sampler encodes as "no texture"); `get_sampler_key`
    // returns a pooled key, but ensure it explicitly (mirrors the editor).
    renderer.textures.ensure_sampler_in_pool(sampler_key);

    let bound_at = (tref.asset, srgb, mipmap_kind);
    let key = match cache.bound.get(&bound_at) {
        Some(cached) => *cached,
        None => {
            // Resolve the encoding before the mutable `decoded` borrow below.
            let encoding = cache.encoding(tref.asset);
            let caps = cache.caps;
            let cache_two_channel = cache.two_channel.contains(&tref.asset);
            let decoded = match cache.decoded.entry(tref.asset) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                // Not prefetched (e.g. a slot the collector doesn't know about)
                // — fall back to the old on-demand path, cached for next time.
                std::collections::hash_map::Entry::Vacant(v) => {
                    let two_ch = cache_two_channel;
                    v.insert(fetch_decode(assets, tref.asset, encoding, caps, two_ch).await)
                }
            };
            let key = decoded.as_ref().and_then(|d| match d {
                DecodedImage::Bitmap { image_data } => {
                    let color = TextureColorInfo {
                        mipmap_kind,
                        srgb_to_linear: srgb,
                        premultiplied_alpha: None,
                    };
                    // Clones of `ImageData::Bitmap` share the underlying JS
                    // `ImageBitmap` handle — the pool never closes it, so one
                    // decode can feed multiple `(srgb, kind)` pool entries.
                    let image_data = image_data.clone();
                    let format = image_data.format();
                    renderer
                        .textures
                        .add_image(image_data, format, sampler_key, color)
                        .ok()
                }
                DecodedImage::Compressed {
                    target,
                    width,
                    height,
                    levels,
                } => {
                    // The slot's color-space picks the concrete block format
                    // — sRGB decode rides the format on compressed uploads,
                    // so srgb_to_linear stays FALSE (the compute pass can't
                    // run on block data, and must not be requested).
                    let format = texture_format_for_target(*target, srgb)?;
                    tracing::info!(
                        "scene-loader: binding compressed texture as {format:?} (srgb={srgb})"
                    );
                    let compressed = CompressedImage {
                        format,
                        width: *width,
                        height: *height,
                        levels: levels.iter().map(|l| l.data.clone()).collect(),
                    };
                    let color = TextureColorInfo {
                        mipmap_kind,
                        srgb_to_linear: false,
                        premultiplied_alpha: None,
                    };
                    renderer
                        .textures
                        .add_image(
                            ImageData::Compressed(Arc::new(compressed)),
                            format,
                            sampler_key,
                            color,
                        )
                        .ok()
                }
            });
            cache.bound.insert(bound_at, key);
            key
        }
    };
    let key = key?;

    let transform_key = tref.transform.as_ref().map(|t| {
        renderer
            .textures
            .insert_texture_transform(&awsm_renderer::textures::TextureTransform {
                offset: t.offset,
                origin: [0.0, 0.0],
                rotation: t.rotation,
                scale: t.scale,
            })
    });

    Some(MaterialTexture {
        key,
        sampler_key: Some(sampler_key),
        uv_index: Some(tref.uv_index),
        transform_key,
    })
}
