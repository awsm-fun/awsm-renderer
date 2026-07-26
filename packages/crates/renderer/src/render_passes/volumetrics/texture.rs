//! The froxel scattering volume — two `rgba16float` 3D textures over the view
//! frustum.
//!
//! - **scatter** (`inject` writes, `integrate` samples): per-froxel
//!   `rgb = in-scattered radiance`, `a = extinction`. This is the *unintegrated*
//!   medium — what light arrives at that point in the air and how opaque the
//!   air is there.
//! - **integrated** (`integrate` writes, the effects pass samples):
//!   `rgb = accumulated in-scatter from the eye to this slice`,
//!   `a = transmittance to this slice`. Marching stops here; the effects pass
//!   does one trilinear fetch.
//!
//! Two textures rather than one because WebGPU has no `read_write` storage
//! access for `rgba16float`, and a slice can't be both sampled and
//! storage-written in the same dispatch — the same constraint that forces
//! bloom's ping-pong pyramids.
//!
//! **Resolution.** X/Y are the viewport divided by [`FROXEL_TILE_PIXEL_SIZE`]
//! (16 px), which is not a free choice: it makes a froxel column line up
//! exactly with a light-culling tile, so `froxel_base_for_pixel` at the tile
//! centre returns *that* column's light list rather than a neighbour's. Z is
//! [`FROXEL_SLICE_COUNT`] over the same exponential depth mapping the culling
//! grid uses, for the same reason.

use awsm_renderer_core::{
    error::{AwsmCoreError, Result},
    renderer::AwsmRendererWebGpu,
    texture::{
        Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsage,
        TextureViewDescriptor, TextureViewDimension,
    },
};

/// Pixels per froxel column edge. Must equal `FROXEL_TILE_PIXEL_SIZE` in
/// `shared_wgsl/lighting/froxel_walk.wgsl` — the volume and the light-culling
/// grid have to agree on what a tile is, or the medium is lit by the wrong
/// lights.
pub const FROXEL_TILE_PIXEL_SIZE: u32 = 16;

/// Depth slices in the volume. Same count and same exponential mapping as the
/// light-culling grid, so a froxel's light list is the one for its own depth.
pub const FROXEL_SLICE_COUNT: u32 = 32;

/// Owns the scattering + integrated volumes and the views both stages bind.
pub struct VolumetricsTexture {
    scatter: web_sys::GpuTexture,
    /// Storage-write target for `inject`.
    pub scatter_storage_view: web_sys::GpuTextureView,
    /// Sampled source for `integrate`.
    pub scatter_sample_view: web_sys::GpuTextureView,
    integrated: web_sys::GpuTexture,
    /// Storage-write target for `integrate`.
    pub integrated_storage_view: web_sys::GpuTextureView,
    /// Sampled source for the effects pass.
    pub integrated_sample_view: web_sys::GpuTextureView,
    pub width: u32,
    pub height: u32,
}

impl VolumetricsTexture {
    /// Release both volumes. The pass's resize path calls this rather than
    /// waiting on JS GC — at 16 px froxels these are a few MB each.
    pub fn destroy(self) {
        self.scatter.destroy();
        self.integrated.destroy();
    }

    pub fn new(gpu: &AwsmRendererWebGpu, view_width: u32, view_height: u32) -> Result<Self> {
        let (width, height) = Self::dims_for(view_width, view_height);
        let (scatter, scatter_storage_view, scatter_sample_view) =
            create_volume(gpu, "Volumetrics Scatter", width, height)?;
        let (integrated, integrated_storage_view, integrated_sample_view) =
            create_volume(gpu, "Volumetrics Integrated", width, height)?;
        Ok(Self {
            scatter,
            scatter_storage_view,
            scatter_sample_view,
            integrated,
            integrated_storage_view,
            integrated_sample_view,
            width,
            height,
        })
    }

    /// Froxel-grid X/Y for a viewport — the tile count the light culling would
    /// compute for the same viewport, so the two grids stay aligned.
    pub fn dims_for(view_width: u32, view_height: u32) -> (u32, u32) {
        (
            view_width.div_ceil(FROXEL_TILE_PIXEL_SIZE).max(1),
            view_height.div_ceil(FROXEL_TILE_PIXEL_SIZE).max(1),
        )
    }

    /// Re-allocates to match the viewport. Returns `true` when new textures
    /// were created, so the caller can mark the dependent bind groups dirty.
    ///
    /// Compares the DERIVED grid dims, not the viewport: many viewport widths
    /// map to the same tile count, and rebuilding the volume (plus every bind
    /// group that references it) on a resize that didn't change the grid is
    /// the per-frame-rebuild bug bloom's `ensure_size` documents.
    pub fn ensure_size(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        view_width: u32,
        view_height: u32,
    ) -> Result<bool> {
        let (width, height) = Self::dims_for(view_width, view_height);
        if self.width == width && self.height == height {
            return Ok(false);
        }
        let old = std::mem::replace(self, Self::new(gpu, view_width, view_height)?);
        old.destroy();
        Ok(true)
    }
}

/// One `rgba16float` 3D volume plus its storage-write and sampled views.
fn create_volume(
    gpu: &AwsmRendererWebGpu,
    label: &str,
    width: u32,
    height: u32,
) -> Result<(
    web_sys::GpuTexture,
    web_sys::GpuTextureView,
    web_sys::GpuTextureView,
)> {
    let texture = gpu.create_texture(
        &TextureDescriptor::new(
            TextureFormat::Rgba16float,
            Extent3d::new(width, Some(height), Some(FROXEL_SLICE_COUNT)),
            TextureUsage::new()
                .with_storage_binding()
                .with_texture_binding(),
        )
        // WITHOUT this the texture is 2D (the descriptor's default) and every
        // 3D view of it is invalid — a mismatch nothing catches natively, since
        // naga validates the shader and not the texture/view/layout agreement.
        .with_dimension(TextureDimension::N3d)
        .with_label(label)
        .into(),
    )?;

    let view = |name: &str| -> Result<web_sys::GpuTextureView> {
        let descriptor: web_sys::GpuTextureViewDescriptor = TextureViewDescriptor::new(Some(name))
            .with_dimension(TextureViewDimension::N3d)
            .into();
        texture
            .create_view_with_descriptor(&descriptor)
            .map_err(AwsmCoreError::create_texture_view)
    };

    let storage_view = view(label)?;
    let sample_view = view(label)?;
    Ok((texture, storage_view, sample_view))
}
