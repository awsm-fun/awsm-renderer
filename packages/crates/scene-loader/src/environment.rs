//! Player-side environment (skybox + IBL) application — the player counterpart of
//! the editor's `env_sync`.
//!
//! The bundle's `scene.toml` carries `scene.environment`; this module pushes it
//! onto the renderer at load, so a scene looks the same played as authored.
//! Both environment kinds the editor can set are supported:
//! - **procedural** — the built-in default and agent-authored two-color
//!   sky-gradient (§18), generated on the fly (`CubemapSkyGradient`);
//! - **file-based** — a KTX2 cubemap, read from the bundle by the convention
//!   `assets/<id>.ktx2` (the same name the editor bake emits).
//!
//! Applied BEFORE the load's single pipeline compile so IBL-sampling materials
//! compile against the final environment (mirroring the editor's boot order).

use anyhow::{anyhow, Result};
use awsm_renderer::environment::Skybox;
use awsm_renderer::lights::ibl::{Ibl, IblTexture};
use awsm_renderer::AwsmRenderer;
use awsm_renderer_core::command::color::Color;
use awsm_renderer_core::cubemap::images::CubemapSkyGradient;
use awsm_renderer_core::cubemap::CubemapImage;
use awsm_renderer_scene::{AssetId, EnvironmentConfig, IblConfig, SkyboxConfig, ASSETS_DIR};

use crate::assets::SceneAssets;

/// Apply a scene's `EnvironmentConfig` (skybox + IBL) to the renderer. KTX
/// cubemaps are read from the bundle by the `assets/<id>.ktx2` convention.
pub async fn apply_environment(
    renderer: &mut AwsmRenderer,
    env: &EnvironmentConfig,
    assets: &impl SceneAssets,
) -> Result<()> {
    let skybox = skybox_image(&env.skybox, assets).await?;
    set_skybox(renderer, skybox).await?;

    let (prefiltered, irradiance) = ibl_images(&env.ibl, assets).await?;
    set_ibl(renderer, prefiltered, irradiance).await?;
    Ok(())
}

/// A `CubemapSkyGradient` from linear-RGB zenith/nadir (§18).
fn sky_gradient(zenith: [f32; 3], nadir: [f32; 3]) -> CubemapSkyGradient {
    let c = |v: [f32; 3]| Color::new_values(v[0] as f64, v[1] as f64, v[2] as f64, 1.0);
    CubemapSkyGradient::new(c(zenith), c(nadir))
}

async fn skybox_image(cfg: &SkyboxConfig, assets: &impl SceneAssets) -> Result<CubemapImage> {
    match cfg {
        SkyboxConfig::BuiltInDefault => {
            CubemapImage::new_sky_gradient(CubemapSkyGradient::default(), 1024, 1024)
                .await
                .map_err(|e| anyhow!("sky gradient: {e}"))
        }
        SkyboxConfig::SkyGradient { zenith, nadir } => {
            CubemapImage::new_sky_gradient(sky_gradient(*zenith, *nadir), 1024, 1024)
                .await
                .map_err(|e| anyhow!("sky gradient: {e}"))
        }
        SkyboxConfig::Ktx { asset_id } => load_ktx(*asset_id, assets).await,
    }
}

async fn ibl_images(
    cfg: &IblConfig,
    assets: &impl SceneAssets,
) -> Result<(CubemapImage, CubemapImage)> {
    match cfg {
        IblConfig::BuiltInDefault => gradient_ibl(CubemapSkyGradient::default()).await,
        IblConfig::SkyGradient { zenith, nadir } => {
            gradient_ibl(sky_gradient(*zenith, *nadir)).await
        }
        IblConfig::Ktx {
            prefiltered_asset_id,
            irradiance_asset_id,
        } => {
            let p = load_ktx(*prefiltered_asset_id, assets).await?;
            let i = load_ktx(*irradiance_asset_id, assets).await?;
            Ok((p, i))
        }
    }
}

/// Prefiltered (1024²) + irradiance (32²) env from a sky gradient — matches
/// `env_sync::gradient_ibl`.
async fn gradient_ibl(gradient: CubemapSkyGradient) -> Result<(CubemapImage, CubemapImage)> {
    let p = CubemapImage::new_sky_gradient(gradient.clone(), 1024, 1024)
        .await
        .map_err(|e| anyhow!("prefiltered: {e}"))?;
    let i = CubemapImage::new_sky_gradient(gradient, 32, 32)
        .await
        .map_err(|e| anyhow!("irradiance: {e}"))?;
    Ok((p, i))
}

/// Read + parse a KTX2 cubemap from the bundle (`assets/<id>.ktx2`).
async fn load_ktx(asset_id: AssetId, assets: &impl SceneAssets) -> Result<CubemapImage> {
    let path = format!("{ASSETS_DIR}/{}.ktx2", asset_id.0);
    let bytes = assets
        .fetch(&path)
        .await
        .map_err(|e| anyhow!("fetch env ktx {path}: {e}"))?;
    CubemapImage::load_ktx_bytes(bytes).map_err(|e| anyhow!("parse {path}: {e}"))
}

async fn set_skybox(renderer: &mut AwsmRenderer, image: CubemapImage) -> Result<()> {
    let (texture, view, mip_count) = image
        .create_texture_and_view(&renderer.gpu, Some("Skybox"))
        .await
        .map_err(|e| anyhow!("create skybox texture: {e}"))?;
    let texture_key = renderer.textures.insert_cubemap(texture);
    let sampler_key = renderer
        .textures
        .get_sampler_key(&renderer.gpu, Skybox::sampler_cache_key())
        .map_err(|e| anyhow!("skybox sampler: {e}"))?;
    let sampler = renderer
        .textures
        .get_sampler(sampler_key)
        .map_err(|e| anyhow!("get skybox sampler: {e}"))?
        .clone();
    renderer.set_skybox(Skybox::new(texture_key, view, sampler, mip_count));
    Ok(())
}

async fn set_ibl(
    renderer: &mut AwsmRenderer,
    prefiltered: CubemapImage,
    irradiance: CubemapImage,
) -> Result<()> {
    let prefiltered_texture =
        ibl_texture(renderer, prefiltered, "IBL Prefiltered Env Cubemap").await?;
    let irradiance_texture = ibl_texture(renderer, irradiance, "IBL Irradiance Cubemap").await?;
    renderer.set_ibl(Ibl::new(prefiltered_texture, irradiance_texture));
    Ok(())
}

async fn ibl_texture(
    renderer: &mut AwsmRenderer,
    image: CubemapImage,
    label: &str,
) -> Result<IblTexture> {
    let (texture, view, mip_count) = image
        .create_texture_and_view(&renderer.gpu, Some(label))
        .await
        .map_err(|e| anyhow!("create {label}: {e}"))?;
    let texture_key = renderer.textures.insert_cubemap(texture);
    let sampler_key = renderer
        .textures
        .get_sampler_key(&renderer.gpu, IblTexture::sampler_cache_key())
        .map_err(|e| anyhow!("ibl sampler: {e}"))?;
    let sampler = renderer
        .textures
        .get_sampler(sampler_key)
        .map_err(|e| anyhow!("get ibl sampler: {e}"))?
        .clone();
    Ok(IblTexture::new(texture_key, view, sampler, mip_count))
}
