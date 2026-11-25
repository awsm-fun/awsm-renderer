//! Post-processing configuration and updates.

use crate::{error::Result, AwsmRenderer};

/// Post-processing settings for the renderer.
#[derive(Clone, Debug, PartialEq)]
pub struct PostProcessing {
    pub tonemapping: ToneMapping,
    pub bloom: bool,
    pub dof: bool,
    /// Pre-tonemap scene exposure in EV (stops). 0 = unity, +1 = 2x as
    /// bright, -1 = half as bright. Lets the user pull authored
    /// photometric intensities (candela-scale gltf lights) into a range
    /// the tonemapper can resolve without saturating. The renderer
    /// doesn't try to convert lumens→watts; this is the user-facing
    /// knob that compensates.
    pub exposure: f32,
    /// Bloom brightness threshold (in pre-exposure HDR luminance): pixels
    /// brighter than this contribute to the glow. LIVE uniform on the bloom
    /// pyramid pass (no shader recompile on change).
    pub bloom_threshold: f32,
    /// Bloom soft-knee width — the smooth ramp below `bloom_threshold` so the
    /// glow fades in rather than hard-cutting. LIVE uniform.
    pub bloom_knee: f32,
    /// Bloom mix strength — how strongly the accumulated glow is added over the
    /// scene. LIVE uniform.
    pub bloom_intensity: f32,
    /// Bloom scatter — biases the mip-sum toward the coarser (wider) levels, so
    /// higher = a broader, softer halo. LIVE uniform.
    pub bloom_scatter: f32,
    /// Screen-space reflections. See [`Ssr`]. `enabled = false` (default)
    /// records no pass + allocates no targets.
    pub ssr: Ssr,
    /// Atmospheric haze. See [`Atmosphere`]. `enabled = false` (default)
    /// doesn't compile the fog term into the effects shader.
    pub atmosphere: Atmosphere,
}

impl Eq for PostProcessing {}

/// Runtime screen-space-reflection settings (mirrors
/// `awsm_renderer_scene::post_process::SsrConfig`). Reflectance is
/// material-owned (each material writes a `{mask, spread, tint}` descriptor);
/// this carries only the pass-level knobs. The **structural** fields
/// (`enabled`, `temporal`, `resolution_scale`) select compiled shader variants
/// and force a recompile; every other field is a LIVE uniform.
#[derive(Clone, Debug, PartialEq)]
pub struct Ssr {
    pub enabled: bool,
    pub intensity: f32,
    pub max_distance: f32,
    pub thickness: f32,
    pub max_steps: u32,
    /// Skip SSR above this reflection spread (0 mirror … 1 diffuse) → IBL.
    pub spread_cutoff: f32,
    pub edge_fade: f32,
    /// 0.5 = half-res + upsample, 1.0 = full. Structural (recompiles).
    pub resolution_scale: f32,
    /// Temporal accumulation. Structural (recompiles).
    pub temporal: bool,
    pub temporal_weight: f32,
    /// Debug visualization (0 off, 1 confidence, 2 travel, 3 source,
    /// 4 traversal steps). Structural (recompiles the trace variant).
    pub debug: u32,
    /// Software-BVH reflections for SSR misses on near-mirror pixels
    /// (architecture in the bvh.rs module docs). Structural (recompiles the trace +
    /// builds the bvh_trace pass). Default OFF — a high-end tier.
    pub bvh_reflections: bool,
}

impl Default for Ssr {
    fn default() -> Self {
        Self {
            enabled: false,
            intensity: 1.0,
            max_distance: 100.0,
            thickness: 1.0,
            max_steps: 96,
            spread_cutoff: 0.6,
            // 0.04 (was 0.1): the fade margin is a DEAD BAND where reflections
            // dissolve into the env fallback — at 0.1 that's ~10% of EVERY
            // screen border (a quarter of the width across both sides),
            // which reads as "graphics missing in the periphery" while
            // orbiting. 4% still hides the hard ray-exit seam.
            edge_fade: 0.04,
            resolution_scale: 0.5,
            temporal: false,
            temporal_weight: 0.9,
            debug: 0,
            bvh_reflections: false,
        }
    }
}

