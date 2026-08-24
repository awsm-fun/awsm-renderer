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
use std::collections::VecDeque;

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
) -> Result<EnvironmentStream> {
    let skybox = slot_image(&env.skybox, SKYBOX_SIZE, assets).await?;
    set_skybox(renderer, skybox).await?;

    let prefiltered = slot_image(&env.specular, SPECULAR_SIZE, assets).await?;
    let irradiance = slot_image(&env.irradiance, IRRADIANCE_SIZE, assets).await?;
    let stream = EnvironmentStream::new(env, &prefiltered, &irradiance);
    set_ibl(renderer, prefiltered, irradiance).await?;

    // Box-projected reflection probe: parallax-anchors the specular env
    // (IBL + SSR fallback) to the scene bounds. Disabled probes clear any
    // previously-set box.
    renderer
        .lights
        .set_reflection_probe(env.probe.enabled.then_some(
            awsm_renderer::lights::ReflectionProbeBox {
                center: env.probe.center,
                half_extents: env.probe.half_extents,
            },
        ));

    // Authored per-slot environment rotations. Each cubemap turns
    // independently — the slots are meant to be able to disagree. Set
    // unconditionally (rather than only when non-zero) so clearing a rotation
    // actually restores identity on the renderer rather than leaving the
    // previous scene's value in place.
    renderer.lights.set_env_rotations_euler_degrees(
        env.rotation.skybox,
        env.rotation.specular,
        env.rotation.irradiance,
    );
    Ok(stream)
}

/// Which environment slot a streamed level upgrades.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamSlot {
    Skybox,
    Specular,
    Irradiance,
}

/// One fetched-and-decoded streaming level, ready to apply. Produced by
/// [`EnvironmentStream::fetch_next`] (network + parse — no renderer borrow);
/// consumed by [`EnvironmentStream::apply`] (a brief renderer borrow).
pub struct PreparedEnvLevel {
    slot: StreamSlot,
    image: CubemapImage,
}

/// The progressive half of [`apply_environment`] (§ progressive environments):
/// the base slots have been applied — a load blocks on nothing else — and this
/// holds each slot's remaining resolution ladder, lowest-first. Drive it after
/// load:
///
/// ```ignore
/// while stream.has_pending() {
///     let Some(level) = stream.fetch_next(&assets).await else { continue };
///     // lock/borrow the renderer only for the swap:
///     stream.apply(&mut renderer, level).await?;
/// }
/// ```
///
/// Skybox levels swap the visible background wholesale; specular/irradiance
/// levels re-apply the IBL pair (the stream keeps the other half's
/// most-recent image so the pair always advances together). A level that
/// fails to fetch or parse is SKIPPED with a warning — a broken high-res tail
/// must never take down the (already correct) scene.
pub struct EnvironmentStream {
    pending: VecDeque<(StreamSlot, AssetId)>,
    /// Most-recent applied IBL images — the other half of the pair when a
    /// specular-only or irradiance-only level lands. `None` only on
    /// [`EnvironmentStream::empty`], whose ladder is also empty.
    specular: Option<CubemapImage>,
    irradiance: Option<CubemapImage>,
    /// Renderer textures created by PREVIOUS levels of a slot, freed when the
    /// next level of that slot lands (the base slots' textures are managed by
    /// the renderer's own skybox/IBL replace paths).
    old_skybox_texture: Option<awsm_renderer::textures::CubemapTextureKey>,
}

impl Default for EnvironmentStream {
    fn default() -> Self {
        Self::empty()
    }
}

