//! Bind groups for the per-shader-id edge_resolve / skybox_edge_resolve /
//! final_blend pipelines (Priority 3 in docs/plans/more-optimizations.md).
//!
//! Three bind-group shapes, one per pipeline kind:
//!
//! 1. **EdgeResolveBindGroups** — for `material_edge_resolve_{shader_id}`.
//!    Shares the primary opaque pipeline's group(0) / lights /
//!    texture-pool binding shape, then extends group(3) (shadows) with
//!    two extra bindings at the end carrying the edge buffer
//!    (read-write storage) + the edge-layout uniform. This folds what
//!    was previously a separate group(4) into shadows so the layout
//!    fits in 4 bind groups — required to activate on devices with
//!    `maxBindGroups = 4` (macOS Metal in particular).
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
use crate::render_passes::shared::material::bind_group::shadow_bind_group_layout_entries;
use crate::render_passes::RenderPassInitContext;

/// Bind-group layouts for the MSAA edge-resolve pipelines.
///
/// One field per pipeline-kind layout. All three are buildable up-front
/// (no per-frame variation); the *bind groups* themselves are built
/// lazily via `recreate()` when the edge buffer is first allocated.
pub struct MaterialEdgeBindGroupLayouts {
    /// Layout for the per-shader-id edge_resolve pipelines' **group(3)**
    /// — the shadow bind-group layout extended with the edge buffer
    /// (read-write storage) + edge-layout uniform appended at the end
    /// (bindings 10 and 11).
    ///
    /// The edge_resolve pipeline layout is 4 groups: main(0) / lights(1)
    /// / texture-pool(2) / extended-shadows(3). At dispatch time the
    /// render pass builds the extended shadow bind group fresh each
    /// frame (10 shadow resources + 2 edge resources) and binds it at
    /// slot 3 in place of the primary opaque pipeline's plain shadow
    /// bind group.
    pub edge_resolve_extended_shadows_layout_key: BindGroupLayoutKey,

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
        let edge_resolve_extended_shadows_layout_key = build_extended_shadows_layout(ctx)?;
        let skybox_edge_group0_layout_key = build_skybox_edge_layout(ctx)?;
        let final_blend_group0_layout_key = build_final_blend_layout(ctx)?;

        Ok(Self {
            edge_resolve_extended_shadows_layout_key,
            skybox_edge_group0_layout_key,
            final_blend_group0_layout_key,
        })
    }
}

/// Builds the extended-shadows bind-group layout used by the
/// per-shader-id edge_resolve pipelines at group(3).
///
/// Bindings 0..=9 are the standard shadow entries (must stay
/// byte-for-byte compatible with the opaque shadow layout — same shadow
/// resources are bound here). Bindings 10..=11 are the edge buffer +
/// edge-layout uniform that were previously living in a separate
/// group(4); folding them in here lets the edge_resolve pipeline layout
/// fit in 4 bind groups so it activates on macOS Metal
/// (`maxBindGroups = 4`).
fn build_extended_shadows_layout(
    ctx: &mut RenderPassInitContext<'_>,
) -> Result<BindGroupLayoutKey> {
    let mut entries = shadow_bind_group_layout_entries(true);

    // 10: edge_buffer — storage RW (atomics owned by classify; this
    //                   side writes accumulator slots).
    entries.push(BindGroupLayoutCacheKeyEntry {
        resource: BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::Storage),
        ),
        visibility_vertex: false,
        visibility_fragment: false,
        visibility_compute: true,
    });
    // 11: edge_layout — uniform.
    entries.push(BindGroupLayoutCacheKeyEntry {
        resource: BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
        ),
        visibility_vertex: false,
        visibility_fragment: false,
        visibility_compute: true,
    });

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
