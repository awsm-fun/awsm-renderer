//! TAA resolve execution, per-frame upkeep and lifecycle.
//!
//! One compute dispatch at post size (8x8 workgroups, one invocation per
//! output pixel) right after the SSR composite (03 §6). It reads the
//! render-size `composite`, depth and velocity, last frame's depth and
//! history, and writes this frame's history and `out`. Then a depth-to-depth
//! copy saves this frame's depth for the next resolve's disocclusion test.
//!
//! Lifecycle: built by `set_anti_aliasing` when TAA turns on (or at boot),
//! dropped with its textures when it turns off ([`AwsmRenderer::sync_taa_pass`]).
//! With `anti_aliasing.taa == None` nothing here exists or runs.

use std::cell::{Cell, RefCell};

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    command::{compute_pass::ComputePassDescriptor, copy_texture::TexelCopyTextureInfo},
    renderer::AwsmRendererWebGpu,
    texture::Extent3d,
};

use crate::{
    bind_groups::BindGroupCreate,
    buffer::mapped_uploader::MappedUploader,
    error::Result,
    render::RenderContext,
    render_passes::{
        taa::{bind_group::TaaBindGroups, pipeline::TaaPipelines, texture::TaaTextures},
        RenderPassInitContext,
    },
    render_textures::RenderTextureViews,
    taa::TaaConfig,
    AwsmRenderer,
};

/// Whether the pass binds the reactive mask (`TaaRenderPass::new`): the
/// render textures allocate `views.reactive` whenever TAA is on (plan phase
/// 3), bound at binding 10. `false` remains a valid variant (tests, and a
/// future device path without the mask).
pub(crate) const REACTIVE_MASK: bool = true;

/// `TaaParams`: 88-byte uniform (22 x f32), the live tuning of the resolve.
/// Layout must match `struct TaaParams` in `taa_wgsl/resolve.wgsl`.
///
/// Interior-mutable so the reactive flag can be set after
/// `prepare_taa_frame` ([`TaaParams::set_reactive_valid`]), while the frame
/// borrows the pass shared. Every write goes through the same mapped
/// uploader, so a later write in the frame lands after an earlier one (a
/// `queue.writeBuffer` would run BEFORE the uploader's copies and be
/// overwritten by them).
pub struct TaaParams {
    pub gpu_buffer: web_sys::GpuBuffer,
    raw_data: Cell<[u8; Self::BYTE_SIZE]>,
    uploader: RefCell<MappedUploader>,
    /// Nothing uploaded yet: the first `write` uploads even if the packed
    /// bytes equal the zeroed initial state.
    uploaded: Cell<bool>,
    /// The last values written (the reactive flag is carried over from one
    /// `prepare_taa_frame` to the next, so it does not flip every frame).
    values: Cell<Option<TaaParamsValues>>,
}

/// Everything `TaaParams` packs, in uniform order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TaaParamsValues {
    pub render_size: (u32, u32),
    pub post_size: (u32, u32),
    pub gamma_still: f32,
    pub gamma_moving: f32,
    pub gamma_moving_speed: f32,
    pub disocclusion_threshold: f32,
    pub max_history: f32,
    pub motion_alpha: f32,
    pub motion_alpha_speed: f32,
    pub upscaling: bool,
    pub reset: bool,
    pub debug_view: u32,
    pub kernel_sharpness: f32,
    /// Whether the reactive mask was written this frame (plan phase 3). The
    /// shader multiplies the mask by it; 0 until the mask exists.
    pub reactive_valid: bool,
    pub max_history_moving: f32,
    pub max_history_moving_speed: f32,
    pub motion_edge_alpha: f32,
    pub motion_edge_speed: f32,
    /// Current weight per unit of clip amount (03 §4.8: 0.5).
    pub clip_alpha: f32,
    /// The display's exposure multiplier, `exp2(post_processing.exposure)`:
    /// the resolve's working space is what the display shows.
    pub exposure: f32,
}

