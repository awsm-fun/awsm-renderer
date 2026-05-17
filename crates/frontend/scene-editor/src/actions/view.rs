//! View-layer toggles: grid visibility, skybox, IBL.
//!
//! Skybox/IBL here only mutate scene state — the renderer bridge's
//! `scene.environment` observer reacts and uploads the cubemaps to the
//! GPU. This way Load also fires the same path (hydrating env → observer
//! applies it to the renderer).

use crate::scene::{AssetId, IblConfig, SkyboxConfig};
use crate::state::{app_state, project::asset_disk_path};
use awsm_web_shared::atoms::modal::Modal;
use js_sys::Uint8Array;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::File;

pub fn set_grid_enabled(enabled: bool) {
    let state = app_state();
    state.grid_enabled.set_neq(enabled);
    tracing::info!("action: view::set_grid_enabled({enabled})");
}

pub fn set_gizmo_enabled(enabled: bool) {
    let state = app_state();
    state.gizmo_enabled.set_neq(enabled);
    tracing::info!("action: view::set_gizmo_enabled({enabled})");
}

// ---------------- Skybox ----------------

pub fn apply_default_skybox() {
    let state = app_state();
    update_env(&state, |env| env.skybox = SkyboxConfig::BuiltInDefault);
    tracing::info!("action: view::apply_default_skybox");
}

pub fn apply_skybox_ktx_file(file: File) {
    spawn_local(async move {
        if let Err(err) = stash_and_set_skybox(file).await {
            Modal::error(format!("Skybox KTX failed: {err}"));
        }
    });
}

async fn stash_and_set_skybox(file: File) -> anyhow::Result<()> {
    let asset_id = stash_ktx_bytes(&file).await?;
    let state = app_state();
    update_env(&state, |env| {
        env.skybox = SkyboxConfig::Ktx { asset_id };
    });
    Ok(())
}

// ---------------- IBL ----------------

pub fn apply_default_ibl() {
    let state = app_state();
    update_env(&state, |env| env.ibl = IblConfig::BuiltInDefault);
    tracing::info!("action: view::apply_default_ibl");
}

pub fn apply_ibl_ktx_files(prefiltered: File, irradiance: File) {
    spawn_local(async move {
        if let Err(err) = stash_and_set_ibl(prefiltered, irradiance).await {
            Modal::error(format!("IBL KTX failed: {err}"));
        }
    });
}

async fn stash_and_set_ibl(prefiltered: File, irradiance: File) -> anyhow::Result<()> {
    let prefiltered_asset_id = stash_ktx_bytes(&prefiltered).await?;
    let irradiance_asset_id = stash_ktx_bytes(&irradiance).await?;
    let state = app_state();
    update_env(&state, |env| {
        env.ibl = IblConfig::Ktx {
            prefiltered_asset_id,
            irradiance_asset_id,
        };
    });
    Ok(())
}

// ---------------- shared ----------------

/// Read `file` into bytes and register them under an `AssetId` in the scene
/// asset table + `pending_assets`. Returns the assigned `AssetId`.
async fn stash_ktx_bytes(file: &File) -> anyhow::Result<AssetId> {
    let state = app_state();
    let filename = file.name();
    if filename.is_empty() {
        anyhow::bail!("The chosen file has no name");
    }

    let asset_id = state
        .scene
        .assets
        .lock()
        .unwrap()
        .insert_filename(filename.clone());

    let dir = state.project.lock().unwrap().directory.clone();
    let disk_path = asset_disk_path(&filename);
    let already_on_disk = match &dir {
        Some(dir) => dir.file_exists(&disk_path).await,
        None => false,
    };
    let already_pending = state.pending_assets.lock().unwrap().contains_key(&asset_id);

    if !already_on_disk && !already_pending {
        let bytes = read_file_bytes(file).await?;
        state.pending_assets.lock().unwrap().insert(asset_id, bytes);
    }
    Ok(asset_id)
}

fn update_env(
    state: &crate::state::AppState,
    f: impl FnOnce(&mut crate::scene::EnvironmentConfig),
) {
    let mut env = state.scene.environment.get_cloned();
    f(&mut env);
    state.scene.environment.set(env);
    state.mark_dirty();
}

async fn read_file_bytes(file: &File) -> anyhow::Result<Vec<u8>> {
    let buffer = JsFuture::from(file.array_buffer())
        .await
        .map_err(|e| anyhow::anyhow!("array_buffer: {e:?}"))?;
    let buffer: js_sys::ArrayBuffer = buffer
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("not an ArrayBuffer"))?;
    let array = Uint8Array::new(&buffer);
    let mut out = vec![0u8; array.length() as usize];
    array.copy_to(&mut out);
    Ok(out)
}
