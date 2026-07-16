//! Bloom mip-pyramid texture allocation + per-mip views.
//!
//! TWO `rgba16float` textures sized to the viewport with a capped mip chain
//! (ping-pong pyramids):
//! - the **down** pyramid (`texture`): the prefiltered scene downsampled
//!   through the chain (mip N-1 → mip N).
//! - the **up** pyramid (`texture_up`): the progressive tent-filter upsample
//!   accumulation, `up[N-1] = down[N-1] + scatter · tent9(mip N)`, walked
//!   coarsest → finest. A second texture is required because WebGPU has no
//!   `read_write` storage access for `rgba16float`, and a single mip cannot
//!   be both sampled and storage-written inside one dispatch — but sampling
//!   one texture while storage-writing another (or a DISJOINT mip of the
//!   same texture, as the downsample chain does) is fine.
//!
//! WebGPU requires single-level views for `texture_storage_2d`, so each
//! pyramid keeps one `views_per_mip` entry per level plus a `view_all`
//! sample-side view.
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

/// Owns the bloom down/up pyramid textures + the per-mip views the
/// down/up-sample passes bind.
pub struct BloomTexture {
    #[allow(dead_code)]
    texture: web_sys::GpuTexture,
    /// Down-pyramid sample-side view spanning every mip.
    pub view_all: web_sys::GpuTextureView,
    /// One single-mip view per down-pyramid level. `views_per_mip[n]` is the
    /// storage-write target for level `n` and the sampled source when level
    /// `n+1` (downsample) or the upsample accumulation reads it.
    pub views_per_mip: Vec<web_sys::GpuTextureView>,
    #[allow(dead_code)]
    texture_up: web_sys::GpuTexture,
    /// Up-pyramid sample-side view spanning every mip. Only mip 0 is ever
    /// sampled (by the combine), but the all-mips view keeps
    /// `textureNumLevels` == the number of contributing pyramid levels, which
    /// the combine's scatter-weight normalization relies on.
    pub up_view_all: web_sys::GpuTextureView,
    /// One single-mip view per up-pyramid level. `up_views_per_mip[n]` is the
    /// storage-write target for upsample step `n+1 → n` and the sampled
    /// coarse source for step `n → n-1`. The coarsest level is never written
    /// (the first upsample step samples the DOWN pyramid's coarsest mip); it
    /// is allocated anyway so both pyramids share `mip_count`.
    pub up_views_per_mip: Vec<web_sys::GpuTextureView>,
    /// mip 0 dimensions = half the viewport (rounded up).
    pub base_width: u32,
    pub base_height: u32,
    pub mip_count: u32,
}

impl BloomTexture {
    /// Release both GPU mip pyramids. Called by the pass's resize path —
    /// the texture handles are otherwise only dropped via JS GC.
    pub fn destroy(self) {
        self.texture.destroy();
        self.texture_up.destroy();
    }

    pub fn new(gpu: &AwsmRendererWebGpu, view_width: u32, view_height: u32) -> Result<Self> {
        // mip 0 is half-res (the classic bloom prefilter downsample).
        let base_width = crate::size::half_extent(view_width);
        let base_height = crate::size::half_extent(view_height);
        let max_dim = base_width.max(base_height);
        let full_chain = (32u32 - max_dim.leading_zeros()).max(1);
        // full_chain is already ≥ 1 and BLOOM_MAX_MIPS ≥ 1, so this is a plain
        // upper cap (clamp keeps clippy happy about the min/max pattern).
        let mip_count = full_chain.clamp(1, BLOOM_MAX_MIPS);

        let (texture, view_all, views_per_mip) =
            create_pyramid(gpu, "Bloom Pyramid", base_width, base_height, mip_count)?;
        let (texture_up, up_view_all, up_views_per_mip) =
            create_pyramid(gpu, "Bloom Up Pyramid", base_width, base_height, mip_count)?;

        Ok(Self {
            texture,
            view_all,
            views_per_mip,
            texture_up,
            up_view_all,
            up_views_per_mip,
            base_width,
            base_height,
            mip_count,
        })
    }

    /// Dimensions of pyramid mip `level` (level 0 = half-viewport), clamped ≥ 1.
    pub fn mip_dims(&self, level: u32) -> (u32, u32) {
        awsm_renderer_core::texture::mipmap::get_mipmap_size_for_level(
            self.base_width,
            self.base_height,
            level,
        )
    }
}

/// Allocates one `rgba16float` mip-pyramid texture plus its all-mips view and
/// single-level per-mip views (shared by the down and up pyramids).
#[allow(clippy::type_complexity)]
fn create_pyramid(
    gpu: &AwsmRendererWebGpu,
    label: &str,
    base_width: u32,
    base_height: u32,
    mip_count: u32,
) -> Result<(
    web_sys::GpuTexture,
    web_sys::GpuTextureView,
    Vec<web_sys::GpuTextureView>,
)> {
    let texture = gpu.create_texture(
        &TextureDescriptor::new(
            TextureFormat::Rgba16float,
            Extent3d::new(base_width, Some(base_height), Some(1)),
            TextureUsage::new()
                .with_storage_binding()
                .with_texture_binding(),
        )
        .with_label(label)
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

    Ok((texture, view_all, views_per_mip))
}