impl TaaParamsValues {
    /// The values for `config` this frame.
    pub fn new(
        config: &TaaConfig,
        render_size: (u32, u32),
        post_size: (u32, u32),
        debug_view: u32,
        reset: bool,
    ) -> Self {
        let tuning = &config.tuning;
        // From the sizes actually rendered, not `config.upscales()`: the scene
        // scale is pinned at 1.0 until plan phase 4, so a Quality / Balanced /
        // Performance preset still renders at native size.
        let upscaling = render_size != post_size;
        Self {
            render_size,
            post_size,
            gamma_still: crate::taa::still_gamma(tuning, render_size, post_size),
            gamma_moving: tuning.gamma_moving,
            gamma_moving_speed: tuning.gamma_moving_speed,
            disocclusion_threshold: tuning.disocclusion_threshold,
            max_history: crate::taa::history_cap(tuning, render_size, post_size),
            motion_alpha: tuning.motion_alpha,
            motion_alpha_speed: tuning.motion_alpha_speed,
            upscaling,
            reset,
            debug_view,
            // The reconstruction kernel is the reference's: exp(-k d^2) with
            // d in OUTPUT pixels. The shader measures d in render pixels, so
            // k scales with the squared pixel ratio (1 at native). A kernel
            // one RENDER pixel wide blurs an upscaled image by the pixel
            // ratio: at 0.67 the converged `taa-thin` read 3.48 against the
            // reference, 1.24 with the kernel in output pixels.
            kernel_sharpness: crate::taa::RECONSTRUCTION_SHARPNESS
                * (post_size.0 as f32 / render_size.0.max(1) as f32).powi(2),
            reactive_valid: false,
            max_history_moving: tuning.max_history_moving,
            max_history_moving_speed: tuning.max_history_moving_speed,
            motion_edge_alpha: tuning.motion_edge_alpha,
            motion_edge_speed: tuning.motion_edge_speed,
            clip_alpha: crate::taa::CLIP_ALPHA,
            exposure: 1.0,
        }
    }

    /// The uniform bytes (native endian, like every other uniform here).
    pub fn pack(&self) -> [u8; TaaParams::BYTE_SIZE] {
        let flag = |b: bool| if b { 1.0f32 } else { 0.0 };
        let floats: [f32; 22] = [
            self.render_size.0 as f32,
            self.render_size.1 as f32,
            self.post_size.0 as f32,
            self.post_size.1 as f32,
            self.gamma_still,
            self.gamma_moving,
            self.gamma_moving_speed,
            self.disocclusion_threshold,
            self.max_history,
            self.motion_alpha,
            self.motion_alpha_speed,
            flag(self.upscaling),
            flag(self.reset),
            self.debug_view as f32,
            self.kernel_sharpness,
            flag(self.reactive_valid),
            self.max_history_moving,
            self.max_history_moving_speed,
            self.motion_edge_alpha,
            self.motion_edge_speed,
            self.clip_alpha,
            self.exposure,
        ];
        let mut out = [0u8; TaaParams::BYTE_SIZE];
        for (i, f) in floats.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&f.to_ne_bytes());
        }
        out
    }
}

impl TaaParams {
    pub const BYTE_SIZE: usize = 88;

    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let gpu_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("TaaParams"),
                Self::BYTE_SIZE,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;
        Ok(Self {
            gpu_buffer,
            raw_data: Cell::new([0; Self::BYTE_SIZE]),
            uploader: RefCell::new(MappedUploader::new("TaaParams")),
            uploaded: Cell::new(false),
            values: Cell::new(None),
        })
    }

    /// Upload `values` if they differ from what the buffer holds. A still,
    /// steady frame uploads nothing; a reset frame and the frame after it
    /// upload 88 bytes each (the `reset` flag goes 1 then back to 0).
    pub fn write(&self, gpu: &AwsmRendererWebGpu, values: &TaaParamsValues) -> Result<()> {
        self.values.set(Some(*values));
        let packed = values.pack();
        if self.uploaded.get() && packed == self.raw_data.get() {
            return Ok(());
        }
        self.raw_data.set(packed);
        self.uploader.borrow_mut().write_dirty_ranges(
            gpu,
            &self.gpu_buffer,
            Self::BYTE_SIZE,
            packed.as_slice(),
            &[(0, Self::BYTE_SIZE)],
        )?;
        self.uploaded.set(true);
        Ok(())
    }

    /// The reactive flag of the last write (carried into the next frame's
    /// values).
    pub fn reactive_valid(&self) -> bool {
        self.values.get().is_some_and(|v| v.reactive_valid)
    }

    /// Plan phase 3: whether the reactive mask was written this frame, known
    /// only once the renderables are collected (after `prepare_taa_frame`).
    /// Re-uploads the params when it changes; call before the frame's submit.
    /// Not called before phase 3 (the flag stays 0).
    pub fn set_reactive_valid(&self, gpu: &AwsmRendererWebGpu, valid: bool) -> Result<()> {
        let Some(mut values) = self.values.get() else {
            return Ok(());
        };
        if values.reactive_valid == valid {
            return Ok(());
        }
        values.reactive_valid = valid;
        self.write(gpu, &values)
    }
}

