//! One deterministic playback session: a fresh renderer on a fixed-size
//! canvas, one `taa-*` (or any) bundle loaded through the real player loader,
//! and the scene's scripted scenario played with a fixed 1/60 s timestep.
//!
//! Determinism rules:
//! - Time: `set_time_source(k / 60)` every frame (shader time, UV flows), and
//!   `update_animations(1000 / 60)` / `ParticleDriver::tick(1 / 60)` with the
//!   fixed step (0 on frame 0 and on frozen re-renders). Nothing reads the
//!   wall clock.
//! - Particles: the loader's `ParticleDriver` seeds each emitter from its
//!   stable sorted index.
//! - Frame count: exactly `warmup` frames are rendered at frame 0's state
//!   before the scenario starts; between warm-up frames the harness waits for
//!   the GPU and for every pending pipeline compile **without rendering**, so
//!   the number of `render()` calls before frame k never depends on compile
//!   timing (the temporal passes, SSR and volumetrics, key off it).
//! - Size: the canvas backing store is set explicitly (default 960×720);
//!   `devicePixelRatio` never enters.

use std::collections::HashMap;

use anyhow::{anyhow, Context, Result};
use awsm_renderer::camera::{view_from_world, CameraParams};
use awsm_renderer::features::RendererFeatures;
use awsm_renderer::AwsmRenderer;
use awsm_renderer_core::configuration::{CanvasAlphaMode, CanvasConfiguration};
use awsm_renderer_core::renderer::{AwsmRendererWebGpuBuilder, DeviceRequestLimits};
use awsm_renderer_core::texture::exporter::TextureReadback;
use awsm_renderer_scene::{EditorNode, NodeKind};
use awsm_renderer_scene_loader::{LoadedScene, NodeHandles, ParticleDriver};
use glam::{Mat4, Vec2, Vec3};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::metrics::Image;
use crate::scenario::{self, FrameDrive, Scenario, SceneSetup};

/// The anti-aliasing a session renders with (overrides the bundle's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aa {
    None,
    Msaa,
    Smaa,
    /// TAA at native resolution (needs the `taa-hooks` build).
    Taa,
}

