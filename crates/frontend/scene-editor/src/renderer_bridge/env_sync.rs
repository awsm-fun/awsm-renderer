//! Scene `EnvironmentConfig` → renderer sync.
//!
//! Observes `scene.environment`; whenever it changes (either from a
//! user-driven action in `actions::view` or from a Load), pushes the
//! matching Skybox / IBL state into the renderer. Default cubemaps are
//! generated via `CubemapSkyGradient`; `Ktx` variants resolve their
//! `AssetId` against the scene asset table, then read the bytes from
//! `pending_assets` first, with fall-back to the project directory or
//! a runtime URL fetch (build artifacts).

use crate::context::renderer_handle;
use crate::fs::ProjectDir;
use crate::scene::{AssetId, AssetSource, EnvironmentConfig, IblConfig, SkyboxConfig};
use crate::state::{app_state, project::asset_disk_path};
use awsm_renderer::{
    core::cubemap::{images::CubemapSkyGradient, CubemapImage},
    environment::Skybox,
    lights::ibl::{Ibl, IblTexture},
    AwsmRenderer,
};
use awsm_web_shared::prelude::Modal;
use futures_signals::signal::SignalExt;
use wasm_bindgen_futures::spawn_local;

pub fn start() {
    let state = app_state();
    let signal = state.scene.environment.signal_cloned();
    spawn_local(async move {
        let mut previous: Option<EnvironmentConfig> = None;
        signal
            .for_each(move |env| {
                let sky_changed = previous
                    .as_ref()
                    .map(|p| p.skybox != env.skybox)
                    .unwrap_or(true);
                let ibl_changed = previous.as_ref().map(|p| p.ibl != env.ibl).unwrap_or(true);
                previous = Some(env.clone());
                async move {
                    if sky_changed {
                        if let Err(err) = apply_skybox(&env.skybox).await {
                            tracing::error!("skybox apply failed: {err}");
                            Modal::error(format!("Skybox failed: {err}"));
                        }
                    }
                    if ibl_changed {
                        if let Err(err) = apply_ibl(&env.ibl).await {
                            tracing::error!("ibl apply failed: {err}");
                            Modal::error(format!("IBL failed: {err}"));
                        }
                    }
                }
            })
            .await;
    });
}

async fn apply_skybox(cfg: &SkyboxConfig) -> anyhow::Result<()> {
    let image = match cfg {
        SkyboxConfig::BuiltInDefault => {
            CubemapImage::new_sky_gradient(CubemapSkyGradient::default(), 1024, 1024)
                .await
                .map_err(|e| anyhow::anyhow!("sky gradient: {e}"))?
        }
        SkyboxConfig::Ktx { asset_id } => load_ktx_by_id(*asset_id).await?,
    };

    let handle = renderer_handle();
    let mut renderer = handle.lock().await;
    set_skybox_on_renderer(&mut renderer, image).await
}

async fn apply_ibl(cfg: &IblConfig) -> anyhow::Result<()> {
    let (prefiltered, irradiance) = match cfg {
        IblConfig::BuiltInDefault => {
            let gradient = CubemapSkyGradient::default();
            let p = CubemapImage::new_sky_gradient(gradient.clone(), 1024, 1024)
                .await
                .map_err(|e| anyhow::anyhow!("prefiltered: {e}"))?;
            let i = CubemapImage::new_sky_gradient(gradient, 32, 32)
                .await
                .map_err(|e| anyhow::anyhow!("irradiance: {e}"))?;
            (p, i)
        }
        IblConfig::Ktx {
            prefiltered_asset_id,
            irradiance_asset_id,
        } => {
            let p = load_ktx_by_id(*prefiltered_asset_id).await?;
            let i = load_ktx_by_id(*irradiance_asset_id).await?;
            (p, i)
        }
    };

    let handle = renderer_handle();
    let mut renderer = handle.lock().await;
    set_ibl_on_renderer(&mut renderer, prefiltered, irradiance).await
}

