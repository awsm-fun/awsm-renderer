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
        }
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
    /// Returns only after every newly-required variant is GPU-
    /// resident, so the next render frame can dispatch without
    /// further awaits.
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
            || self.post_processing.ssr.debug != pp.ssr.debug;
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
            || self.post_processing.ssr.debug != pp.ssr.debug;
        // LAZY SSR (axis 1): the pass is `None` until the first enable — a
        // session that never turns SSR on compiles neither the trace compute
        // nor the composite render pipeline. Build it now when enabling with
        // no pass yet; also rebuild an EXISTING pass on the structural axes
        // (see the comment above). Both go through the same construction.
        let ssr_pass_build_needed = (pp.ssr.enabled && self.render_passes.ssr.is_none())
            || (ssr_pass_rebuild_needed && self.render_passes.ssr.is_some());
        self.post_processing = pp;

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
            self.render_passes.ssr = Some(ssr);
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

        if ssr_enabled_changed {
            // Mirrors the MSAA path (`set_anti_aliasing`): flag the reconcile,
            // then drive it. `reconcile_material_variants` → `ensure_scene_pipelines`
            // re-keys every live bucket; the changed `write_ssr_descriptor`
            // MISSES the pipeline cache and recompiles exactly those variants.
            self.materials.mark_variants_dirty();
            self.reconcile_material_variants()?;
            // The SSR `ssr` + `reflection_descriptor` targets are 1×1 placeholders
            // when SSR is off and full-res when on. Flag the bind-group recreate
            // so the next frame's `views()` (which now sees the new `ssr.enabled`)
            // re-allocates them full-res / 1×1 and rebinds binding 24 + the SSR
            // pass. Same mark `set_anti_aliasing` uses for its texture rebuild.
            self.bind_groups
                .mark_create(crate::bind_groups::BindGroupCreate::TextureViewRecreate);
        }

        if !recompile_needed {
            return Ok(());
        }

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

        self.render_passes
            .display
            .pipelines
            .set_render_pipeline_key(
                &self.post_processing,
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