impl Aa {
    pub fn parse(s: &str) -> Option<Aa> {
        match s {
            "none" | "noaa" | "off" => Some(Aa::None),
            "msaa" | "msaa4" => Some(Aa::Msaa),
            "smaa" => Some(Aa::Smaa),
            "taa" => Some(Aa::Taa),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Aa::None => "none",
            Aa::Msaa => "msaa",
            Aa::Smaa => "smaa",
            Aa::Taa => "taa",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionOpts {
    pub scene: String,
    pub aa: Aa,
    pub size: (u32, u32),
    pub origin: String,
    pub warmup: u32,
}

#[allow(dead_code)] // some accessors serve only the taa-hooks build / later gates
pub struct Session {
    pub renderer: AwsmRenderer,
    pub scenario: Scenario,
    pub setup: SceneSetup,
    pub opts: SessionOpts,
    /// Next scenario frame to render.
    pub next_k: u32,
    /// The drive of the last rendered scenario frame.
    pub last_drive: Option<FrameDrive>,
    /// `renderer.frame_index` when scenario frame 0 was rendered.
    pub frame_index_at_k0: u64,
    /// `renderer.frame_index` just before the last NORMAL (non-frozen)
    /// render. Frozen re-renders restore it, so every frame-index cadence
    /// (skin `skin_update_period`, coverage) sees, at scenario frame k, the
    /// same index in every session however many frozen re-renders (64-phase
    /// references, stop / flicker holds) a session interleaves.
    frame_index_base: u64,
    /// The scenario frame of the last normal render.
    last_normal_k: Option<u32>,
    handles: HashMap<String, NodeHandles>,
    /// Every named node (duplicate names kept), authored order.
    all_nodes: Vec<(String, NodeHandles)>,
    particles: ParticleDriver,
    /// Each session owns its canvas (several can run in lockstep).
    canvas_id: String,
    _loaded: LoadedScene,
}

#[allow(dead_code)]
impl Session {
    pub async fn open(opts: SessionOpts) -> Result<Session> {
        // `<scene>@<variant>`: the same bundle under a variant setup
        // (`@cadence`: coverage LOD on + skins at update periods > 1).
        let base_scene = base_scene_name(&opts.scene).to_string();
        let bundle_base = format!("{}/{}/bundle", opts.origin, base_scene);
        let mut scene = fetch_scene(&bundle_base).await?;
        // The requested AA, applied the way the loader applies an authored one
        // (before the commit, so the right variants compile in one batch).
        scene.post_process.anti_aliasing.msaa = opts.aa == Aa::Msaa;
        scene.post_process.anti_aliasing.smaa = opts.aa == Aa::Smaa;
        // Trees whose scene schema carries the TAA choice: the loader applies
        // the authored AA, so the bundle's `taa: None` would switch off the
        // TAA configured on the builder. Author it here too.
        #[cfg(feature = "taa-schema")]
        {
            scene.post_process.anti_aliasing.taa =
                (opts.aa == Aa::Taa).then(awsm_renderer_scene::TaaSettings::default);
        }

        let canvas_id = format!("render-canvas-{}", next_canvas_serial());
        let mut renderer = create_renderer(
            &canvas_id,
            opts.size,
            aa_config(opts.aa)?,
            features_for(&opts.scene),
        )
        .await?;
        // `?fallback&fallback-early`: switch to the fallback (and its velocity
        // overlay) BEFORE the scene loads, so every material — vertex hooks
        // included — is registered after the overlay exists (its hook variants
        // then compile at the load's commit, not at the overlay's build).
        #[cfg(feature = "taa-fb-overlay")]
        if opts.aa == Aa::Taa
            && crate::hooks::fallback()
            && crate::url_value("fallback-early").is_some()
        {
            use awsm_renderer::anti_alias::TaaVelocityPath;
            renderer.set_taa_velocity_path_override(Some(TaaVelocityPath::PrepReconstruction));
            renderer.set_taa_fallback_storage_limit_override(
                crate::url_value("fallback-storage").and_then(|v| v.parse().ok()),
            );
            renderer.set_taa_debug_overlay_check(crate::overlay::debug_check());
            renderer
                .set_anti_aliasing(aa_config(Aa::Taa)?)
                .await
                .map_err(|e| anyhow!("set_anti_aliasing (fallback-early): {e}"))?;
            if renderer.taa_overlay_draw_count() != 0 {
                return Err(anyhow!(
                    "fallback-early: overlay draw set not empty before the load"
                ));
            }
        }

        let assets = awsm_renderer_scene_loader::assets::HttpAssets::new(bundle_base);
        let loaded = awsm_renderer_scene_loader::load_scene_for_player(
            &mut renderer,
            &scene,
            &assets,
            |_| {},
        )
        .await
        .map_err(|e| anyhow!("load_scene_for_player({}): {e:#}", opts.scene))?;
        renderer.update_transforms();
        check_aa(&renderer, opts.aa)?;
        // The loader applies the scene's authored AA (with the schema, its
        // TAA settings: DEFAULT tuning and native scale), which would silently
        // replace the harness's `?tune=` / `?scale=` config. Re-apply it and
        // require the renderer to hold exactly the requested TAA config.
        #[cfg(feature = "taa-hooks")]
        if opts.aa == Aa::Taa {
            let want = crate::hooks::taa_native();
            if renderer.anti_aliasing.taa != want.taa {
                renderer
                    .set_anti_aliasing(want.clone())
                    .await
                    .map_err(|e| anyhow!("set_anti_aliasing (tuning): {e}"))?;
                renderer
                    .commit_load(|_| {})
                    .await
                    .map_err(|e| anyhow!("commit_load after the tuning: {e}"))?;
            }
            if renderer.anti_aliasing.taa != want.taa {
                return Err(anyhow!(
                    "TAA config after load is {:?}, the harness asked for {:?}",
                    renderer.anti_aliasing.taa,
                    want.taa
                ));
            }
            #[cfg(feature = "taa-experimental")]
            renderer.set_taa_experimental_tuning(crate::hooks::experimental());
            // `?particles_alpha`: particles write an alpha-scaled reactive
            // value (the comparison for `reactive_particles`).
            #[cfg(feature = "taa-disocclusion")]
            renderer.set_taa_particles_alpha_scaled(crate::url_value("particles_alpha").is_some());
            // Phase 5: `?fallback` forces the prep-velocity reconstruction
            // path (the 32 B/sample fallback) on a device that has the
            // geometry path.
            #[cfg(feature = "taa-phase5")]
            if want.taa.is_some() && crate::hooks::fallback() {
                use awsm_renderer::anti_alias::TaaVelocityPath;
                renderer.set_taa_velocity_path_override(Some(TaaVelocityPath::PrepReconstruction));
                // `?fallback-storage=N`: the storage-buffer limit behind the
                // overlay's coverage (7-9: vertex hooks stay reactive; < 7:
                // no overlay); `overlay-check` builds its debug variant.
                #[cfg(feature = "taa-fb-overlay")]
                {
                    renderer.set_taa_fallback_storage_limit_override(
                        crate::url_value("fallback-storage").and_then(|v| v.parse().ok()),
                    );
                    renderer.set_taa_debug_overlay_check(crate::overlay::debug_check());
                }
                renderer
                    .set_anti_aliasing(want.clone())
                    .await
                    .map_err(|e| anyhow!("set_anti_aliasing (fallback): {e}"))?;
                renderer
                    .commit_load(|_| {})
                    .await
                    .map_err(|e| anyhow!("commit_load (fallback): {e}"))?;
                if renderer.taa_velocity_path() != TaaVelocityPath::PrepReconstruction {
                    return Err(anyhow!(
                        "fallback requested, velocity path is {:?}",
                        renderer.taa_velocity_path()
                    ));
                }
            }
            // A silent fall-back from TAA to MSAA would invalidate a TAA gate.
            #[cfg(feature = "taa-phase4")]
            if let Some(reason) = renderer.taa_fallback_reason() {
                return Err(anyhow!("TAA fell back to MSAA: {reason}"));
            }
        }

        // Name → handles (first node of a name wins; scripts use unique names).
        let mut names: Vec<(String, awsm_renderer_scene::NodeId)> = Vec::new();
        collect_names(&scene.nodes, &mut names);
        let mut handles = HashMap::new();
        let all_nodes: Vec<(String, NodeHandles)> = names
            .iter()
            .filter_map(|(n, id)| loaded.nodes.get(id).map(|h| (n.clone(), h.clone())))
            .collect();
        let mut setup = SceneSetup::default();
        for (name, id) in &names {
            if let Some(h) = loaded.nodes.get(id) {
                if !handles.contains_key(name) {
                    if let Ok(local) = renderer.transforms.get_local(h.transform) {
                        setup.nodes.insert(name.clone(), local.clone());
                    }
                    if let Some(cfg) = h.camera_config.as_ref() {
                        let world = *renderer
                            .transforms
                            .get_world(h.transform)
                            .map_err(|e| anyhow!("camera world: {e}"))?;
                        let params: CameraParams = h
                            .camera
                            .and_then(|key| renderer.cameras.get(key))
                            .copied()
                            .unwrap_or_else(|| {
                                awsm_renderer_scene_loader::camera::camera_params_from_config(cfg)
                            });
                        setup.cameras.push((name.clone(), world, params));
                    }
                    handles.insert(name.clone(), h.clone());
                }
            }
        }
        let (center, radius) = scene_bounds(&renderer);
        setup.bounds_center = center;
        setup.bounds_radius = radius;
        // Bounds of what is actually drawn (excludes hidden prefab templates).
        let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
        for (_, m) in renderer.meshes.iter() {
            if let (false, Some(b)) = (m.hidden, m.world_aabb.as_ref()) {
                lo = lo.min(b.min);
                hi = hi.max(b.max);
            }
        }
        if lo.is_finite() && hi.is_finite() {
            setup.visible_center = (lo + hi) * 0.5;
            setup.visible_radius = ((hi - lo).length() * 0.5).max(0.05);
        } else {
            setup.visible_center = center;
            setup.visible_radius = radius;
        }

        let scenario = if opts.scene.ends_with("@popin") {
            scenario::popin_scenario(&base_scene)
        } else if opts.scene.ends_with("@popjump") {
            scenario::popjump_scenario(&base_scene)
        } else if opts.scene.ends_with("@thin") || opts.scene.ends_with("@thinstill") {
            scenario::bloom_thin_scenario(&base_scene, opts.scene.ends_with("@thinstill"))
        } else {
            scenario::scenario_for(&base_scene)
        };
        if opts.scene.ends_with("@cadence") {
            // Skin cadence gating: every skinned mesh at period 2, 4, 1, …
            // (round-robin) so some skins step every 2nd / 4th frame.
            let skinned: Vec<awsm_renderer::meshes::MeshKey> = renderer
                .meshes
                .iter()
                .filter(|(k, _)| matches!(renderer.meshes.mesh_skin_key(*k), Some(Some(_))))
                .map(|(k, _)| k)
                .collect();
            for (i, k) in skinned.iter().enumerate() {
                let period = [2u8, 4, 1][i % 3];
                renderer
                    .set_mesh_skin_update_period(*k, period)
                    .map_err(|e| anyhow!("set_mesh_skin_update_period: {e}"))?;
            }
        }
        for hud in &scenario.hud_nodes {
            let h = handles
                .get(*hud)
                .ok_or_else(|| anyhow!("scenario HUD node `{hud}` not in scene"))?;
            for mesh in &h.meshes {
                renderer
                    .set_mesh_hud(*mesh, true)
                    .map_err(|e| anyhow!("set_mesh_hud: {e}"))?;
            }
        }
        // Emissive gains (`bloom-post@thin`), once at load.
        for (name, gain) in &scenario.emissive_gain {
            let h = handles
                .get(*name)
                .ok_or_else(|| anyhow!("scenario emissive node `{name}` not in scene"))?;
            for mesh in &h.meshes {
                let key = renderer
                    .meshes
                    .get(*mesh)
                    .map_err(|e| anyhow!("mesh {name}: {e}"))?
                    .material_key;
                renderer.update_material(key, |m| {
                    if let awsm_renderer::materials::Material::Pbr(p) = m {
                        for c in p.emissive_factor.iter_mut() {
                            *c *= *gain;
                        }
                    }
                });
            }
        }
        let particles = ParticleDriver::new(&loaded);

        let mut s = Session {
            renderer,
            scenario,
            setup,
            opts,
            next_k: 0,
            last_drive: None,
            frame_index_at_k0: 0,
            frame_index_base: 0,
            last_normal_k: None,
            handles,
            all_nodes,
            particles,
            canvas_id,
            _loaded: loaded,
        };
        s.warm_up().await?;
        Ok(s)
    }

    /// Sessions compared in lockstep with a session that interleaves frozen
    /// re-renders must not depend on the number of renders (05 review item
    /// 9). Frame-index cadences are pinned (see `frame_index_base`); what
    /// keys on the renderer's internal render counters (SSR temporal,
    /// volumetric temporal: `render_textures.frame_count`, ping-pong) or on
    /// asynchronous GPU readbacks (coverage LOD) cannot be, so it is refused.
    pub fn assert_lockstep_safe(&self) -> Result<()> {
        let pp = &self.renderer.post_processing;
        let mut bad = Vec::new();
        if pp.ssr.enabled && pp.ssr.temporal {
            bad.push("SSR temporal");
        }
        if pp.atmosphere.volumetric_temporal {
            bad.push("volumetric temporal");
        }
        if self.renderer.features.coverage_lod {
            bad.push("coverage_lod (asynchronous readback)");
        }
        if bad.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(
                "{}: not lockstep-safe ({}): the reference session's extra frozen renders would change it",
                self.opts.scene,
                bad.join(", ")
            ))
        }
    }

    /// Exactly `warmup` renders of frame 0's state, waiting (without
    /// rendering) for the GPU and every pending compile between them.
    async fn warm_up(&mut self) -> Result<()> {
        for _ in 0..self.opts.warmup {
            self.apply_frame(0, 0.0, None, false)?;
            self.renderer
                .render(None)
                .map_err(|e| anyhow!("render: {e}"))?;
            gpu_idle(&self.renderer).await;
            self.settle_compiles(10_000).await?;
        }
        Ok(())
    }

    /// Poll the pipeline scheduler (no render) until nothing is compiling.
    pub async fn settle_compiles(&mut self, max_ms: u32) -> Result<()> {
        let start = crate::now_ms();
        loop {
            self.renderer.poll_pipeline_scheduler();
            let p = self.renderer.compile_progress();
            if p.materials_pending == 0 && p.in_flight_subcompiles == 0 {
                return Ok(());
            }
            if crate::now_ms() - start > max_ms as f64 {
                return Err(anyhow!(
                    "pipelines still compiling after {max_ms} ms ({} pending, {} sub-compiles)",
                    p.materials_pending,
                    p.in_flight_subcompiles
                ));
            }
            sleep_ms(8).await;
        }
    }

    /// Set the scene to frame `k`'s state: time, animations (+`dt_ms`),
    /// particles, scripted transforms, camera, optional jitter override.
    ///
    /// `frozen`: a re-render of the last normally rendered frame (time
    /// frozen). It must re-render exactly that frame (so it can never step
    /// into a scripted cut: a cut only resets on its normal render), and it
    /// restores `renderer.frame_index` (see `frame_index_base`).
    fn apply_frame(
        &mut self,
        k: u32,
        dt_ms: f64,
        jitter: Option<Vec2>,
        frozen: bool,
    ) -> Result<FrameDrive> {
        if frozen {
            if self.last_normal_k != Some(k) {
                return Err(anyhow!(
                    "frozen re-render of k{k}, but the last normal render was {:?}",
                    self.last_normal_k
                ));
            }
            self.renderer.frame_index = self.frame_index_base;
        } else {
            self.frame_index_base = self.renderer.frame_index;
            self.last_normal_k = Some(k);
        }
        let r = &mut self.renderer;
        r.set_time_source(scenario::time_at(k) as f32);
        r.update_animations(dt_ms)
            .map_err(|e| anyhow!("update_animations: {e}"))?;
        self.particles.tick(r, (dt_ms / 1000.0) as f32);
        let drive = scenario::drive(&self.scenario, k, &self.setup);
        // Pop-ins: hidden until their frame, then shown (transitions only, so
        // a frozen re-render changes nothing).
        for (name, at) in &self.scenario.appear {
            let h = self
                .handles
                .get(*name)
                .ok_or_else(|| anyhow!("scenario pop-in node `{name}` not in scene"))?;
            for mesh in &h.meshes {
                let hidden = k < *at;
                if r.meshes.get(*mesh).is_ok_and(|m| m.hidden != hidden) {
                    r.set_mesh_hidden(*mesh, hidden)
                        .map_err(|e| anyhow!("set_mesh_hidden {name}: {e}"))?;
                }
            }
        }
        for (name, tr) in &drive.transforms {
            let h = self
                .handles
                .get(name)
                .ok_or_else(|| anyhow!("scenario node `{name}` not in scene"))?;
            r.transforms
                .set_local(h.transform, tr.clone())
                .map_err(|e| anyhow!("set_local {name}: {e}"))?;
        }
        // Rescaled nodes (`bloom-post@thin`), every frame.
        for (name, scale) in &self.scenario.rescaled {
            let h = self
                .handles
                .get(*name)
                .ok_or_else(|| anyhow!("scenario rescaled node `{name}` not in scene"))?;
            let base = self.setup.base(name);
            r.transforms
                .set_local(
                    h.transform,
                    awsm_renderer::transforms::Transform {
                        scale: *scale,
                        ..base
                    },
                )
                .map_err(|e| anyhow!("set_local {name}: {e}"))?;
        }
        // One-frame displacements (`@popjump`).
        for (name, at, offset) in &self.scenario.displaced {
            if k != *at {
                continue;
            }
            let h = self
                .handles
                .get(*name)
                .ok_or_else(|| anyhow!("scenario displaced node `{name}` not in scene"))?;
            let base = drive
                .transforms
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, t)| t.clone())
                .unwrap_or_else(|| self.setup.base(name));
            r.transforms
                .set_local(
                    h.transform,
                    awsm_renderer::transforms::Transform {
                        translation: base.translation + *offset,
                        ..base
                    },
                )
                .map_err(|e| anyhow!("set_local {name}: {e}"))?;
        }
        for (name, weights) in &drive.morphs {
            let h = self
                .handles
                .get(name)
                .ok_or_else(|| anyhow!("scenario morph node `{name}` not in scene"))?;
            for mesh in &h.meshes {
                if let Some(key) = r.meshes.geometry_morph_key_for_mesh(*mesh) {
                    r.meshes
                        .morphs
                        .geometry
                        .update_morph_weights_with(key, |w| {
                            for (dst, src) in w.iter_mut().zip(weights) {
                                *dst = *src;
                            }
                        })
                        .map_err(|e| anyhow!("morph weights {name}: {e}"))?;
                }
            }
        }
        #[cfg(feature = "taa-hooks")]
        {
            crate::hooks::set_jitter(r, jitter);
            if drive.cut && dt_ms > 0.0 {
                r.reset_temporal_history();
            }
            // A teleported node has no meaningful previous transform (G10).
            #[cfg(feature = "taa-phase1")]
            if dt_ms > 0.0 {
                for name in &drive.teleports {
                    if let Some(h) = self.handles.get(name) {
                        r.reset_motion_history(h.transform);
                    }
                }
            }
        }
        #[cfg(not(feature = "taa-hooks"))]
        if jitter.is_some() {
            return Err(anyhow!("jitter override needs the taa-hooks build"));
        }
        r.update_transforms();
        r.set_camera(view_from_world(drive.camera_world), drive.camera_params)
            .map_err(|e| anyhow!("set_camera: {e}"))?;
        Ok(drive)
    }

    /// Render the next scenario frame.
    pub fn render_next(&mut self) -> Result<u32> {
        self.render_next_jittered(None)
    }

    /// [`Self::render_next`] with a camera-jitter override for this frame.
    pub fn render_next_jittered(&mut self, jitter: Option<Vec2>) -> Result<u32> {
        let k = self.next_k;
        let dt = if k == 0 { 0.0 } else { scenario::DT_MS };
        if k == 0 {
            self.frame_index_at_k0 = self.renderer.frame_index;
        }
        let drive = self.apply_frame(k, dt, jitter, false)?;
        self.renderer
            .render(None)
            .map_err(|e| anyhow!("render k={k}: {e}"))?;
        self.last_drive = Some(drive);
        self.next_k += 1;
        Ok(k)
    }

    /// Re-render the last scenario frame with time frozen (dt = 0), with an
    /// optional camera-jitter override (render pixels). The reference capture.
    pub fn render_frozen(&mut self, jitter: Option<Vec2>) -> Result<()> {
        let k = self
            .next_k
            .checked_sub(1)
            .ok_or_else(|| anyhow!("render_frozen before any frame"))?;
        let drive = self.apply_frame(k, 0.0, jitter, true)?;
        self.renderer
            .render(None)
            .map_err(|e| anyhow!("render frozen k={k}: {e}"))?;
        // Leave the override off for whatever renders next.
        #[cfg(feature = "taa-hooks")]
        crate::hooks::set_jitter(&mut self.renderer, None);
        self.last_drive = Some(drive);
        Ok(())
    }

    /// Render frames until frame `k` has been rendered (no captures).
    pub async fn play_to(&mut self, k: u32) -> Result<()> {
        while self.next_k <= k {
            self.render_next()?;
            gpu_idle(&self.renderer).await;
        }
        Ok(())
    }

    /// Snapshot the frame just rendered (must be called right after a render,
    /// before yielding: the swap-chain texture is only valid until present).
    pub fn begin_capture(&self) -> Result<TextureReadback> {
        let gpu = &self.renderer.gpu;
        let texture = gpu
            .current_context_texture()
            .map_err(|e| anyhow!("current texture: {e}"))?;
        let (w, h) = gpu
            .current_context_texture_size()
            .map_err(|e| anyhow!("texture size: {e}"))?;
        let format = gpu.current_context_format();
        Ok(gpu
            .copy_texture_for_readback(&texture, w, h, 0, format)
            .map_err(|e| anyhow!("copy_texture_for_readback: {e}"))?
            // The display pass already sRGB-encodes into the non-sRGB canvas.
            .mark_display_encoded())
    }

    /// Render the next frame and capture it.
    pub async fn render_next_captured(&mut self) -> Result<(u32, Image)> {
        let k = self.render_next()?;
        let rb = self.begin_capture()?;
        Ok((k, finish_capture(rb, self.opts.size).await?))
    }

    /// Frozen re-render + capture.
    pub async fn render_frozen_captured(&mut self, jitter: Option<Vec2>) -> Result<Image> {
        self.render_frozen(jitter)?;
        let rb = self.begin_capture()?;
        finish_capture(rb, self.opts.size).await
    }

    /// World matrix of a named node (for analytic coverage).
    pub fn node_world(&self, name: &str) -> Option<Mat4> {
        let h = self.handles.get(name)?;
        self.renderer
            .transforms
            .get_world(h.transform)
            .ok()
            .copied()
    }

    /// The current camera's (unjittered) view-projection.
    pub fn view_projection(&self) -> Option<Mat4> {
        self.renderer.camera_matrices().map(|m| m.view_projection())
    }

    /// World-space corners of a box of `half_extents` centred on a named
    /// node's origin (the tracked objects are authored as boxes, so this is
    /// their exact hull for analytic coverage).
    pub fn node_box_corners(&self, name: &str, half_extents: Vec3) -> Option<Vec<Vec3>> {
        let world = self.node_world(name)?;
        Some(
            (0..8)
                .map(|i| {
                    let l = Vec3::new(
                        if i & 1 == 0 {
                            -half_extents.x
                        } else {
                            half_extents.x
                        },
                        if i & 2 == 0 {
                            -half_extents.y
                        } else {
                            half_extents.y
                        },
                        if i & 4 == 0 {
                            -half_extents.z
                        } else {
                            half_extents.z
                        },
                    );
                    world.transform_point3(l)
                })
                .collect(),
        )
    }

    /// Handles of every node with this name (duplicates included).
    pub fn nodes_named(&self, name: &str) -> Vec<NodeHandles> {
        self.all_nodes
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, h)| h.clone())
            .collect()
    }

    /// Handles of a named node (the first of that name).
    pub fn node(&self, name: &str) -> Option<&NodeHandles> {
        self.handles.get(name)
    }

    /// Hide (for the rest of the session) the content that has no motion
    /// vectors by design (03 §5, phase-3 reactive content): particle
    /// emitters, fat lines, and the named nodes `extra` (glass, UV-flow
    /// water). Returns the names hidden.
    pub fn hide_no_vector_content(&mut self, extra: &[&str]) -> Vec<String> {
        let mut hidden = Vec::new();
        let mut names: Vec<String> = self.handles.keys().cloned().collect();
        names.sort();
        for name in names {
            let h = self.handles[&name].clone();
            let by_kind = h.emitter.is_some() || h.line.is_some();
            if !(by_kind || extra.contains(&name.as_str())) {
                continue;
            }
            for m in &h.meshes {
                let _ = self.renderer.set_mesh_hidden(*m, true);
            }
            if let Some(e) = &h.emitter {
                let _ = self.renderer.set_mesh_hidden(e.mesh, true);
            }
            if let Some(line) = h.line {
                self.renderer.remove_line(line);
            }
            hidden.push(name);
        }
        hidden
    }

    /// Hide (for the rest of the session) every node named in `names` (all
    /// nodes of that name). Returns the names that matched.
    pub fn hide_named(&mut self, names: &[&str]) -> Vec<String> {
        let mut hidden = Vec::new();
        for name in names {
            let nodes = self.nodes_named(name);
            if nodes.is_empty() {
                continue;
            }
            for h in nodes {
                for m in &h.meshes {
                    let _ = self.renderer.set_mesh_hidden(*m, true);
                }
                if let Some(e) = &h.emitter {
                    let _ = self.renderer.set_mesh_hidden(e.mesh, true);
                }
                if let Some(line) = h.line {
                    self.renderer.remove_line(line);
                }
            }
            hidden.push(name.to_string());
        }
        hidden
    }

    /// Morph weights of every morphed mesh of every named node (diagnostics:
    /// proves a morph clip is actually driving the mesh).
    pub fn morph_weights(&self) -> Vec<(String, Vec<f32>)> {
        let mut out = Vec::new();
        let mut names: Vec<&String> = self.handles.keys().collect();
        names.sort();
        for name in names {
            for mesh in &self.handles[name].meshes {
                if let Some(key) = self.renderer.meshes.geometry_morph_key_for_mesh(*mesh) {
                    if let Ok(w) = self.renderer.meshes.morphs.geometry.read_morph_weights(key) {
                        out.push((name.clone(), w));
                    }
                }
            }
        }
        out
    }

    pub fn destroy(self) {
        self.renderer.gpu.device.destroy();
        let doc = web_sys::window().and_then(|w| w.document());
        if let Some(c) = doc.and_then(|d| d.get_element_by_id(&self.canvas_id)) {
            c.remove();
        }
    }
}

