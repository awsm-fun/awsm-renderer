//! Froxel volumetrics pass execution.
//!
//! Two compute dispatches in one pass:
//! 1. **inject** — one invocation per froxel. Evaluates the medium at the
//!    froxel centre (the same exponential height profile the analytic haze
//!    integrates along a ray, sampled pointwise here) and walks that froxel's
//!    lights through `froxel_walk`, accumulating
//!    `radiance · attenuation · shadow · phase · volumetric_intensity`.
//! 2. **integrate** — one invocation per froxel COLUMN, marching front to back
//!    down the slices and accumulating transmittance.
//!
//! Both coalesce into a single `begin_compute_pass`; WebGPU inserts the
//! storage-write→sample barrier between them.

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
        volumetrics::{
            bind_group::VolumetricsBindGroups,
            pipeline::VolumetricsPipelines,
            texture::{VolumetricsTexture, FROXEL_SLICE_COUNT},
        },
        RenderPassInitContext,
    },
};

/// `VolumetricParams` — 48-byte uniform describing the medium and the grid.
///
///   0  scattering_color : vec3<f32>   (the medium's albedo/tint)
///   12 density          : f32
///   16 base_height      : f32
///   20 height_falloff   : f32
///   24 anisotropy       : f32         (Henyey-Greenstein g)
///   28 slice_count      : f32
///   32 grid_size        : vec2<u32>   (froxel columns in x, y)
///   40 z_near           : f32
///   44 log_far_over_near: f32
///
/// The medium fields deliberately mirror `Atmosphere` one for one: the
/// volumetric path renders the SAME air as the analytic path, only integrated
/// differently. A second, independently-tuned medium description here would be
/// a way for the two paths to silently disagree.
pub struct VolumetricParams {
    pub gpu_buffer: web_sys::GpuBuffer,
    raw_data: [u8; Self::BYTE_SIZE],
    uploader: MappedUploader,
}

