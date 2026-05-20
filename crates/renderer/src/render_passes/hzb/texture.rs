//! HZB texture allocation + per-mip views.
//!
//! One `r32float` texture sized to the viewport with a full mip chain
//! (`floor(log2(max(w, h))) + 1` levels). Each mip is bound separately
//! as a storage texture during the build pass — WebGPU requires
//! single-level views for `texture_storage_2d`. A combined sample-side
//! `view_all` is kept around for consumers that want to read across
//! mips with `textureSampleLevel`.

use awsm_renderer_core::{
    error::{AwsmCoreError, Result},
    renderer::AwsmRendererWebGpu,
    texture::{
        Extent3d, TextureDescriptor, TextureFormat, TextureUsage, TextureViewDescriptor,
        TextureViewDimension,
    },
};

/// Owns the HZB texture and the per-mip views the build pass binds.
pub struct HzbTexture {
    pub texture: web_sys::GpuTexture,
    /// Sampling-side view covering every mip level. Consumers read
    /// via `textureSampleLevel(view_all, sampler, uv, lod)`. Linear
    /// sampling across mips is the canonical HZB lookup form.
    pub view_all: web_sys::GpuTextureView,
    /// One single-mip storage view per mip level. Indexed by the mip
    /// level itself; `views_per_mip[0]` is the mip-0 write target
    /// (seeded from the depth buffer), `views_per_mip[N]` is the
    /// mip-N write target of the reduce step.
    pub views_per_mip: Vec<web_sys::GpuTextureView>,
    pub width: u32,
    pub height: u32,
    pub mip_count: u32,
}

impl HzbTexture {
    pub fn new(gpu: &AwsmRendererWebGpu, width: u32, height: u32) -> Result<Self> {
        let width = width.max(1);
        let height = height.max(1);
        let max_dim = width.max(height);
        // `floor(log2(max_dim)) + 1` mips so the chain bottoms out at
        // a 1×1 (or near-1) texel coarse level. `leading_zeros` gives
        // `log2` for power-of-2 inputs and rounds down for non-power-of-2.
        let mip_count = 32u32 - max_dim.leading_zeros();
        let mip_count = mip_count.max(1);

        let texture = gpu
            .create_texture(
                &TextureDescriptor::new(
                    TextureFormat::R32float,
                    Extent3d::new(width, Some(height), Some(1)),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding(),
                )
                .with_label("HZB")
                .with_mip_level_count(mip_count)
                .into(),
            )
            .map_err(AwsmCoreError::from)?;

        let view_all = {
            let descriptor: web_sys::GpuTextureViewDescriptor =
                TextureViewDescriptor::new(Some("HZB All Mips"))
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
                TextureViewDescriptor::new(Some("HZB Mip"))
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
            width,
            height,
            mip_count,
        })
    }

    /// Dimensions of mip `level` (clamped to ≥ 1 on each axis — same
    /// rule WebGPU applies for non-power-of-2 textures).
    pub fn mip_dims(&self, level: u32) -> (u32, u32) {
        let w = (self.width >> level).max(1);
        let h = (self.height >> level).max(1);
        (w, h)
    }
}