fn next_canvas_serial() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SERIAL: AtomicU32 = AtomicU32::new(0);
    SERIAL.fetch_add(1, Ordering::Relaxed)
}

/// Await the readback and check its size.
pub async fn finish_capture(rb: TextureReadback, size: (u32, u32)) -> Result<Image> {
    let rgba = rb
        .finish_rgba8()
        .await
        .map_err(|e| anyhow!("readback: {e}"))?;
    let mut img = Image::from_rgba(size.0, size.1, rgba);
    // Swap-chain alpha is don't-care (opaque canvas): normalise it.
    for px in img.rgba.chunks_exact_mut(4) {
        px[3] = 255;
    }
    Ok(img)
}

/// Wait until the GPU has finished everything submitted so far.
pub async fn gpu_idle(renderer: &AwsmRenderer) {
    let _ = JsFuture::from(renderer.gpu.device.queue().on_submitted_work_done()).await;
}

/// After selecting a TAA debug view: wait until the resolve variant with the
/// debug views is ready (it compiles on first selection; until then the view
/// would silently show the normal image). Immediate for `FreezeJitter` and
/// `None` (id 0) and on renderers without the lazy variant.
pub async fn wait_taa_debug_variant(renderer: &AwsmRenderer) {
    #[cfg(feature = "taa-debug-variant")]
    for _ in 0..4000 {
        if renderer.taa_debug_variant_ready() {
            return;
        }
        sleep_ms(5).await;
    }
    #[cfg(not(feature = "taa-debug-variant"))]
    let _ = renderer;
}

