//! SSR min-Z pyramid texture allocation + per-mip views.
//!
//! One `r32float` texture sized to the viewport with a full mip chain
//! (`floor(log2(max(w, h))) + 1` levels). Each mip is bound separately
//! as a storage texture during the build pass — WebGPU requires
//! single-level views for `texture_storage_2d`. A combined sample-side
//! `view_all` is kept around so the SSR trace can descend across mips
//! with `textureLoad(..., lod)`.
//!
//! Structurally identical to `hzb::texture::HzbTexture`; the ONLY
//! difference between the two pyramids is the reduce operator (MAX for
//! the occlusion HZB, MIN here — the nearest occluder is what a
//! reflection ray needs).

use awsm_renderer_core::{
    error::{AwsmCoreError, Result},
    renderer::AwsmRendererWebGpu,
    texture::{
        Extent3d, TextureDescriptor, TextureFormat, TextureUsage, TextureViewDescriptor,
        TextureViewDimension,
    },
};

/// Owns the min-Z pyramid texture and the per-mip views the build pass binds.
pub struct SsrMinzTexture {
    pub texture: web_sys::GpuTexture,
    /// Sampling-side view covering every mip level. The SSR trace reads
    /// via `textureLoad(view_all, coords, lod)` while descending the
    /// pyramid.
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

impl SsrMinzTexture {
    pub fn new(gpu: &AwsmRendererWebGpu, width: u32, height: u32) -> Result<Self> {
        let width = width.max(1);
        let height = height.max(1);
        let max_dim = width.max(height);
        // `floor(log2(max_dim)) + 1` mips so the chain bottoms out at
        // a 1×1 (or near-1) texel coarse level. `leading_zeros` gives
        // `log2` for power-of-2 inputs and rounds down for non-power-of-2.
        let mip_count = 32u32 - max_dim.leading_zeros();
        let mip_count = mip_count.max(1);

        let texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::R32float,
                Extent3d::new(width, Some(height), Some(1)),
                TextureUsage::new()
                    .with_storage_binding()
                    .with_texture_binding(),
            )
            .with_label("SSR MinZ")
            .with_mip_level_count(mip_count)
            .into(),
        )?;

        let view_all = {
            let descriptor: web_sys::GpuTextureViewDescriptor =
                TextureViewDescriptor::new(Some("SSR MinZ All Mips"))
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
                TextureViewDescriptor::new(Some("SSR MinZ Mip"))
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