/// Runtime atmospheric-haze settings (mirrors
/// `awsm_renderer_scene::post_process::AtmosphereConfig`).
///
/// A stylized exponential medium applied in the EFFECTS pass: a pixel's colour
/// is lerped toward `color` by `1 - exp(-density · distance)`, with an optional
/// closed-form height integral so haze can pool below `base_height` and thin
/// out above it.
///
/// `mode` and `volumetric_temporal` are the **structural** fields. `mode` is a
/// three-way choice rather than an enable plus a style flag: the volumetric
/// path REPLACES the analytic term (same medium — running both double-counts
/// the air). Everything else is a LIVE uniform.
#[derive(Clone, Debug, PartialEq)]
pub struct Atmosphere {
    /// Off / analytic fog / froxel volumetrics. Structural.
    pub mode: AtmosphereMode,
    /// Linear radiance an infinitely distant surface fades to.
    pub color: [f32; 3],
    /// Extinction per meter (1/e distance = `1 / density`).
    pub density: f32,
    /// World Y where density is full.
    pub base_height: f32,
    /// Exponential thinning per meter above `base_height`; `0` = uniform.
    pub height_falloff: f32,
    /// Henyey-Greenstein phase anisotropy (0 iso, >0 forward). Live uniform,
    /// volumetric path only.
    pub scattering_anisotropy: f32,
    /// Far plane of the froxel volume in meters — the medium isn't simulated
    /// past it. Deliberately much nearer than the light-culling far plane the
    /// slicing is borrowed from (see the scene-side docs). Live uniform.
    pub volumetric_distance: f32,
    /// Temporal reprojection of the froxel volume. Structural.
    pub volumetric_temporal: bool,
}

/// Runtime mirror of `awsm_renderer_scene::post_process::AtmosphereMode`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AtmosphereMode {
    #[default]
    Off,
    /// Closed-form fog along the view ray. Cheap; no light shafts.
    Fog,
    /// Froxel scattering volume with per-light in-scatter. Beams and shafts,
    /// substantially more expensive.
    Volumetric,
}

impl Default for Atmosphere {
    fn default() -> Self {
        Self {
            mode: AtmosphereMode::Off,
            color: [0.5, 0.6, 0.7],
            density: 0.02,
            base_height: 0.0,
            height_falloff: 0.0,
            scattering_anisotropy: 0.3,
            volumetric_distance: 80.0,
            volumetric_temporal: false,
        }
    }
}

/// Tonemapping operator selection.
#[derive(Clone, Debug, PartialEq, Eq, Copy, Hash)]
pub enum ToneMapping {
    None,
    KhronosNeutralPbr,
    Aces,
}

impl Default for PostProcessing {
    fn default() -> Self {
        Self {
            tonemapping: ToneMapping::KhronosNeutralPbr,
            bloom: false,
            dof: false,
            exposure: 0.0,
            bloom_threshold: 1.0,
            bloom_knee: 0.5,
            bloom_intensity: 1.0,
            bloom_scatter: 1.0,
            ssr: Ssr::default(),
            atmosphere: Atmosphere::default(),
        }
    }
}

/// Which post-process pipelines failed to compile, by the part of the
/// post-processing config that asked for them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FailedPostProcess {
    /// A pipeline of the SSR pass (trace / resolve / temporal / BVH trace).
    pub ssr: bool,
    /// A pipeline of the bloom pyramid pass.
    pub bloom: bool,
    /// A pipeline of the froxel volumetrics pass.
    pub volumetrics: bool,
    /// The effects pass's plain variant (tone mapping, haze term) or the
    /// display pipeline — both compiled for the config as a whole, so a
    /// failure cannot be pinned on one effect. (Its bloom-BLEND variant can:
    /// that one counts as `bloom`.)
    pub effects: bool,
}

