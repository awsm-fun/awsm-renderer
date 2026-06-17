//! Compact per-edge-sample shadow-visibility texture (Plan B Stage 5b-shadow,
//! docs/plans/deferred-shared-prep-pass.md).
//!
//! Under MSAA, `cs_prep_edge` fills this texture (per edge pixel × MSAA sample ×
//! `ceil(K/4)` packed slot-layers) and `cs_edge` reads it instead of inline-
//! sampling shadow maps — which is what lets the ~50 KB `sample_shadow_*` block
//! drop from the MSAA opaque module (the MSAA analog of Stage 4).
//!
//! **Why a TEXTURE, not a storage buffer.** The unified MSAA opaque module's
//! `cs_edge` already binds 10 storage buffers (the macOS Metal baseline
//! `maxStorageBuffersPerShaderStage` cap — see `edge_buffers.rs` and
//! `edge_bind_group.rs`). Adding an 11th storage buffer would exceed the cap.
//! A sampled/storage TEXTURE does not count against that limit, so the compact
//! buffer is an `Rgba8unorm` `texture_2d_array`: `cs_prep_edge` writes it as a
//! storage texture, `cs_edge` reads it with `textureLoad` (no sampler).
//!
//! **Keying.** Flat index `idx = edge_pixel_id * MAX_EDGE_SHADOW_SAMPLES +
//! sample`, mapped to 2D as `(idx % EDGE_SHADOW_TEX_WIDTH, idx /
//! EDGE_SHADOW_TEX_WIDTH)`; the packed slot group `slot / 4` selects the array
//! layer. Both `cs_prep_edge` (write) and `cs_edge` (read) compute the identical
//! mapping (the WGSL `EDGE_SHADOW_TEX_WIDTH` const is rendered from the same
//! `EDGE_SHADOW_TEX_WIDTH` value below).
//!
//! **Size.** `EDGE_SHADOW_TEX_WIDTH × height × layers` texels, where `height =
//! ceil(max_edge_budget * MAX_EDGE_SHADOW_SAMPLES / WIDTH)` and `layers =
//! ceil(K/4)`. At the 512K desktop budget, K≤4: 4096 × 512 × 1 × 4 B ≈ 8 MB —
//! the spec's target. Only allocated under prep + MSAA.

use awsm_renderer_core::{
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
    texture::{
        Extent3d, TextureDescriptor, TextureFormat, TextureUsage, TextureViewDescriptor,
        TextureViewDimension,
    },
};

/// Fixed width (texels) of the compact edge-shadow texture. The flat edge-sample
/// index wraps at this width. MUST equal the WGSL `EDGE_SHADOW_TEX_WIDTH` const
/// (rendered from this value via the prep compute template). Chosen so that even
/// the 24-bit `MAX_EDGE_BUDGET` ceiling keeps `height` within `maxTextureDimension2D`
/// for the desktop/mobile defaults.
pub const EDGE_SHADOW_TEX_WIDTH: u32 = 4096;

/// MSAA samples per edge pixel the compact buffer reserves a slot for. MSAA-4
/// today; mirrors the WGSL `MAX_EDGE_SHADOW_SAMPLES`.
pub const MAX_EDGE_SHADOW_SAMPLES: u32 = 4;

/// The compact per-edge-sample shadow-visibility texture + its sampled/array
/// view. Owned by the prep module; allocated only when prep is enabled AND MSAA
/// is on. Sized from `max_edge_budget` × samples × `shadow_visibility_layers`.
pub struct EdgeShadowBuffer {
    pub texture: web_sys::GpuTexture,
    /// Storage-write array view bound to `cs_prep_edge` (`edge_shadow_out`).
    pub storage_view: web_sys::GpuTextureView,
    /// Sampled array view bound to `cs_edge` (read via `textureLoad`).
    pub sampled_view: web_sys::GpuTextureView,
    pub max_edge_budget: u32,
    pub layers: u32,
}

/// Height (rows) needed to hold `max_edge_budget * MAX_EDGE_SHADOW_SAMPLES`
/// texels at `EDGE_SHADOW_TEX_WIDTH` columns.
pub fn edge_shadow_tex_height(max_edge_budget: u32) -> u32 {
    let total = max_edge_budget.saturating_mul(MAX_EDGE_SHADOW_SAMPLES);
    total.div_ceil(EDGE_SHADOW_TEX_WIDTH).max(1)
}

impl EdgeShadowBuffer {
    /// Allocates the compact edge-shadow texture for `max_edge_budget` edges and
    /// `layers = ceil(K/4)` packed slot-groups.
    pub fn new(
        gpu: &AwsmRendererWebGpu,
        max_edge_budget: u32,
        layers: u32,
    ) -> Result<Self, AwsmCoreError> {
        let max_edge_budget = max_edge_budget.max(1);
        let layers = layers.max(1);
        let width = EDGE_SHADOW_TEX_WIDTH;
        let height = edge_shadow_tex_height(max_edge_budget);

        let texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::Rgba8unorm,
                Extent3d::new(width, Some(height), Some(layers)),
                TextureUsage::new()
                    .with_storage_binding()
                    .with_texture_binding(),
            )
            .with_label("PrepEdgeShadow")
            .into(),
        )?;

        let storage_view = texture.create_view_with_descriptor(
            &TextureViewDescriptor::new(Some("PrepEdgeShadow storage"))
                .with_dimension(TextureViewDimension::N2dArray)
                .with_array_layer_count(layers)
                .into(),
        )
        .map_err(AwsmCoreError::create_texture_view)?;
        let sampled_view = texture.create_view_with_descriptor(
            &TextureViewDescriptor::new(Some("PrepEdgeShadow sampled"))
                .with_dimension(TextureViewDimension::N2dArray)
                .with_array_layer_count(layers)
                .into(),
        )
        .map_err(AwsmCoreError::create_texture_view)?;

        Ok(Self {
            texture,
            storage_view,
            sampled_view,
            max_edge_budget,
            layers,
        })
    }
}
