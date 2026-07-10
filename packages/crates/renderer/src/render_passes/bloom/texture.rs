//! Bloom mip-pyramid texture allocation + per-mip views.
//!
//! One `rgba16float` texture sized to the viewport with a capped mip chain.
//! COD/Jimenez-style bloom downsamples the prefiltered scene through this
//! pyramid then upsamples back additively, so each mip is bound BOTH as a
//! sampled source (previous level) and a storage-write target (this level) —
//! WebGPU requires single-level views for `texture_storage_2d`, so we keep one
//! `views_per_mip` entry per level plus a `view_all` sample-side view.
//!
//! The chain is capped (`BLOOM_MAX_MIPS`) rather than run to 1×1: ~6–7 levels
//! already spread the glow across a large fraction of the screen, and the
//! coarsest levels contribute the wide, soft halo.

use awsm_renderer_core::{
    error::{AwsmCoreError, Result},
    renderer::AwsmRendererWebGpu,
    texture::{
        Extent3d, TextureDescriptor, TextureFormat, TextureUsage, TextureViewDescriptor,
        TextureViewDimension,
    },
};

/// Max levels in the bloom pyramid. mip 0 is HALF the viewport (the prefilter
/// downsamples ×2), so N levels reach 1/2^(N) of the viewport.
pub const BLOOM_MAX_MIPS: u32 = 6;

/// Owns the bloom pyramid texture + the per-mip views the down/up-sample
/// passes bind.
pub struct BloomTexture {
    #[allow(dead_code)]
    texture: web_sys::GpuTexture,
    /// Sample-side view spanning every mip (`textureSampleLevel` across mips).
    pub view_all: web_sys::GpuTextureView,
    /// One single-mip view per level. `views_per_mip[n]` is the storage-write
    /// target for level `n` and the sampled source when level `n+1`/`n-1`
    /// reads it.
    pub views_per_mip: Vec<web_sys::GpuTextureView>,
    /// mip 0 dimensions = half the viewport (rounded up).
    pub base_width: u32,
    pub base_height: u32,
    pub mip_count: u32,
}

impl BloomTexture {
    pub fn new(gpu: &AwsmRendererWebGpu, view_width: u32, view_height: u32) -> Result<Self> {
        // mip 0 is half-res (the classic bloom prefilter downsample).
        let base_width = (view_width / 2).max(1);
        let base_height = (view_height / 2).max(1);
        let max_dim = base_width.max(base_height);
        let full_chain = (32u32 - max_dim.leading_zeros()).max(1);
        // full_chain is already ≥ 1 and BLOOM_MAX_MIPS ≥ 1, so this is a plain
        // upper cap (clamp keeps clippy happy about the min/max pattern).
        let mip_count = full_chain.clamp(1, BLOOM_MAX_MIPS);

        let texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::Rgba16float,
                Extent3d::new(base_width, Some(base_height), Some(1)),
                TextureUsage::new()
                    .with_storage_binding()
                    .with_texture_binding(),
            )
            .with_label("Bloom Pyramid")
            .with_mip_level_count(mip_count)
            .into(),
        )?;

        let view_all = {
            let descriptor: web_sys::GpuTextureViewDescriptor =
                TextureViewDescriptor::new(Some("Bloom All Mips"))
                    .with_dimension(TextureViewDimension::N2d)
                    .with_mip_level_count(mip_count)
                    .into();
            texture
                .create_view_with_descriptor(&descriptor)
                .map_err(AwsmCoreError::create_texture_view)?
        };

        let mut views_per_mip = Vec::with_capacity(mip_count as usize);
        for mip in 0..mip_count {
            let descriptor: web_sys::GpuTextureViewDescriptor =
                TextureViewDescriptor::new(Some("Bloom Mip"))
                    .with_dimension(TextureViewDimension::N2d)
                    .with_base_mip_level(mip)
                    .with_mip_level_count(1)
                    .into();
            let view = texture
                .create_view_with_descriptor(&descriptor)
                .map_err(AwsmCoreError::create_texture_view)?;
            views_per_mip.push(view);
        }

        Ok(Self {
            texture,
            view_all,
            views_per_mip,
            base_width,
            base_height,
            mip_count,
        })
    }

    /// Dimensions of pyramid mip `level` (level 0 = half-viewport), clamped ≥ 1.
    pub fn mip_dims(&self, level: u32) -> (u32, u32) {
        let w = (self.base_width >> level).max(1);
        let h = (self.base_height >> level).max(1);
        (w, h)
    }
}
