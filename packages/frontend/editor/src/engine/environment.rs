//! Environment (image-based lighting + skybox). Generates the prefiltered-env +
//! irradiance + skybox cubemaps from a preset and installs them via the
//! renderer's `set_ibl` / `set_skybox`. Procedural presets (sky gradient + solid
//! colors) need no assets; the HDR preset loads `.ktx2` cubemaps from a base URL
//! (`<base>/{env,irradiance,skybox}.ktx2`).

use awsm_renderer::environment::Skybox;
use awsm_renderer::lights::ibl::{Ibl, IblTexture};
use awsm_renderer::AwsmRenderer;
use awsm_renderer_core::command::color::Color;
use awsm_renderer_core::cubemap::images::{CubemapBitmapColors, CubemapSkyGradient};
use awsm_renderer_core::cubemap::CubemapImage;

use crate::engine::context::renderer_handle;
use crate::prelude::*;

/// Environment presets the user can apply.
#[derive(Clone)]
pub enum EnvPreset {
    /// Procedural sky gradient (horizon→zenith). No assets.
    SimpleSky,
    /// Uniform white studio lighting. No assets.
    StudioWhite,
    /// Uniform neutral grey. No assets.
    NeutralGrey,
    /// Photoreal HDR: loads `<base_url>/{env,irradiance,skybox}.ktx2`.
    Hdr { base_url: String },
}

/// Apply an environment preset (spawns the async load + GPU upload).
pub fn apply(preset: EnvPreset) {
    spawn_local(async move {
        match apply_inner(preset).await {
            Ok(()) => Toast::info("Environment applied"),
            Err(e) => {
                tracing::error!("environment apply failed: {e}");
                Toast::error(format!("Environment failed: {e}"));
            }
        }
    });
}

async fn apply_inner(preset: EnvPreset) -> anyhow::Result<()> {
    let handle = renderer_handle();
    let mut r = handle.lock().await;

    // (prefiltered-env, irradiance, skybox) source cubemaps.
    let (env, irr, sky) = match &preset {
        EnvPreset::SimpleSky => {
            let g = CubemapSkyGradient::default();
            (
                CubemapImage::new_sky_gradient(g.clone(), 1024, 1024).await?,
                CubemapImage::new_sky_gradient(g.clone(), 32, 32).await?,
                CubemapImage::new_sky_gradient(g, 1024, 1024).await?,
            )
        }
        EnvPreset::StudioWhite => solid(Color::WHITE).await?,
        EnvPreset::NeutralGrey => solid(Color::new_values(0.5, 0.5, 0.5, 1.0)).await?,
        EnvPreset::Hdr { base_url } => {
            let b = base_url.trim_end_matches('/');
            (
                CubemapImage::load_url_ktx(&format!("{b}/env.ktx2")).await?,
                CubemapImage::load_url_ktx(&format!("{b}/irradiance.ktx2")).await?,
                CubemapImage::load_url_ktx(&format!("{b}/skybox.ktx2")).await?,
            )
        }
    };

    let prefiltered = ibl_texture(&mut r, env).await?;
    let irradiance = ibl_texture(&mut r, irr).await?;
    r.set_ibl(Ibl::new(prefiltered, irradiance));

    let skybox = make_skybox(&mut r, sky).await?;
    r.set_skybox(skybox);
    Ok(())
}

/// Three uniform-color cubemaps (env / irradiance / skybox) for a solid preset.
async fn solid(color: Color) -> anyhow::Result<(CubemapImage, CubemapImage, CubemapImage)> {
    let cols = CubemapBitmapColors::all(color);
    Ok((
        CubemapImage::new_colors(cols.clone(), 1024, 1024).await?,
        CubemapImage::new_colors(cols.clone(), 32, 32).await?,
        CubemapImage::new_colors(cols, 1024, 1024).await?,
    ))
}

async fn ibl_texture(r: &mut AwsmRenderer, img: CubemapImage) -> anyhow::Result<IblTexture> {
    let (texture, view, mip_count) = img
        .create_texture_and_view(&r.gpu, Some("IBL Cubemap"))
        .await?;
    let texture_key = r.textures.insert_cubemap(texture);
    let sampler_key = r
        .textures
        .get_sampler_key(&r.gpu, IblTexture::sampler_cache_key())?;
    let sampler = r.textures.get_sampler(sampler_key)?.clone();
    Ok(IblTexture::new(texture_key, view, sampler, mip_count))
}

async fn make_skybox(r: &mut AwsmRenderer, img: CubemapImage) -> anyhow::Result<Skybox> {
    let (texture, view, mip_count) = img.create_texture_and_view(&r.gpu, Some("Skybox")).await?;
    let key = r.textures.insert_cubemap(texture);
    let sampler_key = r
        .textures
        .get_sampler_key(&r.gpu, Skybox::sampler_cache_key())?;
    let sampler = r.textures.get_sampler(sampler_key)?.clone();
    Ok(Skybox::new(key, view, sampler, mip_count))
}