/// The pipeline keys the post-process passes hold right now.
#[derive(Default)]
pub(crate) struct PostProcessPipelineKeys {
    pub ssr: Vec<crate::pipelines::compute_pipeline::ComputePipelineKey>,
    pub bloom: Vec<crate::pipelines::compute_pipeline::ComputePipelineKey>,
    pub volumetrics: Vec<crate::pipelines::compute_pipeline::ComputePipelineKey>,
    /// The effects pass's plain (no-bloom) variant.
    pub effects: Vec<crate::pipelines::compute_pipeline::ComputePipelineKey>,
    /// The effects pass's bloom-blend variant — only dispatched while bloom
    /// is on, so its failure costs bloom and nothing else.
    pub effects_bloom_blend: Vec<crate::pipelines::compute_pipeline::ComputePipelineKey>,
    pub display: Vec<crate::pipelines::render_pipeline::RenderPipelineKey>,
}

impl PostProcessPipelineKeys {
    /// Says which post-process passes the failed pipelines belong to — or
    /// `None` when any of them is not a post-process pipeline at all. Those
    /// (geometry, shadows, material prep, …) are pipelines a frame cannot be
    /// drawn without, so their failure stays an error.
    pub(crate) fn attribute(
        &self,
        failed_compute: impl IntoIterator<Item = crate::pipelines::compute_pipeline::ComputePipelineKey>,
        failed_render: impl IntoIterator<Item = crate::pipelines::render_pipeline::RenderPipelineKey>,
    ) -> Option<FailedPostProcess> {
        let mut failed = FailedPostProcess::default();
        for key in failed_compute {
            if self.ssr.contains(&key) {
                failed.ssr = true;
            } else if self.bloom.contains(&key) || self.effects_bloom_blend.contains(&key) {
                failed.bloom = true;
            } else if self.volumetrics.contains(&key) {
                failed.volumetrics = true;
            } else if self.effects.contains(&key) {
                failed.effects = true;
            } else {
                return None;
            }
        }
        for key in failed_render {
            if self.display.contains(&key) {
                failed.effects = true;
            } else {
                return None;
            }
        }
        Some(failed)
    }
}

impl PostProcessing {
    /// The config to run with after `failed` could not be compiled: each
    /// failed effect turned off, everything else as authored.
    ///
    /// A failed effects / display variant belongs to no single effect, so it
    /// falls back to `fallback` as a whole (the config the renderer was built
    /// with). `None` when that is already the config in use — there is
    /// nothing left to fall back to, and the failure is an error.
    pub(crate) fn degraded(
        &self,
        failed: FailedPostProcess,
        fallback: &PostProcessing,
    ) -> Option<PostProcessing> {
        let mut degraded = if failed.effects {
            if self == fallback {
                return None;
            }
            fallback.clone()
        } else {
            self.clone()
        };
        if failed.ssr {
            degraded.ssr.enabled = false;
        }
        if failed.bloom {
            degraded.bloom = false;
        }
        if failed.volumetrics && degraded.atmosphere.mode == AtmosphereMode::Volumetric {
            degraded.atmosphere.mode = AtmosphereMode::Off;
        }
        Some(degraded)
    }
}

