//! Scene `EnvironmentConfig` → renderer sync (skybox + IBL).
//!
//! Observes `controller().scene.environment`; whenever it changes (from a
//! `SetEnvironment` command or a project Load), pushes the matching Skybox /
//! IBL state into the renderer. This is the single place the environment
//! reaches the GPU — the ribbon never touches the renderer directly, so the
//! environment is fully driven by `EditorController` (serialized into the
//! scene, undoable, MCP-drivable).
//!
//! `BuiltInDefault` generates a `CubemapSkyGradient` ("Simple Sky") for both
//! skybox and IBL — and because the observer fires on its first emission, the
//! editor boots with that default applied (no black void). `Ktx` variants
//! resolve their `AssetId` against the scene asset table, reading bytes from
//! the in-memory HDR stash (populated by the ribbon's 3-file picker) and
//! blob-loading them through the existing `load_url_ktx`.

use std::cell::RefCell;
use std::collections::HashMap;

use awsm_renderer::environment::Skybox;
use awsm_renderer::lights::ibl::{Ibl, IblTexture};
use awsm_renderer::AwsmRenderer;
use awsm_renderer_core::cubemap::images::CubemapSkyGradient;
use awsm_renderer_core::cubemap::CubemapImage;

use crate::controller::controller;
use crate::engine::context::renderer_handle;
use crate::engine::scene::{AssetId, AssetSource, EnvSlot, EnvironmentConfig};
use crate::prelude::*;

/// Procedural-gradient cubemap sizes per slot role — mirror
/// `scene_loader::environment` so the editor and player generate the built-in
/// default identically.
const SKYBOX_SIZE: u32 = 1024;
const SPECULAR_SIZE: u32 = 1024;
const IRRADIANCE_SIZE: u32 = 32;

thread_local! {
    /// In-memory KTX bytes for HDR assets picked this session, keyed by the
    /// `AssetId` recorded in the scene asset table. The `EnvironmentConfig`
    /// (which ids) round-trips through TOML; writing the *bytes* to the project
    /// directory on save (so HDR survives reload) is the follow-on.
    static KTX_BYTES: RefCell<HashMap<AssetId, Vec<u8>>> = RefCell::new(HashMap::new());
}

/// Stash raw KTX bytes for a freshly-picked HDR asset so `env_sync` can resolve
/// it when the `SetEnvironment` command lands.
pub fn stash_ktx(id: AssetId, bytes: Vec<u8>) {
    KTX_BYTES.with(|m| m.borrow_mut().insert(id, bytes));
}

fn stashed_ktx(id: AssetId) -> Option<Vec<u8>> {
    KTX_BYTES.with(|m| m.borrow().get(&id).cloned())
}

/// The stashed KTX bytes for `id` — the persistence seam (`persistence::ktx_files`
/// writes them to `assets/<id>.ktx2`; `restore_ktx` re-stashes them via [`stash_ktx`]
/// on reload so an HDR skybox/IBL survives Save→reload).
pub fn ktx_bytes(id: AssetId) -> Option<Vec<u8>> {
    stashed_ktx(id)
}

/// Whether KTX bytes for `id` are stashed — presence only, no byte clone (the
/// save census asks this per env id; a skybox cubemap can be 100+ MB).
pub fn has_ktx(id: AssetId) -> bool {
    KTX_BYTES.with(|m| m.borrow().contains_key(&id))
}

/// Drop every stashed KTX payload. Only the `VerifyRoundtrip` self-test calls
/// this: it models a truly cold load, where `restore_ktx` (re-reading the
/// serialized `assets/<id>.ktx2` bytes) is the ONLY way an HDR environment
/// comes back.
pub fn clear_ktx_stash() {
    KTX_BYTES.with(|m| m.borrow_mut().clear());
}

/// Apply the current `scene.environment` (skybox + IBL) ONCE, awaited — call at
/// boot AFTER the renderer is ready but BEFORE the render loop starts.
///
/// The renderer's default skybox cubemap is solid **black**; the "Simple Sky"
/// gradient only reaches the GPU through [`apply_skybox`]. That apply is
/// otherwise driven only by the async observer in [`start`], which lands after
/// the render loop has begun — and on a cold empty scene it never reflects until
/// some later event (an import) forces an opaque bind-group rebuild, so the sky
/// stays black ("black sky on cold start"). Applying it synchronously here,
/// before the first paint, means the first frame already has the gradient.
/// [`start`] seeds its `previous` to this same environment so it does not
/// redundantly re-apply on its first (replayed) emission.
pub async fn apply_initial() {
    let env = controller().scene.environment.get_cloned();
    if let Err(err) = apply_skybox(&env.skybox).await {
        tracing::error!("initial skybox apply failed: {err}");
    }
    if let Err(err) = apply_ibl(&env.specular, &env.irradiance).await {
        tracing::error!("initial ibl apply failed: {err}");
    }
    apply_probe(&env.probe).await;
}