impl VolumetricParams {
    pub const BYTE_SIZE: usize = 48;

    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let gpu_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("VolumetricParams"),
                Self::BYTE_SIZE,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;
        let defaults = crate::post_process::Atmosphere::default();
        let mut params = Self {
            gpu_buffer,
            raw_data: [0; Self::BYTE_SIZE],
            uploader: MappedUploader::new("VolumetricParams"),
        };
        params.pack(
            &defaults,
            1,
            1,
            crate::render_passes::light_culling::buffers::FroxelDepthRange::default().z_near,
        );
        Ok(params)
    }

    fn pack(
        &mut self,
        atmosphere: &crate::post_process::Atmosphere,
        grid_x: u32,
        grid_y: u32,
        z_near: f32,
    ) {
        let d = &mut self.raw_data;
        d[0..4].copy_from_slice(&atmosphere.color[0].to_ne_bytes());
        d[4..8].copy_from_slice(&atmosphere.color[1].to_ne_bytes());
        d[8..12].copy_from_slice(&atmosphere.color[2].to_ne_bytes());
        d[12..16].copy_from_slice(&atmosphere.density.to_ne_bytes());
        d[16..20].copy_from_slice(&atmosphere.base_height.to_ne_bytes());
        d[20..24].copy_from_slice(&atmosphere.height_falloff.to_ne_bytes());
        // Clamped short of ±1: the Henyey-Greenstein denominator collapses to
        // zero at the extremes and the phase function goes to infinity along
        // the axis, which shows up as a single blown-out froxel rather than a
        // beam.
        d[24..28].copy_from_slice(
            &atmosphere
                .scattering_anisotropy
                .clamp(-0.95, 0.95)
                .to_ne_bytes(),
        );
        d[28..32].copy_from_slice(&(FROXEL_SLICE_COUNT as f32).to_ne_bytes());
        d[32..36].copy_from_slice(&grid_x.to_ne_bytes());
        d[36..40].copy_from_slice(&grid_y.to_ne_bytes());
        // The volume's OWN depth range. It shares the light grid's near plane
        // and slicing SHAPE, but not its far plane — the culling grid runs to
        // ~10 km to bin distant lights, and 32 slices over that make the far
        // ones kilometres thick, saturating to solid haze.
        let z_far = atmosphere.volumetric_distance.max(z_near * 2.0);
        d[40..44].copy_from_slice(&z_near.to_ne_bytes());
        d[44..48].copy_from_slice(&(z_far / z_near.max(f32::EPSILON)).ln().to_ne_bytes());
    }

    /// Packs + uploads, skipping the GPU write when nothing moved — same house
    /// standard as `BloomParams`.
    pub fn write(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        atmosphere: &crate::post_process::Atmosphere,
        grid_x: u32,
        grid_y: u32,
        z_near: f32,
    ) -> Result<()> {
        let prev = self.raw_data;
        self.pack(atmosphere, grid_x, grid_y, z_near);
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

pub struct VolumetricsRenderPass {
    pub bind_groups: VolumetricsBindGroups,
    pub pipelines: VolumetricsPipelines,
    pub texture: VolumetricsTexture,
    pub params: VolumetricParams,
}

impl VolumetricsRenderPass {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = VolumetricsBindGroups::new(ctx).await?;
        let reverse_z = ctx.features.reverse_z;
        let pipelines = VolumetricsPipelines::new(ctx, &bind_groups, reverse_z).await?;
        // Tiny initial allocation; the per-frame resize hook grows it to the
        // live viewport before the first dispatch (mirrors bloom).
        let texture = VolumetricsTexture::new(ctx.gpu, 1, 1)?;
        let params = VolumetricParams::new(ctx.gpu)?;
        Ok(Self {
            bind_groups,
            pipelines,
            texture,
            params,
        })
    }

    /// Re-allocates the volume to match the viewport. `true` ⇒ new textures,
    /// so the caller marks dependent bind groups dirty.
    pub fn ensure_size(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        view_width: u32,
        view_height: u32,
    ) -> Result<bool> {
        Ok(self.texture.ensure_size(gpu, view_width, view_height)?)
    }

    /// Injects then integrates the volume for this frame.
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Volumetrics"))
                .with_timestamp_writes_opt(
                    ctx.gpu_timestamps
                        .and_then(|t| t.writes_for_compute("Volumetrics")),
                )
                .into(),
        ));

        let (gx, gy) = (self.texture.width, self.texture.height);

        // Inject — one invocation per froxel. 4×4×4 workgroups: the light walk
        // is per-froxel and coherent within a column, so keeping some z in the
        // group shares the froxel light-list fetch across slices.
        compute_pass.set_pipeline(ctx.pipelines.compute.get(self.pipelines.inject)?);
        compute_pass.set_bind_group(0, self.bind_groups.inject()?, None)?;
        compute_pass.set_bind_group(1, self.bind_groups.lights()?, None)?;
        compute_pass.set_bind_group(2, self.bind_groups.shadows()?, None)?;
        compute_pass.dispatch_workgroups(
            gx.div_ceil(4),
            Some(gy.div_ceil(4)),
            Some(FROXEL_SLICE_COUNT.div_ceil(4)),
        );

        // Integrate — one invocation per COLUMN (each marches all slices), so
        // this is a 2D dispatch over the grid, not a 3D one over froxels.
        compute_pass.set_pipeline(ctx.pipelines.compute.get(self.pipelines.integrate)?);
        compute_pass.set_bind_group(0, self.bind_groups.integrate()?, None)?;
        compute_pass.set_bind_group(1, self.bind_groups.lights()?, None)?;
        compute_pass.set_bind_group(2, self.bind_groups.shadows()?, None)?;
        compute_pass.dispatch_workgroups(gx.div_ceil(8), Some(gy.div_ceil(8)), Some(1));

        compute_pass.end();
        Ok(())
    }
}