pub async fn sleep_ms(ms: i32) {
    let p = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(p).await;
}

/// Hard check after load: the renderer runs exactly the AA the session asked
/// for. A TAA gate that silently ran with TAA off (or MSAA on) must be
/// impossible; every session fails to open instead.
fn check_aa(renderer: &AwsmRenderer, aa: Aa) -> Result<()> {
    let a = &renderer.anti_aliasing;
    let msaa = a.msaa_sample_count.is_some();
    #[cfg(feature = "taa-hooks")]
    let taa = a.taa.is_some();
    #[cfg(not(feature = "taa-hooks"))]
    let taa = false;
    let ok = match aa {
        Aa::None => !msaa && !a.smaa && !taa,
        Aa::Msaa => msaa && !a.smaa && !taa,
        Aa::Smaa => !msaa && a.smaa && !taa,
        Aa::Taa => !msaa && !a.smaa && taa,
    };
    if ok {
        Ok(())
    } else {
        Err(anyhow!(
            "AA after load is msaa={msaa} smaa={} taa={taa}, session asked for {} — refusing to measure",
            a.smaa,
            aa.name()
        ))
    }
}

/// The renderer AA settings for a mode. With the TAA hooks (any tree after
/// 0.45) these are the explicit constructors: never `..Default::default()`,
/// because the default AA mode will become TAA and MSAA + TAA is rejected.
#[cfg(feature = "taa-hooks")]
fn aa_config(aa: Aa) -> Result<awsm_renderer::anti_alias::AntiAliasing> {
    use awsm_renderer::anti_alias::AntiAliasing;
    Ok(match aa {
        Aa::None => AntiAliasing::none(),
        Aa::Msaa => AntiAliasing::msaa4(),
        Aa::Smaa => AntiAliasing::smaa(),
        Aa::Taa => crate::hooks::taa_native(),
    })
}