/// Load a KTX cubemap by `AssetId`. Resolves through the scene asset
/// table, then reads bytes from `pending_assets` / disk / URL.
async fn load_ktx_by_id(asset_id: AssetId) -> anyhow::Result<CubemapImage> {
    let state = app_state();

    let source = state
        .scene
        .assets
        .lock()
        .unwrap()
        .get(asset_id)
        .map(|e| e.source.clone())
        .ok_or_else(|| anyhow::anyhow!("asset id {asset_id} not in the project asset table"))?;

    let (label, bytes) = match source {
        AssetSource::Filename(filename) => {
            let bytes = {
                let in_memory = state.pending_assets.lock().unwrap().get(&asset_id).cloned();
                match in_memory {
                    Some(b) => b,
                    None => {
                        let dir: Option<ProjectDir> =
                            state.project.lock().unwrap().directory.clone();
                        match dir {
                            Some(dir) => {
                                let disk_path = asset_disk_path(&filename);
                                dir.read_bytes(&disk_path)
                                    .await
                                    .map_err(|e| anyhow::anyhow!("read {filename}: {e}"))?
                            }
                            None => anyhow::bail!(
                                "KTX '{filename}' is not in memory and no project directory is set"
                            ),
                        }
                    }
                }
            };
            (filename, bytes)
        }
        AssetSource::Url(url) => {
            let bytes = gloo_net::http::Request::get(&url)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("fetch {url}: {e}"))?
                .binary()
                .await
                .map_err(|e| anyhow::anyhow!("fetch {url} body: {e}"))?;
            (url, bytes)
        }
        AssetSource::Material(_) | AssetSource::Texture(_) | AssetSource::Mesh(_) => {
            anyhow::bail!(
                "KTX skybox / IBL must reference a file-backed asset (Filename or Url); \
                 got procedural asset source"
            );
        }
    };

    let array = js_sys::Uint8Array::from(bytes.as_slice());
    let parts = js_sys::Array::new();
    parts.push(&array);
    let options = web_sys::BlobPropertyBag::new();
    options.set_type("application/octet-stream");
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options)
        .map_err(|e| anyhow::anyhow!("blob: {e:?}"))?;
    let url_for_loader = web_sys::Url::create_object_url_with_blob(&blob)
        .map_err(|e| anyhow::anyhow!("object url: {e:?}"))?;

    let result = CubemapImage::load_url_ktx(&url_for_loader)
        .await
        .map_err(|e| anyhow::anyhow!("load_url_ktx {label}: {e}"));
    let _ = web_sys::Url::revoke_object_url(&url_for_loader);
    result
}

async fn set_skybox_on_renderer(
    renderer: &mut AwsmRenderer,
    image: CubemapImage,
) -> anyhow::Result<()> {
    let (texture, view, mip_count) = image
        .create_texture_and_view(&renderer.gpu, Some("Skybox"))
        .await
        .map_err(|e| anyhow::anyhow!("create texture: {e}"))?;
    let texture_key = renderer.textures.insert_cubemap(texture);
    let sampler_key = renderer
        .textures
        .get_sampler_key(&renderer.gpu, Skybox::sampler_cache_key())
        .map_err(|e| anyhow::anyhow!("sampler: {e}"))?;
    let sampler = renderer
        .textures
        .get_sampler(sampler_key)
        .map_err(|e| anyhow::anyhow!("get sampler: {e}"))?
        .clone();
    renderer.set_skybox(Skybox::new(texture_key, view, sampler, mip_count));
    Ok(())
}

async fn set_ibl_on_renderer(
    renderer: &mut AwsmRenderer,
    prefiltered: CubemapImage,
    irradiance: CubemapImage,
) -> anyhow::Result<()> {
    let prefiltered_texture =
        cubemap_to_ibl_texture(renderer, prefiltered, "IBL Prefiltered Env Cubemap").await?;
    let irradiance_texture =
        cubemap_to_ibl_texture(renderer, irradiance, "IBL Irradiance Cubemap").await?;
    renderer.set_ibl(Ibl::new(prefiltered_texture, irradiance_texture));
    Ok(())
}

async fn cubemap_to_ibl_texture(
    renderer: &mut AwsmRenderer,
    image: CubemapImage,
    label: &str,
) -> anyhow::Result<IblTexture> {
    let (texture, view, mip_count) = image
        .create_texture_and_view(&renderer.gpu, Some(label))
        .await
        .map_err(|e| anyhow::anyhow!("create {label}: {e}"))?;
    let texture_key = renderer.textures.insert_cubemap(texture);
    let sampler_key = renderer
        .textures
        .get_sampler_key(&renderer.gpu, IblTexture::sampler_cache_key())
        .map_err(|e| anyhow::anyhow!("sampler: {e}"))?;
    let sampler = renderer
        .textures
        .get_sampler(sampler_key)
        .map_err(|e| anyhow::anyhow!("get sampler: {e}"))?
        .clone();
    Ok(IblTexture::new(texture_key, view, sampler, mip_count))
}
