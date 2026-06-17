//! Bind group layout + recreation for the material prep compute pass
//! (Plan B, docs/plans/deferred-shared-prep-pass.md).
//!
//! Single bind group; layout must stay in lockstep with the WGSL emitted by
//! [`super::shader::template::ShaderTemplateMaterialPrepBindGroups`]:
//!
//!   0 visibility_data_tex — uint texture (triangle id + meta offset). MSAA
//!     variant is multisampled.
//!   1 barycentric_tex     — uint texture (bary weights + instance id). MSAA
//!     variant is multisampled.
//!   2 visibility_data     — storage RO, the merged geometry pool.
//!   3 material_mesh_metas — storage RO, per-mesh metadata table.
//!   4 uv_out              — storage texture (rg32float, write) — materialized UV0.
//!   5 vcolor_out          — storage texture (rgba32float, write) — materialized vcolor0.
//!
//! Mirrors [`crate::render_passes::material_classify::bind_group`] for the dual
//! MSAA layout (single- vs multi-sampled geometry textures).

use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    BufferBindingLayout, BufferBindingType, StorageTextureAccess, StorageTextureBindingLayout,
    TextureBindingLayout,
};
use awsm_renderer_core::buffers::BufferBinding;
use awsm_renderer_core::texture::{TextureFormat, TextureSampleType, TextureViewDimension};

use crate::bind_group_layout::{
    BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey,
};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::render_passes::RenderPassInitContext;

/// Bind group layout(s) + cached bind group for the prep pass.
pub struct MaterialPrepBindGroups {
    pub multisampled_bind_group_layout_key: BindGroupLayoutKey,
    pub singlesampled_bind_group_layout_key: BindGroupLayoutKey,
    bind_group: Option<web_sys::GpuBindGroup>,
}

impl MaterialPrepBindGroups {
    /// Creates the bind group layouts for the prep pass (both MSAA-geometry
    /// variants). The bind group itself is built lazily via [`Self::recreate`]
    /// when the renderer's `TextureViewRecreate` event fires (the prep output
    /// storage textures are recreated alongside the other render-texture views
    /// on resize).
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let multisampled_bind_group_layout_key = create_bind_group_layout_key(ctx, true)?;
        let singlesampled_bind_group_layout_key = create_bind_group_layout_key(ctx, false)?;

        Ok(Self {
            multisampled_bind_group_layout_key,
            singlesampled_bind_group_layout_key,
            bind_group: None,
        })
    }

    /// Returns the live prep bind group. Errors if [`Self::recreate`] hasn't
    /// been called yet this session (e.g. before the first frame's
    /// `BindGroups::recreate`).
    pub fn get_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Material Prep".to_string()))
    }

    /// (Re)builds the prep bind group against the current visibility +
    /// barycentric views, the merged geometry pool, the mesh-meta table, and
    /// the prep output storage textures. Picks the layout by the live MSAA
    /// state. Called from [`crate::bind_groups::BindGroups`] in response to a
    /// `TextureViewRecreate` event.
    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let msaa = ctx.anti_aliasing.msaa_sample_count.is_some();
        let layout_key = if msaa {
            self.multisampled_bind_group_layout_key
        } else {
            self.singlesampled_bind_group_layout_key
        };

        // Output storage textures only exist when prep is enabled; if they're
        // absent the prep pass should never have been constructed. Treat a
        // missing view as a hard error (mirrors the opaque pass's Option-view
        // NotFound discipline).
        let uv_out = ctx
            .render_texture_views
            .prep_uv
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Material Prep - uv_out".to_string()))?;
        let vcolor_out = ctx
            .render_texture_views
            .prep_vcolor
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Material Prep - vcolor_out".to_string()))?;

        let entries = vec![
            // 0 visibility_data_tex.
            BindGroupEntry::new(
                0,
                BindGroupResource::TextureView(Cow::Borrowed(
                    &ctx.render_texture_views.visibility_data,
                )),
            ),
            // 1 barycentric_tex.
            BindGroupEntry::new(
                1,
                BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.barycentric)),
            ),
            // 2 visibility_data — merged geometry pool (storage RO).
            BindGroupEntry::new(
                2,
                BindGroupResource::Buffer(BufferBinding::new(
                    ctx.meshes.visibility_geometry_data_gpu_buffer(),
                )),
            ),
            // 3 material_mesh_metas — storage RO.
            BindGroupEntry::new(
                3,
                BindGroupResource::Buffer(BufferBinding::new(ctx.meshes.meta.material_gpu_buffer())),
            ),
            // 4 uv_out — storage texture (write).
            BindGroupEntry::new(4, BindGroupResource::TextureView(Cow::Borrowed(uv_out))),
            // 5 vcolor_out — storage texture (write).
            BindGroupEntry::new(5, BindGroupResource::TextureView(Cow::Borrowed(vcolor_out))),
        ];

        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(layout_key)?,
            Some("Material Prep"),
            entries,
        );
        self.bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }
}

fn create_bind_group_layout_key(
    ctx: &mut RenderPassInitContext<'_>,
    multisampled_geometry: bool,
) -> Result<BindGroupLayoutKey> {
    let entries = vec![
        // 0 visibility_data_tex — uint texture; MSAA variant is multisampled.
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Texture(
                TextureBindingLayout::new()
                    .with_view_dimension(TextureViewDimension::N2d)
                    .with_sample_type(TextureSampleType::Uint)
                    .with_multisampled(multisampled_geometry),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // 1 barycentric_tex — uint texture; MSAA variant is multisampled.
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Texture(
                TextureBindingLayout::new()
                    .with_view_dimension(TextureViewDimension::N2d)
                    .with_sample_type(TextureSampleType::Uint)
                    .with_multisampled(multisampled_geometry),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // 2 visibility_data — merged geometry pool (storage RO).
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // 3 material_mesh_metas — storage RO.
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // 4 uv_out — storage texture (rg32float, write).
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::StorageTexture(
                StorageTextureBindingLayout::new(TextureFormat::Rg32float)
                    .with_view_dimension(TextureViewDimension::N2d)
                    .with_access(StorageTextureAccess::WriteOnly),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // 5 vcolor_out — storage texture (rgba32float, write).
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::StorageTexture(
                StorageTextureBindingLayout::new(TextureFormat::Rgba32float)
                    .with_view_dimension(TextureViewDimension::N2d)
                    .with_access(StorageTextureAccess::WriteOnly),
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