/// The 0.45 release has no constructors and no TAA: its `Default` is MSAA 4×.
#[cfg(not(feature = "taa-hooks"))]
fn aa_config(aa: Aa) -> Result<awsm_renderer::anti_alias::AntiAliasing> {
    if aa == Aa::Taa {
        return Err(anyhow!("aa=taa needs the taa-hooks build"));
    }
    Ok(awsm_renderer::anti_alias::AntiAliasing {
        msaa_sample_count: (aa == Aa::Msaa).then_some(4),
        smaa: aa == Aa::Smaa,
        mipmap: true,
    })
}

/// Renderer features per scene: the player defaults, except where a scene
/// only exercises its subject with a feature gate on (the same gates
/// examples/player-tests uses: discrete LOD, cluster LOD + streaming).
/// The bundle name of a `<scene>@<variant>` session name.
pub fn base_scene_name(scene: &str) -> &str {
    scene.split('@').next().unwrap_or(scene)
}

pub fn features_for(scene: &str) -> RendererFeatures {
    if scene.ends_with("@cadence") {
        // The per-mesh skin update cadence (layer 1 of the skin gate) needs
        // no feature: only `skin_update_period` > 1. Coverage LOD (layer 2)
        // stays off: its GPU readback lands on timing-dependent frames.
        return RendererFeatures::default();
    }
    match scene {
        "lod-classic" => RendererFeatures {
            lod: true,
            gpu_culling: true,
            ..Default::default()
        },
        "lod-nanite" | "lod-nanite-open" | "taa-cluster" => RendererFeatures {
            virtual_geometry: true,
            cluster_streaming: true,
            gpu_culling: true,
            ..Default::default()
        },
        _ => RendererFeatures::default(),
    }
}