/// Begin mirroring `scene.environment` onto the renderer. Call once at boot
/// (after the renderer is ready, and after [`apply_initial`]). `previous` is
/// seeded with the current environment so the first (replayed) emission is a
/// no-op — the initial skybox/IBL was already applied by [`apply_initial`];
/// only genuine subsequent changes re-apply.
pub fn start() {
    let signal = controller().scene.environment.signal_cloned();
    let initial = controller().scene.environment.get_cloned();
    spawn_local(async move {
        let mut previous: Option<EnvironmentConfig> = Some(initial);
        signal
            .for_each(move |env| {
                let sky_changed = previous
                    .as_ref()
                    .map(|p| p.skybox != env.skybox)
                    .unwrap_or(true);
                // IBL re-applies if EITHER the specular (prefiltered) or the
                // irradiance slot changed — both feed a single `set_ibl`.
                let ibl_changed = previous
                    .as_ref()
                    .map(|p| p.specular != env.specular || p.irradiance != env.irradiance)
                    .unwrap_or(true);
                let probe_changed = previous
                    .as_ref()
                    .map(|p| p.probe != env.probe)
                    .unwrap_or(true);
                previous = Some(env.clone());
                async move {
                    if sky_changed {
                        if let Err(err) = apply_skybox(&env.skybox).await {
                            tracing::error!("skybox apply failed: {err}");
                            Toast::error(format!("Skybox failed: {err}"));
                        }
                    }
                    if ibl_changed {
                        if let Err(err) = apply_ibl(&env.specular, &env.irradiance).await {
                            tracing::error!("ibl apply failed: {err}");
                            Toast::error(format!("IBL failed: {err}"));
                        }
                    }
                    if probe_changed {
                        apply_probe(&env.probe).await;
                    }
                }
            })
            .await;
    });
}

/// Build a `CubemapSkyGradient` from agent-supplied linear-RGB zenith/nadir (§18).
fn sky_gradient(zenith: [f32; 3], nadir: [f32; 3]) -> CubemapSkyGradient {
    use awsm_renderer_core::command::color::Color;
    let c = |v: [f32; 3]| Color::new_values(v[0] as f64, v[1] as f64, v[2] as f64, 1.0);
    CubemapSkyGradient::new(c(zenith), c(nadir))
}

async fn apply_skybox(slot: &EnvSlot) -> anyhow::Result<()> {
    let image = slot_image(slot, SKYBOX_SIZE).await?;
    let handle = renderer_handle();
    let mut renderer = handle.lock().await;
    set_skybox_on_renderer(&mut renderer, image).await
}

/// Push the box-projected reflection probe into the renderer (a pure uniform
/// update — infallible, no assets involved).
async fn apply_probe(probe: &crate::engine::scene::ReflectionProbe) {
    let handle = renderer_handle();
    let mut renderer = handle.lock().await;
    renderer
        .lights
        .set_reflection_probe(
            probe
                .enabled
                .then_some(awsm_renderer::lights::ReflectionProbeBox {
                    center: probe.center,
                    half_extents: probe.half_extents,
                }),
        );
}

async fn apply_ibl(specular: &EnvSlot, irradiance: &EnvSlot) -> anyhow::Result<()> {
    let prefiltered = slot_image(specular, SPECULAR_SIZE).await?;
    let irradiance = slot_image(irradiance, IRRADIANCE_SIZE).await?;
    let handle = renderer_handle();
    let mut renderer = handle.lock().await;
    set_ibl_on_renderer(&mut renderer, prefiltered, irradiance).await
}

/// Resolve one environment slot to a cubemap at the role's `size`. `Ktx` slots
/// ignore `size` (the file carries its own resolution/mips); the procedural
/// variants generate at `size`. §18 sky-gradient uses the same generator as the
/// built-in default.
async fn slot_image(slot: &EnvSlot, size: u32) -> anyhow::Result<CubemapImage> {
    match slot {
        EnvSlot::BuiltInDefault => {
            CubemapImage::new_sky_gradient(CubemapSkyGradient::default(), size, size)
                .await
                .map_err(|e| anyhow::anyhow!("sky gradient: {e}"))
        }
        EnvSlot::SkyGradient { zenith, nadir } => {
            CubemapImage::new_sky_gradient(sky_gradient(*zenith, *nadir), size, size)
                .await
                .map_err(|e| anyhow::anyhow!("sky gradient: {e}"))
        }
        EnvSlot::Ktx { asset_id } => load_ktx_by_id(*asset_id).await,
    }
}

/// Resolve a KTX cubemap by `AssetId`: the scene asset table gives the source,
/// then bytes come from the in-memory HDR stash (picked files) or a URL fetch.
async fn load_ktx_by_id(asset_id: AssetId) -> anyhow::Result<CubemapImage> {
    let entry = controller()
        .scene
        .assets
        .lock()
        .unwrap()
        .entries
        .get(&asset_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("asset id {asset_id} not in the project asset table"))?;

    let (label, bytes) = match &entry.source {
        AssetSource::Filename(name) => {
            let bytes = stashed_ktx(asset_id).ok_or_else(|| {
                anyhow::anyhow!(
                    "KTX '{name}' bytes aren't in memory (re-pick the HDR set; on-disk \
                     persistence of HDR assets is a follow-on)"
                )
            })?;
            (name.clone(), bytes)
        }
        AssetSource::Url(url) => {
            let bytes = gloo_net::http::Request::get(url)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("fetch {url}: {e}"))?
                .binary()
                .await
                .map_err(|e| anyhow::anyhow!("fetch {url} body: {e}"))?;
            (url.clone(), bytes)
        }
        _ => anyhow::bail!("KTX skybox / IBL must reference a Filename or Url asset"),
    };

    // Parse the KTX2 straight from bytes (same path the player's `scene_loader`
    // uses) — no browser blob / object-URL round-trip.
    CubemapImage::load_ktx_bytes(bytes).map_err(|e| anyhow::anyhow!("load ktx {label}: {e}"))
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