/// Per-frame bookkeeping of the resolve that must only advance when a frame's
/// resolve was actually SUBMITTED: a frame that fails before its submit, or
/// renders without the resolve, must neither flip the history parity nor
/// consume a reset. No GPU state, so it is unit-tested.
#[derive(Debug)]
pub(crate) struct ResolveBookkeeping {
    /// The history texture the next resolve writes (it reads the other).
    history_index: Cell<usize>,
    /// The history holds nothing yet (created or recreated): the next
    /// submitted resolve must reset.
    history_fresh: Cell<bool>,
    /// This frame recorded a resolve.
    recorded: Cell<bool>,
    /// This frame recorded its depth copy into the pending previous-depth
    /// target.
    copied_into_next: Cell<bool>,
}

impl ResolveBookkeeping {
    pub(crate) fn new() -> Self {
        Self {
            history_index: Cell::new(0),
            history_fresh: Cell::new(true),
            recorded: Cell::new(false),
            copied_into_next: Cell::new(false),
        }
    }

    pub(crate) fn history_index(&self) -> usize {
        self.history_index.get()
    }

    pub(crate) fn history_fresh(&self) -> bool {
        self.history_fresh.get()
    }

    pub(crate) fn mark_history_fresh(&self) {
        self.history_fresh.set(true);
    }

    /// Start of a frame's upkeep: forget a recording that was never
    /// submitted (that frame failed).
    pub(crate) fn begin_frame(&self) {
        self.recorded.set(false);
        self.copied_into_next.set(false);
    }

    pub(crate) fn resolve_recorded(&self, copied_into_next: bool) {
        self.recorded.set(true);
        self.copied_into_next.set(copied_into_next);
    }

    /// The frame was submitted. Returns `(resolved, copied_into_next)`: when
    /// a resolve was in it, the parity flips and the fresh history is filled.
    pub(crate) fn submitted(&self) -> (bool, bool) {
        let resolved = self.recorded.replace(false);
        let copied = self.copied_into_next.replace(false);
        if resolved {
            self.history_index.set(1 - self.history_index.get());
            self.history_fresh.set(false);
        }
        (resolved, copied)
    }
}

pub struct TaaRenderPass {
    pub bind_groups: TaaBindGroups,
    pub pipelines: TaaPipelines,
    pub params: TaaParams,
    pub textures: TaaTextures,
    /// The reactive seed draw (temporal-reactive materials), with the
    /// reactive variant only.
    pub reactive_seed: Option<super::reactive_seed::TaaReactiveSeed>,
    /// RCAS sharpening after the resolve (03 §9): `Some` only while
    /// `TaaConfig::sharpen > 0` (built and dropped by
    /// `AwsmRenderer::sync_taa_rcas`). Bloom, effects and the HUD-after-TAA
    /// pass then use its output instead of `textures.out`.
    pub rcas: Option<super::rcas::TaaRcas>,
    /// Resolve dispatches per frame (`AwsmRenderer::set_taa_debug_repeat`).
    /// 1 normally; N > 1 only to measure the pass's cost.
    repeat: u32,
    bookkeeping: ResolveBookkeeping,
}

