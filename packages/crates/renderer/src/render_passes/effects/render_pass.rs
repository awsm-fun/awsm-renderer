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

/// `AtmosphereParams` — 48-byte uniform: `color` (vec3), `density`,
/// `base_height`, `height_falloff`, then the froxel grid's depth mapping
/// (`slice_count`, `z_near`, `log_far_over_near`) used only by the volumetric
/// composite, plus tail padding to the vec4 alignment WGSL gives the struct.
///
/// The froxel numbers are COPIED from `LightCullingBuffers::froxel_depth`
/// rather than re-derived: the effects pass has to map a pixel's depth back to
/// the same slice the volumetrics pass wrote, and two derivations of an
/// exponential mapping that disagree by a hair misregister the whole volume.
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
    pub const BYTE_SIZE: usize = 48;

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
            &defaults,
            crate::render_passes::light_culling::buffers::FroxelDepthRange::default(),
        );
        Ok(params)
    }

    fn pack(
        &mut self,
        atmosphere: &crate::post_process::Atmosphere,
        froxel: crate::render_passes::light_culling::buffers::FroxelDepthRange,
    ) {
        let d = &mut self.raw_data;
        d[0..4].copy_from_slice(&atmosphere.color[0].to_ne_bytes());
        d[4..8].copy_from_slice(&atmosphere.color[1].to_ne_bytes());
        d[8..12].copy_from_slice(&atmosphere.color[2].to_ne_bytes());
        d[12..16].copy_from_slice(&atmosphere.density.to_ne_bytes());
        d[16..20].copy_from_slice(&atmosphere.base_height.to_ne_bytes());
        d[20..24].copy_from_slice(&atmosphere.height_falloff.to_ne_bytes());
        d[24..28].copy_from_slice(
            &(crate::render_passes::volumetrics::texture::FROXEL_SLICE_COUNT as f32).to_ne_bytes(),
        );
        // The VOLUME's range, not the light grid's — the composite maps a
        // pixel's depth back to the slice the volumetrics pass wrote, so it
        // has to use that pass's mapping.
        let z_far = atmosphere.volumetric_distance.max(froxel.z_near * 2.0);
        d[28..32].copy_from_slice(&froxel.z_near.to_ne_bytes());
        d[32..36].copy_from_slice(&(z_far / froxel.z_near.max(f32::EPSILON)).ln().to_ne_bytes());
    }

    /// Packs + uploads via the mapped-ring path, skipping the GPU write when
    /// the bytes are unchanged — same house standard as `BloomParams`: these
    /// only move on user edits, so an every-frame upload while haze is merely
    /// ENABLED is pure idle work.
    pub fn write(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        atmosphere: &crate::post_process::Atmosphere,
        froxel: crate::render_passes::light_culling::buffers::FroxelDepthRange,
    ) -> Result<()> {
        let prev = self.raw_data;
        self.pack(atmosphere, froxel);
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
