//! Bind groups for the per-shader-id edge_resolve / skybox_edge_resolve /
//! final_blend pipelines (Priority 3 in docs/plans/more-optimizations.md).
//!
//! Three bind-group shapes, one per pipeline kind:
//!
//! 1. **EdgeResolveBindGroups** — for `material_edge_resolve_{shader_id}`.
//!    Shares the primary opaque pipeline's group(0)/lights/textures/shadows
//!    binding shape, then adds a group(4) carrying the edge buffer (read-write
//!    storage) + the edge-layout uniform.
//!
//! 2. **SkyboxEdgeResolveBindGroups** — for `skybox_edge_resolve`.
//!    Compact: just the edge buffer + layout + camera + skybox tex/sampler.
//!
//! 3. **FinalBlendBindGroups** — for `final_blend`.
//!    Compact: read-only edge buffer + layout + the opaque storage tex.
//!
//! All three bind-group layouts are constructed lazily — when the edge
//! pipelines are first compiled (post-cold-boot, per the scheduler-managed
//! lifecycle). Cached on `MaterialEdgeBindGroups` for subsequent use.

use awsm_renderer_core::bind_groups::{
    BindGroupLayoutResource, BufferBindingLayout, BufferBindingType, SamplerBindingLayout,
    SamplerBindingType, StorageTextureAccess, StorageTextureBindingLayout, TextureBindingLayout,
};
use awsm_renderer_core::texture::{TextureSampleType, TextureViewDimension};

use crate::bind_group_layout::BindGroupLayoutKey;
use crate::bind_group_layout::{BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry};
use crate::error::Result;
use crate::render_passes::RenderPassInitContext;

/// Bind-group layouts for the MSAA edge-resolve pipelines.
///
/// One field per pipeline-kind layout. All three are buildable up-front
/// (no per-frame variation); the *bind groups* themselves are built
/// lazily via `recreate()` when the edge buffer is first allocated.
pub struct MaterialEdgeBindGroupLayouts {
    /// Layout for the per-shader-id edge_resolve pipelines.
    /// Composed of (group(0): primary-opaque-main, group(1): lights,
    /// group(2): texture-pool, group(3): shadows, group(4):
    /// edge_buffer + edge_layout).
    ///
    /// Stored as the standalone group(4) layout key — the other 4
    /// groups are reused from `MaterialOpaqueBindGroups` and bound at
    /// dispatch time. Edge_resolve pipeline layout = the existing
    /// opaque pipeline layout extended with this key.
    pub edge_resolve_group4_layout_key: BindGroupLayoutKey,

    /// Layout for the global skybox_edge_resolve pipeline (single
    /// bind group at group(0)).
    pub skybox_edge_group0_layout_key: BindGroupLayoutKey,

    /// Layout for the global final_blend pipeline (single bind
    /// group at group(0)).
    pub final_blend_group0_layout_key: BindGroupLayoutKey,
}

impl MaterialEdgeBindGroupLayouts {
    /// Constructs the three layouts. Sync-cached in
    /// `BindGroupLayouts`; cheap on subsequent calls with the same
    /// renderer config.
    pub fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let edge_resolve_group4_layout_key = build_edge_resolve_group4_layout(ctx)?;
        let skybox_edge_group0_layout_key = build_skybox_edge_layout(ctx)?;
        let final_blend_group0_layout_key = build_final_blend_layout(ctx)?;

        Ok(Self {
            edge_resolve_group4_layout_key,
            skybox_edge_group0_layout_key,
            final_blend_group0_layout_key,
        })
    }
}

fn build_edge_resolve_group4_layout(
    ctx: &mut RenderPassInitContext<'_>,
) -> Result<BindGroupLayoutKey> {
    // 0: edge_buffer — storage RW (atomics owned by classify;
    //                  this side writes accumulator slots).
    // 1: edge_layout — uniform.
    let entries = vec![
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::Storage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
    ];
    Ok(ctx
        .bind_group_layouts
        .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?)
}

fn build_skybox_edge_layout(ctx: &mut RenderPassInitContext<'_>) -> Result<BindGroupLayoutKey> {
    // 0: edge_buffer — storage RW
    // 1: edge_layout — uniform
    // 2: camera_raw  — uniform (vec4 array)
    // 3: skybox_tex  — texture_cube
    // 4: skybox_smp  — sampler
    let entries = vec![
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::Storage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Texture(
                TextureBindingLayout::new().with_view_dimension(TextureViewDimension::Cube),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Sampler(
                SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Filtering),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
    ];
    Ok(ctx
        .bind_group_layouts
        .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?)
}

fn build_final_blend_layout(ctx: &mut RenderPassInitContext<'_>) -> Result<BindGroupLayoutKey> {
    // 0: edge_buffer  — storage RO
    // 1: edge_layout  — uniform
    // 2: opaque_tex   — storage texture (write)
    let entries = vec![
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::StorageTexture(
                StorageTextureBindingLayout::new(ctx.render_texture_formats.color)
                    .with_view_dimension(TextureViewDimension::N2d)
                    .with_access(StorageTextureAccess::WriteOnly),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
    ];
    let _ = TextureSampleType::UnfilterableFloat; // suppress unused-import warning
    Ok(ctx
        .bind_group_layouts
        .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?)
}