impl TaaRenderPass {
    /// `reactive`: bind the reactive mask (plan phase 3; see
    /// [`REACTIVE_MASK`]).
    pub async fn new(ctx: &mut RenderPassInitContext<'_>, reactive: bool) -> Result<Self> {
        let bind_groups = TaaBindGroups::new(ctx, reactive)?;
        let pipelines = TaaPipelines::new(ctx, &bind_groups, reactive).await?;
        let params = TaaParams::new(ctx.gpu)?;
        let reactive_seed = if reactive {
            Some(super::reactive_seed::TaaReactiveSeed::new(ctx).await?)
        } else {
            None
        };
        // Sizes are not known here (as for SMAA); the textures start 1x1 and
        // the first frame's `prepare_taa_frame` sizes them. The history
        // starts fresh, so the first submitted resolve resets.
        let textures = TaaTextures::new(ctx.gpu, (1, 1), (1, 1), ctx.render_texture_formats.depth)?;
        Ok(Self {
            bind_groups,
            pipelines,
            params,
            textures,
            reactive_seed,
            rcas: None,
            repeat: 1,
            bookkeeping: ResolveBookkeeping::new(),
        })
    }

    /// Whether the history is allocated for `post_size` (see
    /// [`TaaTextures::history_matches`]).
    pub fn history_matches(&self, post_size: (u32, u32)) -> bool {
        self.textures.history_matches(post_size)
    }

    /// Call right after the frame's `submit_commands`. Returns whether a
    /// resolve was submitted: the caller then clears the pending reset it
    /// honoured. A frame that failed before its submit never calls this, so
    /// its parity flip, reset and depth copy are redone next frame.
    pub fn frame_submitted(&self) -> bool {
        let (resolved, copied_into_next) = self.bookkeeping.submitted();
        if copied_into_next {
            self.textures.mark_next_prev_depth_filled();
        }
        resolved
    }

    /// Release the GPU textures (TAA turned off).
    pub fn destroy(self) {
        if let Some(seed) = self.reactive_seed {
            seed.destroy();
        }
        if let Some(rcas) = self.rcas {
            rcas.destroy();
        }
        self.textures.destroy();
    }

    /// The post-size image the frame continues from after the resolve: the
    /// RCAS output while sharpening is on, else the resolve's `out`. Bloom,
    /// effects and the HUD-after-TAA pass use this.
    pub fn final_view(&self) -> &web_sys::GpuTextureView {
        self.rcas
            .as_ref()
            .and_then(|rcas| rcas.out_view())
            .unwrap_or(&self.textures.out_view)
    }

    /// Resolve, then save this frame's depth for the next resolve.
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        let (post_w, post_h) = self.textures.post_size;
        {
            let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
                &ComputePassDescriptor::new(Some("TAA Resolve"))
                    .with_timestamp_writes_opt(
                        ctx.gpu_timestamps.and_then(|t| t.writes_for_compute("Taa")),
                    )
                    .into(),
            ));
            compute_pass.set_pipeline(ctx.pipelines.compute.get(self.pipelines.resolve)?);
            // The pass's own parity, flipped only after a submitted resolve.
            compute_pass.set_bind_group(
                0,
                self.bind_groups.group(self.bookkeeping.history_index())?,
                None,
            )?;
            // `repeat > 1` is the cost-measurement hook (05 §5): the extra
            // dispatches redo the same work into the same targets, so the
            // image is unchanged and (T(N) - T(1)) / (N - 1) is one resolve.
            for _ in 0..self.repeat.max(1) {
                compute_pass.dispatch_workgroups(
                    post_w.div_ceil(8),
                    Some(post_h.div_ceil(8)),
                    Some(1),
                );
            }
            compute_pass.end();
        }

        // Last frame's depth for the next resolve (03 §3, §4.7). Recorded
        // right after the resolve, the only reader of the previous depth, so
        // this frame's resolve has already read last frame's. Nothing after
        // TAA writes the scene depth, so this is the depth that matches the
        // colour just accumulated. A copy is not a compute command: after
        // `end()`.
        let (target, target_size, into_next) = self.textures.prev_depth_copy_target();
        let render_size = ctx.render_texture_views.render_size;
        let mut copied_into_next = false;
        if let Some(inner) = ctx.render_textures.inner() {
            // `prepare_taa_frame` sized the target to this frame's render
            // size; a mismatch would be a validation error that loses the
            // frame, so skip the copy instead (the next frame's disocclusion
            // test then compares against an older depth, which is safe:
            // clipping still bounds the history).
            if target_size == render_size {
                ctx.command_encoder.copy_texture_to_texture(
                    &TexelCopyTextureInfo::new(&inner.depth).into(),
                    &TexelCopyTextureInfo::new(target).into(),
                    &Extent3d::new(render_size.0, Some(render_size.1), Some(1)).into(),
                )?;
                copied_into_next = into_next;
            }
        }
        self.bookkeeping.resolve_recorded(copied_into_next);
        Ok(())
    }
}

