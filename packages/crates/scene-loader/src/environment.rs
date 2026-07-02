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
use awsm_renderer_scene::{env_ktx_path, AssetId, EnvSlot, EnvironmentConfig};

use crate::assets::SceneAssets;

/// The procedural-gradient cubemap resolution for each slot role. Skybox and the
/// prefiltered specular map are full-res; irradiance is a tiny diffuse-convolved
/// map. Shared with `env_sync` (the editor counterpart) so both paths generate
/// the built-in default identically.
const SKYBOX_SIZE: u32 = 1024;
const SPECULAR_SIZE: u32 = 1024;
const IRRADIANCE_SIZE: u32 = 32;

/// Apply a scene's `EnvironmentConfig` (skybox + IBL) to the renderer. KTX
/// cubemaps are read from the bundle by the `assets/<id>.ktx2` convention. All
/// three slots (skybox / specular / irradiance) resolve independently.
pub async fn apply_environment(
    renderer: &mut AwsmRenderer,
    env: &EnvironmentConfig,
    assets: &impl SceneAssets,
) -> Result<()> {
    let skybox = slot_image(&env.skybox, SKYBOX_SIZE, assets).await?;
    set_skybox(renderer, skybox).await?;

    let prefiltered = slot_image(&env.specular, SPECULAR_SIZE, assets).await?;
    let irradiance = slot_image(&env.irradiance, IRRADIANCE_SIZE, assets).await?;
    set_ibl(renderer, prefiltered, irradiance).await?;
    Ok(())
}

/// A `CubemapSkyGradient` from linear-RGB zenith/nadir (§18).
fn sky_gradient(zenith: [f32; 3], nadir: [f32; 3]) -> CubemapSkyGradient {
    let c = |v: [f32; 3]| Color::new_values(v[0] as f64, v[1] as f64, v[2] as f64, 1.0);
    CubemapSkyGradient::new(c(zenith), c(nadir))
}

/// Resolve one environment slot to a cubemap at the role's `size` (KTX slots
/// ignore `size` — the file carries its own resolution/mips).
async fn slot_image(
    slot: &EnvSlot,
    size: u32,
    assets: &impl SceneAssets,
) -> Result<CubemapImage> {
    match slot {
        EnvSlot::BuiltInDefault => {
            CubemapImage::new_sky_gradient(CubemapSkyGradient::default(), size, size)
                .await
                .map_err(|e| anyhow!("sky gradient: {e}"))
        }
        EnvSlot::SkyGradient { zenith, nadir } => {
            CubemapImage::new_sky_gradient(sky_gradient(*zenith, *nadir), size, size)
                .await
                .map_err(|e| anyhow!("sky gradient: {e}"))
        }
        EnvSlot::Ktx { asset_id } => load_ktx(*asset_id, assets).await,
    }
}

/// Read + parse a KTX2 cubemap from the bundle, at the shared [`env_ktx_path`]
/// convention (the same path the editor's Save/export writes).
async fn load_ktx(asset_id: AssetId, assets: &impl SceneAssets) -> Result<CubemapImage> {
    let path = env_ktx_path(asset_id);
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
