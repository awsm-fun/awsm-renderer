//! Player-side texture loading: decode a bundle's `assets/<id>.png` and bind it
//! to a renderer material slot.
//!
//! The shared [`material`](crate::material) conversion is texture-LESS by design
//! (the editor and player resolve textures differently). The editor resolves
//! against its session pool (procedural / GPU-readback); the player has the raw
//! PNG bytes the bundle exported, so it decodes them here — mirroring the glTF
//! loader's embedded-image path (`bitmap::load_u8` → `ImageData::Bitmap` →
//! `textures.add_image`) and the editor's `sampler_for` / `apply_textures`.

use std::collections::HashMap;

use awsm_renderer::materials::MaterialTexture;
use awsm_renderer::textures::{SamplerCacheKey, SamplerKey, TextureKey};
use awsm_renderer::AwsmRenderer;
use awsm_renderer_core::image::{
    ColorSpaceConversion, ImageBitmapOptions, ImageData, PremultiplyAlpha,
};
use awsm_renderer_core::sampler::{AddressMode, FilterMode, MipmapFilterMode};
use awsm_renderer_core::texture::mipmap::MipmapTextureKind;
use awsm_renderer_core::texture::texture_pool::TextureColorInfo;
use awsm_renderer_scene::{
    AssetId, TextureFilter, TextureRef, TextureSampler, TextureWrap, ASSETS_DIR,
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
}

/// A fetched + decoded bundle image, not yet staged in the pool.
struct DecodedImage {
    image_data: ImageData,
}

impl TextureCache {
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
        let pending: Vec<AssetId> = ids
            .into_iter()
            .filter(|id| !self.decoded.contains_key(id))
            .collect();
        let total = pending.len();
        if total == 0 {
            return;
        }
        on_progress(0, total);
        let mut stream = futures::stream::iter(
            pending
                .into_iter()
                .map(|id| async move { (id, fetch_decode(assets, id).await) }),
        )
        .buffer_unordered(PREFETCH_CONCURRENCY);
        let mut done = 0;
        while let Some((id, decoded)) = stream.next().await {
            self.decoded.insert(id, decoded);
            done += 1;
            on_progress(done, total);
        }
    }
}

/// Fetch `assets/<id>.png` and decode it to an `ImageBitmap` — the pure
/// (renderer-free) half of texture loading, so it can run concurrently.
async fn fetch_decode(assets: &impl SceneAssets, asset: AssetId) -> Option<DecodedImage> {
    let path = format!("{ASSETS_DIR}/{asset}.png");
    let Ok(bytes) = assets.fetch(&path).await else {
        // A material references this texture but the bundle didn't ship it — the
        // slot renders unbound (e.g. an extension factor with no mask → applied
        // everywhere). Loud because it's silent-wrong otherwise.
        tracing::warn!("scene-loader: bundle missing texture `{path}` — slot left unbound");
        return None;
    };

    // Decode the PNG to an ImageBitmap (browser), same options the glTF loader
    // uses for embedded images.
    let options = Some(
        ImageBitmapOptions::new()
            .with_premultiply_alpha(PremultiplyAlpha::None)
            .with_color_space_conversion(ColorSpaceConversion::Default),
    );
    let image = awsm_renderer_core::image::bitmap::load_u8(&bytes, "image/png", options.clone())
        .await
        .ok()?;
    Some(DecodedImage {
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
    };
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
            let decoded = match cache.decoded.entry(tref.asset) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                // Not prefetched (e.g. a slot the collector doesn't know about)
                // — fall back to the old on-demand path, cached for next time.
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(fetch_decode(assets, tref.asset).await)
                }
            };
            let key = decoded.as_ref().and_then(|d| {
                let color = TextureColorInfo {
                    mipmap_kind,
                    srgb_to_linear: srgb,
                    premultiplied_alpha: None,
                };
                // Clones of `ImageData::Bitmap` share the underlying JS
                // `ImageBitmap` handle — the pool never closes it, so one
                // decode can feed multiple `(srgb, kind)` pool entries.
                let image_data = d.image_data.clone();
                let format = image_data.format();
                renderer
                    .textures
                    .add_image(image_data, format, sampler_key, color)
                    .ok()
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