/// How far the volumetrics froxel jitter and the SSR march-jitter frame
/// counter are offset from their own frame index while TAA is on (03 §1.4).
/// The camera jitter is Halton(2, 3) indexed by `taa.frame_index`, and the
/// other two restart with it (an AA change rebuilds SSR and resets TAA in the
/// same call; a resize restarts volumetrics), so unoffset they would step in
/// lock-step and their patterns would correlate into visible structure. A
/// prime, so it shares no factor with any of the cycles (8 / 16 / the jitter
/// phase counts).
pub(crate) const TAA_JITTER_DECORRELATION_OFFSET: u32 = 7;

impl AwsmRenderer {
    /// Build the TAA pass when TAA is on and drop it (destroying its
    /// textures) when it is off. Called by `set_anti_aliasing`. Entering TAA
    /// resets the temporal history (03 §8: an AA-mode change); a change of
    /// TAA tuning or render scale while TAA stays on keeps it.
    pub(crate) async fn sync_taa_pass(&mut self) -> Result<()> {
        match (
            self.anti_aliasing.taa.is_some(),
            self.render_passes.taa.is_some(),
        ) {
            (true, false) => {
                let mut ctx = RenderPassInitContext {
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
                    max_edge_budget: self
                        .material_edge_buffers
                        .as_ref()
                        .map(|b| b.max_edge_budget)
                        .unwrap_or(
                            crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP,
                        ),
                };
                let pass = TaaRenderPass::new(&mut ctx, REACTIVE_MASK).await?;
                self.render_passes.taa = Some(pass);
                self.reset_temporal_history();
                self.sync_taa_rcas().await?;
            }
            (false, true) => {
                if let Some(pass) = self.render_passes.taa.take() {
                    // A slot reserved before the first commit and never
                    // compiled would otherwise be compiled at the next drain
                    // for nothing.
                    self.pipelines
                        .compute
                        .release_reserved(pass.pipelines.keys());
                    if let Some(seed) = pass.reactive_seed.as_ref() {
                        self.pipelines.render.release_reserved(seed.pipelines());
                    }
                    pass.destroy();
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Recompile the resolve for the current tonemapper (its working space,
    /// `ShaderCacheKeyTaa::tonemapping`), keeping the pass, its textures and
    /// the history: the stored history does not depend on the tonemapper.
    /// Called by `set_post_processing` when the tonemapper changes under TAA.
    pub(crate) async fn rebuild_taa_resolve_pipeline(&mut self) -> Result<()> {
        let Some(pass) = self.render_passes.taa.as_ref() else {
            return Ok(());
        };
        let mut ctx = RenderPassInitContext {
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
            max_edge_budget: self
                .material_edge_buffers
                .as_ref()
                .map(|b| b.max_edge_budget)
                .unwrap_or(
                    crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP,
                ),
        };
        let pipelines = TaaPipelines::new(&mut ctx, &pass.bind_groups, REACTIVE_MASK).await?;
        let new_keys = pipelines.keys();
        let old = match self.render_passes.taa.as_mut() {
            Some(pass) => std::mem::replace(&mut pass.pipelines, pipelines),
            None => return Ok(()),
        };
        self.release_replaced_pass_slots(old.keys(), &new_keys);
        Ok(())
    }

    /// Build the RCAS pass when TAA sharpening turns on (`sharpen > 0`) and
    /// drop it when it turns off. Called by `set_anti_aliasing`, on both its
    /// structural and its light path (a sharpen change alone is light: no
    /// pipeline of the frame recompiles, only this pass's own). Either change
    /// switches what bloom / effects / the HUD-after-TAA pass read, so the
    /// view-dependent bind groups are rebuilt. Zero cost while sharpen is 0:
    /// no pass, no texture.
    pub(crate) async fn sync_taa_rcas(&mut self) -> Result<()> {
        let want = self
            .anti_aliasing
            .taa
            .as_ref()
            .is_some_and(|taa| taa.sharpen.is_finite() && taa.sharpen > 0.0);
        let Some(pass) = self.render_passes.taa.as_mut() else {
            return Ok(());
        };
        match (want, pass.rcas.is_some()) {
            (true, false) => {
                pass.rcas = Some(super::rcas::TaaRcas::new(&self.gpu).await?);
                // Its output is created (and the consumers rebound) by the
                // next `prepare_taa_frame`.
            }
            (false, true) => {
                if let Some(rcas) = pass.rcas.take() {
                    rcas.destroy();
                }
                self.bind_groups
                    .mark_create(BindGroupCreate::TextureViewRecreate);
            }
            _ => {}
        }
        Ok(())
    }

    /// Per-frame TAA upkeep, before the bind-group ledger runs: size the
    /// textures, decide whether this frame's resolve resets, and upload the
    /// params. No-op (and no cost) when TAA is off.
    ///
    /// Resets (03 §8): a pending `reset_temporal_history()` (AA-mode change,
    /// scene load, cuts and teleports, the camera-jump guard and a history
    /// about to be (re)created, both of which `render` evaluates before
    /// choosing the frame's jitter) or a fresh history. Neither is consumed
    /// here: [`TaaRenderPass::frame_submitted`] clears them once a resolve
    /// that honoured them was submitted. A render-size change alone is not a
    /// reset: the history lives at post size.
    pub(crate) fn prepare_taa_frame(&mut self, views: &RenderTextureViews) -> Result<()> {
        let (Some(config), Some(pass)) = (
            self.anti_aliasing.taa.as_ref(),
            self.render_passes.taa.as_mut(),
        ) else {
            return Ok(());
        };

        pass.bookkeeping.begin_frame();
        pass.textures.tick_retired();
        if pass.textures.ensure_post_size(&self.gpu, views.post_size)? {
            // `out` is new: bloom / effects rebind it, as does the resolve.
            self.bind_groups
                .mark_create(BindGroupCreate::TextureViewRecreate);
            pass.bookkeeping.mark_history_fresh();
        }
        if pass
            .textures
            .ensure_render_size(&self.gpu, views.render_size)?
        {
            self.bind_groups.mark_create(BindGroupCreate::TaaResources);
        }
        if let Some(rcas) = pass.rcas.as_mut() {
            if rcas.ensure_size(&self.gpu, views.post_size)? {
                // A new output: the RCAS group itself (rebuilt with the
                // resolve's) and its consumers (bloom, effects).
                self.bind_groups
                    .mark_create(BindGroupCreate::TextureViewRecreate);
            }
            rcas.write_strength(&self.gpu, config.sharpen)?;
        }
        let reset = self.taa.reset_pending || pass.bookkeeping.history_fresh();
        pass.repeat = self.taa.debug_repeat.max(1);
        let mut values = TaaParamsValues::new(
            config,
            views.render_size,
            views.post_size,
            self.taa.debug_shader_id(),
            reset,
        );
        values.reactive_valid = pass.params.reactive_valid();
        values.exposure = self.post_processing.exposure.exp2();
        if let Some([clip_alpha, _]) = self.taa.experimental {
            values.clip_alpha = clip_alpha;
        }
        pass.params.write(&self.gpu, &values)?;
        Ok(())
    }

    /// Cost measurement (05 §5): dispatch the TAA resolve `n` times per frame
    /// (clamped to at least 1; 1 is normal). The extra dispatches redo the
    /// same work, so the image does not change and the resolve's cost is
    /// `(T(n) - T(1)) / (n - 1)`. Pass timestamps overlap on Apple GPUs, so
    /// this, not the per-pass stats, is how the pass is measured.
    pub fn set_taa_debug_repeat(&mut self, n: u32) {
        self.taa.debug_repeat = n.max(1);
    }

    /// Cost measurement: draw the TAA reactive seed `n` times per frame
    /// (at least 1; 1 is normal). Repeated draws change nothing (MAX blend,
    /// colour untouched), so the seed's cost is the slope of the pass that
    /// draws it. Applies to the live TAA pass; a rebuilt pass starts at 1.
    #[doc(hidden)]
    pub fn set_taa_debug_seed_repeat(&self, n: u32) {
        if let Some(seed) = self
            .render_passes
            .taa
            .as_ref()
            .and_then(|taa| taa.reactive_seed.as_ref())
        {
            seed.set_debug_repeat(n);
        }
    }

    /// Why this renderer replaced TAA by MSAA 4x, or `None` if it never had
    /// to. TAA falls back when its pipelines fail to compile on the device
    /// (a driver rejecting a TAA variant at a commit) or when
    /// `set_anti_aliasing` could not build the TAA pass. Editors and players
    /// show this instead of offering TAA again. Sticky for the renderer's
    /// lifetime.
    pub fn taa_fallback_reason(&self) -> Option<&str> {
        self.taa.fallback_reason.as_deref()
    }

    /// Replace TAA by MSAA 4x (same mipmap setting) after `reason`, giving
    /// back the reserved slots of the pipelines that failed so no later drain
    /// compiles (and fails) them again. Only while TAA is on and only once.
    /// The caller re-ensures the material pipelines for the new config (a
    /// commit does).
    pub(crate) async fn fall_back_from_taa(
        &mut self,
        reason: String,
        failed_compute: Vec<crate::pipelines::compute_pipeline::ComputePipelineKey>,
        failed_render: Vec<crate::pipelines::render_pipeline::RenderPipelineKey>,
    ) -> Result<()> {
        tracing::warn!("TAA unavailable, falling back to MSAA 4x: {reason}");
        self.taa.fallback_reason = Some(reason);
        self.pipelines.compute.release_reserved(failed_compute);
        self.pipelines.render.release_reserved(failed_render);
        let fallback = crate::anti_alias::AntiAliasing {
            mipmap: self.anti_aliasing.mipmap,
            ..crate::anti_alias::AntiAliasing::msaa4()
        };
        Box::pin(self.set_anti_aliasing(fallback)).await
    }

    /// Whether a failure with TAA on may still fall back to MSAA 4x.
    pub(crate) fn taa_may_fall_back(&self) -> bool {
        self.anti_aliasing.taa.is_some() && self.taa.fallback_reason.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parity_and_reset_advance_only_with_a_submitted_resolve() {
        let b = ResolveBookkeeping::new();
        // A new pass: fresh history, writes history[0].
        assert_eq!(b.history_index(), 0);
        assert!(b.history_fresh());

        // Frame 1 records a resolve but fails before its submit: nothing
        // advances. The next frame's upkeep forgets the recording.
        b.begin_frame();
        b.resolve_recorded(true);
        b.begin_frame();
        assert_eq!(b.history_index(), 0);
        assert!(b.history_fresh(), "an unsubmitted reset is redone");

        // A submitted frame WITHOUT a resolve: nothing advances either.
        assert_eq!(b.submitted(), (false, false));
        assert_eq!(b.history_index(), 0);
        assert!(b.history_fresh());

        // A submitted resolve: parity flips, the fresh history is filled,
        // and the depth copy into the pending target is reported once.
        b.begin_frame();
        b.resolve_recorded(true);
        assert_eq!(b.submitted(), (true, true));
        assert_eq!(b.history_index(), 1);
        assert!(!b.history_fresh());
        assert_eq!(b.submitted(), (false, false), "reported once");

        // Steady frames alternate.
        for expect in [0, 1, 0] {
            b.begin_frame();
            b.resolve_recorded(false);
            assert_eq!(b.submitted(), (true, false));
            assert_eq!(b.history_index(), expect);
        }

        // A recreated history is fresh again until a resolve is submitted.
        b.mark_history_fresh();
        b.begin_frame();
        assert!(b.history_fresh());
        b.resolve_recorded(false);
        b.submitted();
        assert!(!b.history_fresh());
    }

    #[test]
    fn params_pack_matches_the_wgsl_struct() {
        // Field order of `struct TaaParams` in taa_wgsl/resolve.wgsl, one
        // f32 each: 22 x 4 = 88 bytes, no implicit padding (vec2 at offsets
        // 0 and 8 are 8-aligned).
        let config = TaaConfig::native();
        let v = TaaParamsValues::new(&config, (1920, 1435), (1920, 1435), 3, true);
        let bytes = v.pack();
        let f = |i: usize| f32::from_ne_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
        let t = &config.tuning;
        assert_eq!(bytes.len(), 88);
        assert_eq!(f(20), crate::taa::CLIP_ALPHA);
        assert_eq!(f(21), 1.0, "exposure (EV 0)");
        assert_eq!(f(16), t.max_history_moving);
        assert_eq!(f(17), t.max_history_moving_speed);
        assert_eq!(f(18), t.motion_edge_alpha);
        assert_eq!(f(19), t.motion_edge_speed);
        assert_eq!((f(0), f(1)), (1920.0, 1435.0), "render_size");
        assert_eq!((f(2), f(3)), (1920.0, 1435.0), "post_size");
        assert_eq!(f(4), t.gamma_still, "native: the still box as tuned");
        assert_eq!(f(5), t.gamma_moving);
        assert_eq!(f(6), t.gamma_moving_speed);
        assert_eq!(f(7), t.disocclusion_threshold);
        assert_eq!(f(8), t.max_history_native, "native picks N_max native");
        assert_eq!(f(9), t.motion_alpha);
        assert_eq!(f(10), t.motion_alpha_speed);
        assert_eq!(f(11), 0.0, "upscaling");
        assert_eq!(f(12), 1.0, "reset");
        assert_eq!(f(13), 3.0, "debug_view");
        assert_eq!(f(14), crate::taa::RECONSTRUCTION_SHARPNESS);
        assert_eq!(f(15), 0.0, "reactive_valid starts unset (set per frame)");

        let up = TaaConfig::preset(crate::taa::TaaPreset::Quality);
        let v = TaaParamsValues::new(&up, (1286, 961), (1920, 1435), 0, false);
        let bytes = v.pack();
        let f = |i: usize| f32::from_ne_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
        assert_eq!((f(0), f(1)), (1286.0, 961.0), "render_size");
        assert_eq!((f(2), f(3)), (1920.0, 1435.0), "post_size");
        // 16 native frames x (1920 / 1286)^2 render samples per output pixel.
        assert_eq!(
            f(8),
            crate::taa::history_cap(&up.tuning, (1286, 961), (1920, 1435))
        );
        assert!(f(8) > 35.0 && f(8) < 36.5, "{}", f(8));
        // The kernel is the reference's, in output pixels: its exponent
        // scales with the squared pixel ratio.
        let ratio = 1920.0f32 / 1286.0;
        assert!((f(14) - crate::taa::RECONSTRUCTION_SHARPNESS * ratio * ratio).abs() < 1e-4);
        assert_eq!(f(11), 1.0);
        assert_eq!(f(12), 0.0);

        // An upscaling preset rendered at native size (scale pinned at 1.0
        // before phase 4) is not upscaling: native N_max, flag off.
        let v = TaaParamsValues::new(&up, (1920, 1435), (1920, 1435), 0, false);
        let bytes = v.pack();
        let f = |i: usize| f32::from_ne_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
        assert_eq!(f(8), up.tuning.max_history_native);
        assert_eq!(f(11), 0.0);
    }

    #[test]
    fn params_struct_in_the_shader_has_sixteen_scalars() {
        // The packing above is only right while the WGSL struct keeps this
        // exact field list; a field added there must be added here.
        let src = include_str!("shader/taa_wgsl/resolve.wgsl");
        let start = src.find("struct TaaParams {").expect("TaaParams struct");
        let body = &src[start..start + src[start..].find("};").expect("struct end")];
        let fields: Vec<&str> = body
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with("//") && l.contains(':'))
            .map(|l| l.split(':').next().unwrap().trim())
            .collect();
        assert_eq!(
            fields,
            [
                "render_size",
                "post_size",
                "gamma_still",
                "gamma_moving",
                "gamma_moving_speed",
                "disocclusion_threshold",
                "max_history",
                "motion_alpha",
                "motion_alpha_speed",
                "upscaling",
                "reset",
                "debug_view",
                "kernel_sharpness",
                "reactive_valid",
                "max_history_moving",
                "max_history_moving_speed",
                "motion_edge_alpha",
                "motion_edge_speed",
                "clip_alpha",
                "exposure",
            ]
        );
    }
}