impl AwsmRenderer {
    /// Applies post-processing configuration and recompiles the
    /// dependent pipelines for the new config.
    ///
    /// **Lazy-pool model:** cold-boot only compiled the effects
    /// pass's `BloomPhase::None` shader (the default-disabled bloom
    /// case). Toggling `bloom: true` here triggers a transactional
    /// recompile that compiles the 4 missing bloom phases inside
    /// one batched `Shaders::ensure_keys` + one batched
    /// `ComputePipelines::ensure_keys`. The display pipeline (which
    /// depends on the live tonemapper) is recompiled the same way.
    ///
    /// On a committed scene this returns only after every newly-required
    /// variant is GPU-resident, so the next render frame can dispatch
    /// without further awaits, and a pipeline the driver rejects is this
    /// call's error.
    ///
    /// **Before the first `commit_load`** (and again between `begin_load()`
    /// and its commit) nothing is on screen, so the pipelines are only
    /// RESERVED here and compile in the commit's batch with the rest of the
    /// load. The shader modules are still created and validated by this
    /// call (a WGSL error is returned from it), but a pipeline the driver
    /// rejects surfaces in `commit_load` — where a post-process pass that
    /// cannot be built is turned off with a warning rather than failing the
    /// load (`AwsmRenderer::degrade_failed_post_processing`). Read
    /// `renderer.post_processing` back after the commit if you mirror it.
    pub async fn set_post_processing(&mut self, pp: PostProcessing) -> Result<()> {
        // Race policy per https://github.com/dakom/awsm-renderer/pull/99: config-change
        // APIs return NotReady when called before build() finishes its
        // eager batch.
        if !self.build_complete {
            return Err(crate::error::AwsmError::NotReady);
        }
        // No-op fast path — caller asked for the state we're
        // already in. Saves a redundant batched ensure on UI
        // double-fire (common when sliders re-emit on every focus
        // event).
        if self.post_processing == pp {
            return Ok(());
        }
        // Only the pipeline-VARIANT fields force a shader recompile. Exposure and
        // the bloom params are LIVE uniforms written per-frame from config, so
        // tweaking them (the common tuning case) must NOT recompile — just swap
        // the config and let the next frame pick them up.
        let recompile_needed = self.post_processing.tonemapping != pp.tonemapping
            || self.post_processing.bloom != pp.bloom
            || self.post_processing.dof != pp.dof
            // SSR structural axes select compiled variants (§5a); the scalar
            // SSR knobs are live uniforms and must NOT recompile.
            || self.post_processing.ssr.enabled != pp.ssr.enabled
            || self.post_processing.ssr.temporal != pp.ssr.temporal
            || self.post_processing.ssr.resolution_scale != pp.ssr.resolution_scale
            || self.post_processing.ssr.debug != pp.ssr.debug
            || self.post_processing.ssr.bvh_reflections != pp.ssr.bvh_reflections
            // Atmosphere's structural axes: `mode` selects which haze term (if
            // any) is compiled into the effects shader, `volumetric_temporal`
            // selects the froxel variant. Colour/density/heights/anisotropy are
            // live uniforms and must not recompile — that's the tuning case.
            || self.post_processing.atmosphere.mode != pp.atmosphere.mode
            || self.post_processing.atmosphere.volumetric_temporal
                != pp.atmosphere.volumetric_temporal;
        // Toggling SSR flips the `write_ssr_descriptor` axis on the
        // material_opaque cache key, so the live material modules must recompile
        // to add/drop the descriptor store (lazy — only the variants the scene
        // actually uses). Captured before we overwrite `post_processing` since
        // the launch builders read the new `ssr.enabled` when they re-key.
        let ssr_enabled_changed = self.post_processing.ssr.enabled != pp.ssr.enabled;
        // The SSR PASS itself (its bind-group LAYOUT + trace pipeline) is keyed on
        // the `temporal` and `half_res` structural axes: `temporal` adds the
        // history read/sampler/write bindings (7/8/9) to the layout, and both
        // axes select the compiled trace variant. `enabled` does NOT change the
        // layout (it only swaps the reflection targets 1×1↔full-res). So when
        // temporal / resolution_scale change at RUNTIME, the boot-built SSR pass
        // holds a STALE layout + pipeline and must be reconstructed — otherwise
        // the trace shader (recompiled for the new variant) mismatches the pass's
        // frozen bind-group layout. Mirrors `set_anti_aliasing`, which rebuilds
        // passes on the MSAA structural flip the same way.
        let ssr_pass_rebuild_needed = self.post_processing.ssr.temporal != pp.ssr.temporal
            || self.post_processing.ssr.resolution_scale != pp.ssr.resolution_scale
            || self.post_processing.ssr.debug != pp.ssr.debug
            || self.post_processing.ssr.bvh_reflections != pp.ssr.bvh_reflections;
        // LAZY SSR (axis 1): the pass is `None` until the first enable — a
        // session that never turns SSR on compiles neither the trace compute
        // nor the composite render pipeline. Build it now when enabling with
        // no pass yet; also rebuild an EXISTING pass on the structural axes
        // (see the comment above). Both go through the same construction.
        let ssr_pass_build_needed = (pp.ssr.enabled && self.render_passes.ssr.is_none())
            || (ssr_pass_rebuild_needed && self.render_passes.ssr.is_some());
        // The TAA resolve averages in the display's working space, so its
        // shader is keyed on the tonemapper (exposure is a live uniform).
        let taa_resolve_rebuild_needed =
            self.post_processing.tonemapping != pp.tonemapping && self.render_passes.taa.is_some();
        self.post_processing = pp;

        if taa_resolve_rebuild_needed {
            self.rebuild_taa_resolve_pipeline().await?;
        }

        if ssr_pass_build_needed {
            // (Re)construct the SSR pass against the NEW post-processing state
            // (`self.post_processing` was just assigned above, so `SsrRenderPass::new`
            // reads the new `temporal`/`resolution_scale`). Same `RenderPassInitContext`
            // shape the cold-boot + `set_anti_aliasing` paths build. `render_passes`
            // is not part of the ctx, so reassigning `self.render_passes.ssr` after
            // the borrow ends is a clean disjoint update.
            let mut ctx = crate::render_passes::RenderPassInitContext {
                gpu: &self.gpu,
                bind_group_layouts: &mut self.bind_group_layouts,
                pipeline_layouts: &mut self.pipeline_layouts,
                pipelines: &mut self.pipelines,
                shaders: &mut self.shaders,
                render_texture_formats: &mut self.render_textures.formats,
                textures: &mut self.textures,
                features: &self.features,
                anti_aliasing: &self.anti_aliasing,
                post_processing: &self.post_processing,
                prep_config: &self.prep_config,
                max_edge_budget: self.material_edge_buffers.as_ref().map(|b| b.max_edge_budget).unwrap_or(crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP),
            };
            let ssr = crate::render_passes::ssr::render_pass::SsrRenderPass::new(&mut ctx).await?;
            let new_keys = ssr.pipelines.keys();
            if let Some(old) = self.render_passes.ssr.replace(ssr) {
                self.release_replaced_pass_slots(old.pipelines.keys(), &new_keys);
            }
            // The reflection / history targets are sized from `temporal` +
            // `resolution_scale` too, so force `views()` to re-evaluate their
            // sizes and rebind the freshly-built SSR bind group against them.
            self.bind_groups
                .mark_create(crate::bind_groups::BindGroupCreate::TextureViewRecreate);
        }

        // LAZY bloom (axis 1): mirrors SSR — the mip-pyramid pass is `None`
        // until the first `bloom: true`, so a session that never enables bloom
        // compiles none of its 3 compute pipelines. Built awaited here, so the
        // next frame dispatches without further compiles; the
        // `TextureViewRecreate` mark makes the next frame's bind-group drain
        // build its groups against the live views (the per-frame `ensure_size`
        // then grows the 1×1 pyramid, marking again — same flow a boot-enabled
        // bloom uses).
        if self.post_processing.bloom && self.render_passes.bloom.is_none() {
            let mut ctx = crate::render_passes::RenderPassInitContext {
                gpu: &self.gpu,
                bind_group_layouts: &mut self.bind_group_layouts,
                pipeline_layouts: &mut self.pipeline_layouts,
                pipelines: &mut self.pipelines,
                shaders: &mut self.shaders,
                render_texture_formats: &mut self.render_textures.formats,
                textures: &mut self.textures,
                features: &self.features,
                anti_aliasing: &self.anti_aliasing,
                post_processing: &self.post_processing,
                prep_config: &self.prep_config,
                max_edge_budget: self.material_edge_buffers.as_ref().map(|b| b.max_edge_budget).unwrap_or(crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP),
            };
            let bloom =
                crate::render_passes::bloom::render_pass::BloomRenderPass::new(&mut ctx).await?;
            self.render_passes.bloom = Some(bloom);
            self.bind_groups
                .mark_create(crate::bind_groups::BindGroupCreate::TextureViewRecreate);
        }

        // LAZY volumetrics: mirrors bloom/SSR. The froxel volume is a pair of
        // 3D textures plus two compute pipelines, so a session that never asks
        // for beams must never build it. Awaited here so the next frame
        // dispatches without compiling; the `TextureViewRecreate` mark makes
        // that frame's bind-group drain build the groups against live views
        // (the per-frame `ensure_size` then grows the 1×1 volume, marking
        // again — the same flow a boot-enabled volumetric config uses).
        //
        // ALSO a structural REBUILD path: `volumetric_temporal` adds the
        // history sampler to group 0's layout and selects a different `inject`
        // variant, so a pass built for one setting holds a stale layout +
        // pipeline for the other. Exactly the trap `ssr_pass_rebuild_needed`
        // above exists for, and the same fix.
        let volumetrics_rebuild_needed = self
            .render_passes
            .volumetrics
            .as_ref()
            .is_some_and(|v| v.temporal != self.post_processing.atmosphere.volumetric_temporal);
        if self.post_processing.atmosphere.mode == AtmosphereMode::Volumetric
            && (self.render_passes.volumetrics.is_none() || volumetrics_rebuild_needed)
        {
            let mut ctx = crate::render_passes::RenderPassInitContext {
                gpu: &self.gpu,
                bind_group_layouts: &mut self.bind_group_layouts,
                pipeline_layouts: &mut self.pipeline_layouts,
                pipelines: &mut self.pipelines,
                shaders: &mut self.shaders,
                render_texture_formats: &mut self.render_textures.formats,
                textures: &mut self.textures,
                features: &self.features,
                anti_aliasing: &self.anti_aliasing,
                post_processing: &self.post_processing,
                prep_config: &self.prep_config,
                max_edge_budget: self.material_edge_buffers.as_ref().map(|b| b.max_edge_budget).unwrap_or(crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP),
            };
            let volumetrics =
                crate::render_passes::volumetrics::render_pass::VolumetricsRenderPass::new(
                    &mut ctx,
                )
                .await?;
            // On the REBUILD path, destroy the old pass's GPU volumes rather
            // than letting the assignment drop them: bind groups and
            // pipelines are cheap to orphan, but the scatter / integrated /
            // history 3D textures are real VRAM the JS GC can't see — the
            // same rule `ensure_size` follows on resize.
            let new_keys = volumetrics.pipelines.keys();
            if let Some(old) = self.render_passes.volumetrics.take() {
                self.release_replaced_pass_slots(old.pipelines.keys(), &new_keys);
                old.texture.destroy();
            }
            self.render_passes.volumetrics = Some(volumetrics);
            self.bind_groups
                .mark_create(crate::bind_groups::BindGroupCreate::TextureViewRecreate);
        }

        if ssr_enabled_changed {
            // Mirrors the MSAA path (`set_anti_aliasing`): flag the reconcile,
            // then drive it. `reconcile_material_variants` → `ensure_scene_pipelines`
            // re-keys every live bucket; the changed `write_ssr_descriptor`
            // MISSES the pipeline cache and recompiles exactly those variants.
            //
            // Only when a scene is committed, though. Before the first commit
            // (the caches are deferred) nothing is on screen to recompile for,
            // and the commit runs this reconcile itself, once the texture pool
            // and the bucket layout are final. Kicking the kernels from here
            // would compile them against a pool tier that may still change —
            // and, worse for load time, put the largest compiles of the load
            // at the head of the compiler queue, ahead of everything the
            // commit still has to await one by one (lighting defaults, texture
            // uploads) before it can issue the rest of the load's pipelines.
            self.materials.mark_variants_dirty();
            if !self.pipelines.is_deferred() {
                self.reconcile_material_variants()?;
            }
            // The SSR `ssr` + `reflection_descriptor` targets are 1×1 placeholders
            // when SSR is off and full-res when on. Flag the bind-group recreate
            // so the next frame's `views()` (which now sees the new `ssr.enabled`)
            // re-allocates them full-res / 1×1 and rebinds binding 24 + the SSR
            // pass. Same mark `set_anti_aliasing` uses for its texture rebuild.
            self.bind_groups
                .mark_create(crate::bind_groups::BindGroupCreate::TextureViewRecreate);
            // Edge accumulator slot width tracks ssr.enabled (narrow 16 B off
            // / wide 32 B on — the descriptor half is only allocated when SSR
            // can write it; ~32 MB at the desktop budget). The classify /
            // opaque / final_blend shaders recompile for the new stride via
            // the reconcile + classify re-key above; recreate the buffer to
            // match and rebind everything that references it.
            if let Some(edge_buffers) = self.material_edge_buffers.as_mut() {
                if edge_buffers.set_wide_slots(&self.gpu, self.post_processing.ssr.enabled)? {
                    self.bind_groups.mark_create(
                        crate::bind_groups::BindGroupCreate::MaterialClassifyBuffersResize,
                    );
                }
            }
        }

        if !recompile_needed {
            return Ok(());
        }

        let had_dof_prepare = self
            .render_passes
            .effects
            .pipelines
            .dof_prepare_pipeline()
            .is_some();
        self.render_passes
            .effects
            .pipelines
            .set_render_pipeline_keys(
                &self.anti_aliasing,
                &self.post_processing,
                &self.gpu,
                &mut self.shaders,
                &mut self.pipelines,
                &self.pipeline_layouts,
                &self.render_textures.formats,
                self.features.reverse_z,
            )
            .await?;
        // The effects bind group holds the DoF buffer, sized only while DoF
        // is on.
        if had_dof_prepare
            != crate::render_passes::effects::pipeline::effects_dof(
                &self.anti_aliasing,
                &self.post_processing,
            )
        {
            self.bind_groups
                .mark_create(crate::bind_groups::BindGroupCreate::TextureViewRecreate);
        }
        // DoF before the TAA resolve: its pass exists only while DoF is on.
        self.sync_taa_dof_pre().await?;

        self.render_passes
            .display
            .pipelines
            .set_render_pipeline_key(
                &self.post_processing,
                self.display_downsamples(),
                &self.gpu,
                &mut self.shaders,
                &mut self.pipelines,
                &self.pipeline_layouts,
                &self.render_textures.formats,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod degrade_tests {
    use super::*;
    use crate::pipelines::compute_pipeline::ComputePipelineKey;
    use crate::pipelines::render_pipeline::RenderPipelineKey;
    use slotmap::SlotMap;

    /// A scene config with every effect on.
    fn everything_on() -> PostProcessing {
        let mut pp = PostProcessing {
            bloom: true,
            ..Default::default()
        };
        pp.ssr.enabled = true;
        pp.atmosphere.mode = AtmosphereMode::Volumetric;
        pp
    }

    /// Keys as the passes would hold them: two per compute pass, one display.
    fn pass_keys() -> (
        PostProcessPipelineKeys,
        ComputePipelineKey,
        RenderPipelineKey,
    ) {
        let mut compute: SlotMap<ComputePipelineKey, ()> = SlotMap::with_key();
        let mut render: SlotMap<RenderPipelineKey, ()> = SlotMap::with_key();
        let mut two = || vec![compute.insert(()), compute.insert(())];
        let keys = PostProcessPipelineKeys {
            ssr: two(),
            bloom: two(),
            volumetrics: two(),
            effects: two(),
            effects_bloom_blend: two(),
            display: vec![render.insert(())],
        };
        // …and one of each class that belongs to no post-process pass.
        (keys, compute.insert(()), render.insert(()))
    }

    #[test]
    fn a_failed_pass_pipeline_is_attributed_to_its_effect() {
        let (keys, _, _) = pass_keys();
        assert_eq!(
            keys.attribute([keys.ssr[1]], []),
            Some(FailedPostProcess {
                ssr: true,
                ..Default::default()
            })
        );
        assert_eq!(
            keys.attribute([keys.bloom[0], keys.volumetrics[1]], []),
            Some(FailedPostProcess {
                bloom: true,
                volumetrics: true,
                ..Default::default()
            })
        );
        // The effects variant and the display pipeline are both "the config
        // as a whole".
        let effects = Some(FailedPostProcess {
            effects: true,
            ..Default::default()
        });
        assert_eq!(keys.attribute([keys.effects[0]], []), effects);
        assert_eq!(keys.attribute([], [keys.display[0]]), effects);
        // …except the effects pass's bloom-blend variant, which only bloom
        // dispatches: losing it costs bloom, not the whole config.
        assert_eq!(
            keys.attribute([keys.effects_bloom_blend[0]], []),
            Some(FailedPostProcess {
                bloom: true,
                ..Default::default()
            })
        );
    }

    #[test]
    fn a_failure_outside_post_processing_is_not_attributed() {
        let (keys, other_compute, other_render) = pass_keys();
        assert_eq!(keys.attribute([other_compute], []), None);
        assert_eq!(keys.attribute([], [other_render]), None);
        // One stray failure makes the whole batch an error, whatever else
        // failed with it.
        assert_eq!(keys.attribute([keys.ssr[0], other_compute], []), None);
        assert_eq!(keys.attribute([keys.bloom[0]], [other_render]), None);
    }

    #[test]
    fn a_failed_effect_is_turned_off_and_the_rest_kept() {
        let authored = everything_on();
        let fallback = PostProcessing::default();

        let degraded = authored
            .degraded(
                FailedPostProcess {
                    ssr: true,
                    ..Default::default()
                },
                &fallback,
            )
            .expect("SSR can be turned off");
        assert!(!degraded.ssr.enabled);
        assert!(degraded.bloom);
        assert_eq!(degraded.atmosphere.mode, AtmosphereMode::Volumetric);

        let degraded = authored
            .degraded(
                FailedPostProcess {
                    bloom: true,
                    volumetrics: true,
                    ..Default::default()
                },
                &fallback,
            )
            .expect("bloom and volumetrics can be turned off");
        assert!(degraded.ssr.enabled);
        assert!(!degraded.bloom);
        assert_eq!(degraded.atmosphere.mode, AtmosphereMode::Off);
    }

    #[test]
    fn failed_volumetrics_leave_analytic_fog_alone() {
        // The pass can outlive the mode that built it; its failure must not
        // switch off a fog the scene has since moved to.
        let mut authored = everything_on();
        authored.atmosphere.mode = AtmosphereMode::Fog;
        let degraded = authored
            .degraded(
                FailedPostProcess {
                    volumetrics: true,
                    ..Default::default()
                },
                &PostProcessing::default(),
            )
            .expect("nothing to turn off is still not an error");
        assert_eq!(degraded, authored);
    }

    #[test]
    fn a_failed_effects_variant_falls_back_to_the_built_config() {
        let authored = everything_on();
        let fallback = PostProcessing::default();
        let failed = FailedPostProcess {
            effects: true,
            ..Default::default()
        };
        assert_eq!(authored.degraded(failed, &fallback), Some(fallback.clone()));
        // Already on the built config: nothing left to fall back to.
        assert_eq!(fallback.degraded(failed, &fallback), None);
    }

    #[test]
    fn a_failed_pass_stays_off_in_the_fallback_too() {
        let mut fallback = PostProcessing::default();
        fallback.ssr.enabled = true;
        let degraded = everything_on()
            .degraded(
                FailedPostProcess {
                    ssr: true,
                    effects: true,
                    ..Default::default()
                },
                &fallback,
            )
            .expect("falls back");
        assert!(!degraded.ssr.enabled);
        assert!(!degraded.bloom);
    }
}