impl std::fmt::Debug for EnvironmentStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvironmentStream")
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl EnvironmentStream {
    fn new(env: &EnvironmentConfig, specular: &CubemapImage, irradiance: &CubemapImage) -> Self {
        let mut pending = VecDeque::new();
        // Interleave? No: skybox first (it is what the viewer LOOKS at),
        // then specular, then irradiance — within a slot, ascending quality
        // exactly as authored.
        for id in &env.stream.skybox {
            pending.push_back((StreamSlot::Skybox, *id));
        }
        for id in &env.stream.specular {
            pending.push_back((StreamSlot::Specular, *id));
        }
        for id in &env.stream.irradiance {
            pending.push_back((StreamSlot::Irradiance, *id));
        }
        Self {
            pending,
            specular: Some(specular.clone()),
            irradiance: Some(irradiance.clone()),
            old_skybox_texture: None,
        }
    }

    /// An empty stream for paths that build a [`LoadedScene`] without the
    /// environment apply (tests, partial loads).
    pub fn empty() -> Self {
        Self {
            pending: VecDeque::new(),
            specular: None,
            irradiance: None,
            old_skybox_texture: None,
        }
    }

    /// Whether any levels remain to fetch.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Fetch + decode the next level. `None` when the ladder is exhausted OR
    /// the next level failed (after a warning) — call again while
    /// [`has_pending`](Self::has_pending) to continue past a bad level.
    /// Deliberately renderer-free so callers can run it outside any renderer
    /// lock.
    pub async fn fetch_next(&mut self, assets: &impl SceneAssets) -> Option<PreparedEnvLevel> {
        let (slot, asset_id) = self.pending.pop_front()?;
        match load_ktx(asset_id, assets).await {
            Ok(image) => Some(PreparedEnvLevel { slot, image }),
            Err(err) => {
                tracing::warn!("environment stream: skipping level {asset_id} ({slot:?}): {err:#}");
                None
            }
        }
    }

    /// Swap a prepared level into the renderer. Skybox: replace the
    /// background cubemap (freeing the previous STREAMED one — the load-time
    /// base is the renderer's to manage). Specular/irradiance: re-apply the
    /// IBL pair with this level's image on its side and the most-recent image
    /// on the other.
    pub async fn apply(
        &mut self,
        renderer: &mut AwsmRenderer,
        level: PreparedEnvLevel,
    ) -> Result<()> {
        match level.slot {
            StreamSlot::Skybox => {
                let replaced = set_skybox(renderer, level.image).await?;
                if let Some(old) = self.old_skybox_texture.replace(replaced) {
                    renderer.textures.remove_cubemap(old);
                }
            }
            StreamSlot::Specular => {
                self.specular = Some(level.image);
                self.reapply_ibl(renderer).await?;
            }
            StreamSlot::Irradiance => {
                self.irradiance = Some(level.image);
                self.reapply_ibl(renderer).await?;
            }
        }
        Ok(())
    }

    async fn reapply_ibl(&self, renderer: &mut AwsmRenderer) -> Result<()> {
        let (Some(specular), Some(irradiance)) = (&self.specular, &self.irradiance) else {
            // Only reachable if a caller hand-built levels against an
            // `empty()` stream; nothing correct to apply.
            tracing::warn!("environment stream: IBL level with no base pair — skipped");
            return Ok(());
        };
        set_ibl(renderer, specular.clone(), irradiance.clone()).await
    }
}

/// A `CubemapSkyGradient` from linear-RGB zenith/nadir (§18).
fn sky_gradient(zenith: [f32; 3], nadir: [f32; 3]) -> CubemapSkyGradient {
    let c = |v: [f32; 3]| Color::new_values(v[0] as f64, v[1] as f64, v[2] as f64, 1.0);
    CubemapSkyGradient::new(c(zenith), c(nadir))
}

/// Resolve one environment slot to a cubemap at the role's `size` (KTX slots
/// ignore `size` — the file carries its own resolution/mips).
async fn slot_image(slot: &EnvSlot, size: u32, assets: &impl SceneAssets) -> Result<CubemapImage> {
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

/// Returns the texture key the new skybox occupies, so streamed upgrades can
/// free their predecessor.
async fn set_skybox(
    renderer: &mut AwsmRenderer,
    image: CubemapImage,
) -> Result<awsm_renderer::textures::CubemapTextureKey> {
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
    Ok(texture_key)
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