async fn create_renderer(
    canvas_id: &str,
    (w, h): (u32, u32),
    aa: awsm_renderer::anti_alias::AntiAliasing,
    features: RendererFeatures,
) -> Result<AwsmRenderer> {
    let window = web_sys::window().ok_or_else(|| anyhow!("no window"))?;
    let document = window.document().ok_or_else(|| anyhow!("no document"))?;
    let canvas: web_sys::HtmlCanvasElement = document
        .create_element("canvas")
        .map_err(|e| anyhow!("create canvas: {e:?}"))?
        .dyn_into()
        .map_err(|_| anyhow!("canvas cast"))?;
    canvas.set_id(canvas_id);
    // The backing store IS the render size; CSS size is cosmetic.
    canvas.set_width(w);
    canvas.set_height(h);
    let _ = canvas.set_attribute("style", "width: 480px; height: auto;");
    document
        .body()
        .ok_or_else(|| anyhow!("no body"))?
        .append_child(&canvas)
        .map_err(|e| anyhow!("append canvas: {e:?}"))?;

    let gpu = window.navigator().gpu();
    let gpu_builder = AwsmRendererWebGpuBuilder::new(gpu, canvas)
        .with_configuration(
            CanvasConfiguration::default()
                .with_alpha_mode(CanvasAlphaMode::Opaque)
                // COPY_SRC: the swap chain is read back every captured frame.
                .with_usage(
                    awsm_renderer_core::texture::TextureUsage::new()
                        .with_render_attachment()
                        .with_copy_src(),
                ),
        )
        .with_device_request_limits(DeviceRequestLimits::max_all());
    awsm_renderer::AwsmRendererBuilder::new(gpu_builder)
        .with_features(features)
        .with_anti_aliasing(aa)
        .build()
        .await
        .map_err(|e| anyhow!("renderer build: {e}"))
}

