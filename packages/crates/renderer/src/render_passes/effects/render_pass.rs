//! Effects render pass execution.

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    command::compute_pass::ComputePassDescriptor,
    renderer::AwsmRendererWebGpu,
};

use crate::{
    buffer::mapped_uploader::MappedUploader,
    error::Result,
    render::RenderContext,
    render_passes::{
        effects::{
            bind_group::EffectsBindGroups, pipeline::EffectsPipelines,
            shader::cache_key::BloomPhase,
        },
        RenderPassInitContext,
    },
};

/// `AtmosphereParams` — 32-byte uniform: `color` (vec3), `density`,
/// `base_height`, `height_falloff`, 8 bytes of tail padding to the vec4
/// alignment WGSL gives the struct.
///
/// Unlike `BloomParams` (which lives on the lazily-built `BloomRenderPass`)
/// this buffer is owned by the effects pass and therefore ALWAYS exists — the
/// bind-group layout carries the binding whether or not haze is on, keeping
/// one layout shape across the toggle exactly like the 1×1 SMAA dummy. Only
/// the compiled shader varies; the haze-off variant simply never reads it.
pub struct AtmosphereParams {
    pub gpu_buffer: web_sys::GpuBuffer,
    raw_data: [u8; Self::BYTE_SIZE],
    uploader: MappedUploader,
}

impl AtmosphereParams {
    pub const BYTE_SIZE: usize = 32;

    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let gpu_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("AtmosphereParams"),
                Self::BYTE_SIZE,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;

        let mut params = Self {
            gpu_buffer,
            raw_data: [0; Self::BYTE_SIZE],
            uploader: MappedUploader::new("AtmosphereParams"),
        };
        // Seed from the config defaults so the first frame after an enable
        // renders the authored haze even if the per-frame write is skipped.
        let defaults = crate::post_process::Atmosphere::default();
        params.pack(
            defaults.color,
            defaults.density,
            defaults.base_height,
            defaults.height_falloff,
        );
        Ok(params)
    }

    fn pack(&mut self, color: [f32; 3], density: f32, base_height: f32, height_falloff: f32) {
        self.raw_data[0..4].copy_from_slice(&color[0].to_ne_bytes());
        self.raw_data[4..8].copy_from_slice(&color[1].to_ne_bytes());
        self.raw_data[8..12].copy_from_slice(&color[2].to_ne_bytes());
        self.raw_data[12..16].copy_from_slice(&density.to_ne_bytes());
        self.raw_data[16..20].copy_from_slice(&base_height.to_ne_bytes());
        self.raw_data[20..24].copy_from_slice(&height_falloff.to_ne_bytes());
    }

    /// Packs + uploads via the mapped-ring path, skipping the GPU write when
    /// the bytes are unchanged — same house standard as `BloomParams`: these
    /// only move on user edits, so an every-frame upload while haze is merely
    /// ENABLED is pure idle work.
    pub fn write(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        color: [f32; 3],
        density: f32,
        base_height: f32,
        height_falloff: f32,
    ) -> Result<()> {
        let prev = self.raw_data;
        self.pack(color, density, base_height, height_falloff);
        if self.raw_data == prev {
            return Ok(());
        }
        self.uploader.write_dirty_ranges(
            gpu,
            &self.gpu_buffer,
            Self::BYTE_SIZE,
            self.raw_data.as_slice(),
            &[(0, Self::BYTE_SIZE)],
        )?;
        Ok(())
    }
}

/// Effects pass bind groups and pipelines.
pub struct EffectsRenderPass {
    pub bind_groups: EffectsBindGroups,
    pub pipelines: EffectsPipelines,
    /// Live atmospheric-haze uniform (colour / density / heights).
    pub atmosphere_params: AtmosphereParams,
}

impl EffectsRenderPass {
    /// Creates the effects render pass resources.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = EffectsBindGroups::new(ctx).await?;
        let pipelines = EffectsPipelines::new(ctx, &bind_groups).await?;
        let atmosphere_params = AtmosphereParams::new(ctx.gpu)?;

        Ok(Self {
            bind_groups,
            pipelines,
            atmosphere_params,
        })
    }

    /// Executes the effects pass.
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        // PR #103 review note: T1.5 originally attempted to early-exit
        // here when bloom/dof/smaa are all off, on the theory that the
        // `BloomPhase::None` dispatch is a no-op. It isn't. The shader
        // unconditionally does `textureLoad(composite_tex)` followed
        // by `textureStore(effects_tex, …)` — i.e. it's the only thing
        // that puts pixels into `effects_tex`, which the display pass
        // then samples at binding 0 (see `display/bind_group.rs:99`).
        //
        // Skipping the dispatch left `effects_tex` with stale /
        // driver-defined contents and the display pass showed garbage
        // (or last-known-effects-frame contents) on every frame where
        // no effects were enabled. Reinstating the dispatch is the
        // safe path. The bandwidth cost is small (~5 MB at 400×800)
        // and the only theoretical win was the compute-pass
        // open/close overhead (~30 µs on mobile), recovering which
        // would require plumbing post-processing state into the
        // display bind-group recreation flow so display could sample
        // `composite` directly — disproportionate complexity for the
        // saving. Re-evaluate in a follow-up if profiling justifies.
        let workgroup_size = (
            ctx.render_texture_views.width.div_ceil(8),
            ctx.render_texture_views.height.div_ceil(8),
        );

        if ctx.post_processing.bloom {
            // The wide bloom is built by the dedicated `BloomRenderPass`
            // (COD-style mip pyramid) into `render_texture_views.bloom` BEFORE
            // this pass runs. The effects pass only BLENDS it over the scene —
            // it samples `bloom_tex` and writes `effects_tex`, which the
            // display pass then reads.
            self.dispatch_pass(ctx, BloomPhase::Blend, workgroup_size)?;
        } else {
            // Single pass for other effects only (SMAA, DoF)
            self.dispatch_pass(ctx, BloomPhase::None, workgroup_size)?;
        }

        Ok(())
    }

    fn dispatch_pass(
        &self,
        ctx: &RenderContext,
        phase: BloomPhase,
        workgroup_size: (u32, u32),
    ) -> Result<()> {
        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Effects Pass"))
                .with_timestamp_writes_opt(
                    ctx.gpu_timestamps
                        .and_then(|t| t.writes_for_compute("Effects")),
                )
                .into(),
        ));

        compute_pass.set_bind_group(0, self.bind_groups.get_bind_group()?, None)?;

        if let Some(pipeline_key) = self.pipelines.get_bloom_pipeline(phase) {
            compute_pass.set_pipeline(ctx.pipelines.compute.get(pipeline_key)?);
            compute_pass.dispatch_workgroups(workgroup_size.0, Some(workgroup_size.1), Some(1));
        }

        compute_pass.end();

        Ok(())
    }
}
