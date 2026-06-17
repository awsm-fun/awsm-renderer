//! Bind groups for the unified-edge `cs_shade` + global `final_blend`
//! pipelines (Priority 3 in https://github.com/dakom/awsm-renderer/pull/99).
//!
//! These bind groups all reference the **data_buffer half** of
//! `MaterialEdgeBuffers` (the storage-writable side). The args_buffer
//! half is NOT bound here — it's used only as the `Indirect` source
//! when the render pass calls `dispatch_workgroups_indirect_with_u32`.
//! Splitting them this way is what lets the edge-resolve path
//! pass WebGPU validation; binding the args buffer as Storage *while*
//! using it as Indirect in the same compute pass is rejected.
//!
//! Two bind-group shapes, one per pipeline kind:
//!
//! 1. **Shade extended-shadows (group 3)** — for the unified `cs_shade`
//!    entry point. Shares the primary opaque pipeline's group(0) / lights /
//!    texture-pool binding shape, then extends group(3) (shadows) with
//!    extra bindings at the end carrying the data buffer (read-write
//!    storage) + the edge-layout uniform + `edge_id_tex`. This folds what
//!    was previously a separate group into shadows so the layout fits in
//!    4 bind groups — required to activate on devices with
//!    `maxBindGroups = 4` (macOS Metal in particular).
//!
//! 2. **FinalBlend (group 0)** — for `final_blend`.
//!    Compact: read-only data buffer + layout + the opaque storage tex.
//!    Also reads the args_buffer's `edge_count` counter — but that's
//!    bound at a separate slot because of the Indirect/Storage split.
//!
//! Both bind-group layouts are constructed lazily — when the edge
//! pipelines are first compiled (post-cold-boot, per the scheduler-managed
//! lifecycle). Cached on `MaterialEdgeBindGroups` for subsequent use.

use awsm_renderer_core::bind_groups::{
    BindGroupLayoutResource, BufferBindingLayout, BufferBindingType, StorageTextureAccess,
    StorageTextureBindingLayout,
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
    /// Unified-edge (U1): group(3) layout for the merged `cs_shade` entry
    /// point — the extended-shadows layout (shadows + edge_data@10 +
    /// edge_layout@11) with `edge_id_tex`@12 appended.
    pub shade_extended_shadows_layout_key: BindGroupLayoutKey,

    /// Layout for the global final_blend pipeline (single bind
    /// group at group(0)).
    pub final_blend_group0_layout_key: BindGroupLayoutKey,
}

impl MaterialEdgeBindGroupLayouts {
    /// Constructs the three layouts. Sync-cached in
    /// `BindGroupLayouts`; cheap on subsequent calls with the same
    /// renderer config.
    pub fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let shade_extended_shadows_layout_key = build_extended_shadows_layout(ctx, true)?;
        let final_blend_group0_layout_key = build_final_blend_layout(ctx)?;

        Ok(Self {
            shade_extended_shadows_layout_key,
            final_blend_group0_layout_key,
        })
    }
}

/// Builds the extended-shadows bind-group layout used by the
/// per-shader-id edge_resolve pipelines at group(3).
///
/// Bindings 0..=9 are the standard shadow entries (must stay
/// byte-for-byte compatible with the opaque shadow layout — same shadow
/// resources are bound here). Bindings 10..=11 carry the edge resources
/// that were previously living in a separate group(4); folding them in
/// here lets the edge_resolve pipeline layout fit in 4 bind groups so
/// it activates on macOS Metal (`maxBindGroups = 4`).
///
/// Storage-buffer budget. The args_buffer is *not* bound to
/// edge_resolve as Storage — adding a third extra binding would push
/// the compute stage from 10 to 11 storage buffers, exceeding the
/// WebGPU baseline `maxStorageBuffersPerShaderStage` of 10 (macOS
/// Metal in particular). Instead, classify mirrors the entry counts
/// and the `edge_count` value into a small header at the start of
/// `edge_data`, which edge_resolve reads through the existing
/// binding.
///
/// Bindings:
/// - 10: `edge_data` — storage RW (writes accumulator slots; reads
///   entry-count mirrors from its header).
/// - 11: `edge_layout` — uniform (offsets into `edge_data`).
fn build_extended_shadows_layout(
    ctx: &mut RenderPassInitContext<'_>,
    with_edge_id_tex: bool,
) -> Result<BindGroupLayoutKey> {
    let mut entries = shadow_bind_group_layout_entries(true);

    // 10: edge_data — storage RW (writes accumulator slots).
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
    if with_edge_id_tex {
        // 12: edge_id_tex — read-only R32Uint storage texture (U1 `cs_shade`).
        // Only the unified `cs_shade` entry point references it; the cs_edge
        // layout (with_edge_id_tex=false) omits it so the toggle-OFF ABI is
        // unchanged.
        entries.push(BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::StorageTexture(
                StorageTextureBindingLayout::new(
                    awsm_renderer_core::texture::TextureFormat::R32uint,
                )
                .with_view_dimension(TextureViewDimension::N2d)
                .with_access(StorageTextureAccess::ReadOnly),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        });
    }

    Ok(ctx
        .bind_group_layouts
        .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?)
}

fn build_final_blend_layout(ctx: &mut RenderPassInitContext<'_>) -> Result<BindGroupLayoutKey> {
    // 0: edge_data    — storage RO (reads accumulator + edge_to_xy +
    //                   edge_count from its header).
    // 1: edge_layout  — uniform.
    // 2: opaque_tex   — storage texture (write).
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
