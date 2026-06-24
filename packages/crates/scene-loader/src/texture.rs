//! Player-side texture loading: decode a bundle's `assets/<id>.png` and bind it
//! to a renderer material slot.
//!
//! The shared [`material`](crate::material) conversion is texture-LESS by design
//! (the editor and player resolve textures differently). The editor resolves
//! against its session pool (procedural / GPU-readback); the player has the raw
//! PNG bytes the bundle exported, so it decodes them here — mirroring the glTF
//! loader's embedded-image path (`bitmap::load_u8` → `ImageData::Bitmap` →
//! `textures.add_image`) and the editor's `sampler_for` / `apply_textures`.

use awsm_renderer::materials::MaterialTexture;
use awsm_renderer::textures::{SamplerCacheKey, SamplerKey};
use awsm_renderer::AwsmRenderer;
use awsm_renderer_core::image::{
    ColorSpaceConversion, ImageBitmapOptions, ImageData, PremultiplyAlpha,
};
use awsm_renderer_core::sampler::{AddressMode, FilterMode, MipmapFilterMode};
use awsm_renderer_core::texture::mipmap::MipmapTextureKind;
use awsm_renderer_core::texture::texture_pool::TextureColorInfo;
use awsm_renderer_scene::{TextureFilter, TextureRef, TextureSampler, TextureWrap, ASSETS_DIR};

use crate::assets::SceneAssets;

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

/// Resolve a [`TextureRef`] to a [`MaterialTexture`]: load `assets/<asset>.png`
/// from the bundle, decode it, upload it (staged — the caller's
/// `finalize_gpu_textures` commits), and bind the sampler + UV set +
/// `KHR_texture_transform`. `srgb` is true for base-color/emissive (color data),
/// false for normal/metallic-roughness/occlusion (linear data). `None` if the
/// texture isn't in the bundle or fails to decode.
pub async fn load_texture(
    renderer: &mut AwsmRenderer,
    assets: &impl SceneAssets,
    tref: &TextureRef,
    srgb: bool,
    mipmap_kind: MipmapTextureKind,
) -> Option<MaterialTexture> {
    let path = format!("{ASSETS_DIR}/{}.png", tref.asset);
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
    let image_data = ImageData::Bitmap { image, options };
    let format = image_data.format();

    let sampler_key = sampler_key_for(renderer, &tref.sampler)?;
    // The sampler must be pooled before `materials.insert` packs the slot's
    // uniform word (an unpooled sampler encodes as "no texture"); `get_sampler_key`
    // returns a pooled key, but ensure it explicitly (mirrors the editor).
    renderer.textures.ensure_sampler_in_pool(sampler_key);

    let color = TextureColorInfo {
        mipmap_kind,
        srgb_to_linear: srgb,
        premultiplied_alpha: None,
    };
    let key = renderer
        .textures
        .add_image(image_data, format, sampler_key, color)
        .ok()?;

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