async fn fetch_scene(bundle_base: &str) -> Result<awsm_renderer_scene::Scene> {
    let url = format!("{bundle_base}/scene.toml");
    let resp = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| anyhow!("fetch {url}: {e}"))?;
    if !resp.ok() {
        return Err(anyhow!("fetch {url}: HTTP {}", resp.status()));
    }
    let text = resp.text().await.map_err(|e| anyhow!("read {url}: {e}"))?;
    awsm_renderer_scene::project_dir::scene_from_toml(&text).with_context(|| format!("parse {url}"))
}

fn collect_names(nodes: &[EditorNode], out: &mut Vec<(String, awsm_renderer_scene::NodeId)>) {
    for n in nodes {
        out.push((n.name.clone(), n.id));
        let _ = matches!(n.kind, NodeKind::Camera(_));
        collect_names(&n.children, out);
    }
}

fn scene_bounds(renderer: &AwsmRenderer) -> (Vec3, f32) {
    let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for node in renderer.scene_spatial.iter_all() {
        lo = lo.min(node.aabb.min);
        hi = hi.max(node.aabb.max);
    }
    if !lo.is_finite() || !hi.is_finite() {
        lo = Vec3::splat(-4.0);
        hi = Vec3::splat(4.0);
    }
    ((lo + hi) * 0.5, ((hi - lo).length() * 0.5).max(1.0))
}
