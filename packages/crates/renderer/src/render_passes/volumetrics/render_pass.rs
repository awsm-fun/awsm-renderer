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
///   44 z_far            : f32         (uniform slicing across [z_near, z_far])
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
        // but neither its far plane nor its slicing shape: the culling grid
        // runs to ~10 km to bin distant lights, and it slices exponentially to
        // equalize screen-space error. The volume runs to `volumetric_distance`
        // and slices UNIFORMLY, because the medium's error metric is
        // world-space — see `froxel_slice_view_z`.
        let z_far = atmosphere.volumetric_distance.max(z_near * 2.0);
        d[40..44].copy_from_slice(&z_near.to_ne_bytes());
        d[44..48].copy_from_slice(&z_far.to_ne_bytes());
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

#[cfg(test)]
mod tests {
    /// CPU mirror of `integrate.wgsl`'s per-slice march. Deliberately a
    /// transcription rather than a shared implementation: the shader is the
    /// only place this runs for real, so what is worth pinning is the ALGEBRA
    /// it relies on, not a second copy of the code. Keep the two in step by
    /// hand — the identity below is what breaks loudly if they drift.
    fn march_column(source: f32, extinction: f32, thicknesses: &[f32]) -> (f32, f32) {
        let mut accumulated = 0.0f32;
        let mut transmittance = 1.0f32;
        for &d in thicknesses {
            let slice_transmittance = (-(extinction * d)).exp();
            let slice_scatter = source * (1.0 - slice_transmittance) / extinction.max(1e-6);
            accumulated += transmittance * slice_scatter;
            transmittance *= slice_transmittance;
        }
        (accumulated, transmittance)
    }

    /// The derivation behind the ambient in-scatter weight of exactly 1.0 in
    /// `inject.wgsl`.
    ///
    /// With source `S = color * density` and extinction `sigma_t = density`,
    /// the per-slice term `S/sigma_t * (1 - exp(-sigma_t*d))` telescopes down
    /// the whole column to `color * (1 - T)` — which is the analytic path's
    /// `rgb * T + color * (1 - T)` term, identically. That is what makes the
    /// `fog` and `volumetric` modes describe the SAME medium instead of
    /// disagreeing by 3.5x, which is what they did while air the punctual
    /// lights never reached was a pure absorber.
    ///
    /// Any other weight silently re-tunes every scene's haze relative to the
    /// analytic mode, so the constant is pinned here rather than in prose.
    #[test]
    fn ambient_inscatter_telescopes_to_the_analytic_haze_term() {
        // Deliberately NON-uniform thicknesses, even though the volume now
        // slices uniformly: the identity has to hold for any partition of the
        // ray, and a uniform march would hide a thickness-weighting mistake.
        let thicknesses: Vec<f32> = (0..32)
            .map(|s| 0.1f32 * (1.188f32.powi(s + 1) - 1.188f32.powi(s)))
            .collect();

        for &color in &[0.05f32, 0.45, 0.7, 1.0] {
            for &density in &[0.005f32, 0.02, 0.11, 0.4] {
                let (accumulated, transmittance) =
                    march_column(color * density, density, &thicknesses);
                let analytic = color * (1.0 - transmittance);
                assert!(
                    (accumulated - analytic).abs() < 1e-4,
                    "color={color} density={density}: volumetric column \
                     accumulated {accumulated}, analytic term is {analytic} — \
                     the ambient weight is no longer 1.0, or the slice \
                     integral is no longer energy-conserving"
                );
            }
        }
    }

    /// The march must early-out only where it cannot matter. `integrate.wgsl`
    /// stops once transmittance drops below 0.002 and fills the tail with the
    /// saturated value; this pins that the truncated column is within float
    /// noise of the full one, so the optimisation can't be quietly changing
    /// the image.
    #[test]
    fn early_out_threshold_costs_nothing_visible() {
        let thicknesses: Vec<f32> = (0..32).map(|_| 1.0).collect();
        let (full, _) = march_column(0.5 * 0.4, 0.4, &thicknesses);
        let truncated_len = thicknesses
            .iter()
            .scan(1.0f32, |t, &d| {
                *t *= (-(0.4 * d)).exp();
                Some(*t)
            })
            .position(|t| t < 0.002)
            .map(|i| i + 1)
            .expect("a 32 m column at density 0.4 must saturate");
        let (truncated, _) = march_column(0.5 * 0.4, 0.4, &thicknesses[..truncated_len]);
        assert!(
            (full - truncated).abs() < 0.002,
            "early-out at 0.002 transmittance changed the column from {full} \
             to {truncated}"
        );
    }
}
