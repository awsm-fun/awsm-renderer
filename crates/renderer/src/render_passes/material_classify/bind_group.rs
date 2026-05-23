//! Bind group layout + recreation for the material classify pass.
//!
//! Single bind group:
//!   0 visibility_data_tex — uint texture (per-pixel material id).
//!   1 material_mesh_metas — storage[RO] mesh-meta table.
//!   2 materials_data      — storage[RO] material payload (for shader_id).
//!   3 classify_output     — storage[RW] (atomic) per-`shader_id` buckets.

use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    BufferBindingLayout, BufferBindingType, TextureBindingLayout,
};
use awsm_renderer_core::buffers::BufferBinding;
use awsm_renderer_core::texture::{TextureSampleType, TextureViewDimension};

use crate::bind_group_layout::{BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::{bind_group_layout::BindGroupLayoutKey, render_passes::RenderPassInitContext};

/// Bind group layout + cached bind group for the classify pass.
pub struct MaterialClassifyBindGroups {
    pub multisampled_bind_group_layout_key: BindGroupLayoutKey,
    pub singlesampled_bind_group_layout_key: BindGroupLayoutKey,
    bind_group: Option<web_sys::GpuBindGroup>,
}

impl MaterialClassifyBindGroups {
    /// Creates the bind group layouts for the classify pass. The
    /// bind group itself is built lazily via [`Self::recreate`] when
    /// the renderer's `BindGroups::mark_create` event fires (e.g. on
    /// the first frame, on viewport resize, when classify buffers are
    /// recreated).
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let multisampled_bind_group_layout_key = create_bind_group_layout_key(ctx, true).await?;
        let singlesampled_bind_group_layout_key = create_bind_group_layout_key(ctx, false).await?;

        Ok(Self {
            multisampled_bind_group_layout_key,
            singlesampled_bind_group_layout_key,
            bind_group: None,
        })
    }

    /// Returns the live classify bind group. Errors if
    /// [`Self::recreate`] hasn't been called yet this session.
    pub fn get_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Material Classify".to_string()))
    }

    /// (Re)builds the classify bind group against the current
    /// classify buffer + visibility view + mesh-meta + materials
    /// buffers. Called from [`crate::bind_groups::BindGroups`] in
    /// response to a `MaterialClassifyResourcesChange` event.
    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let msaa = ctx.anti_aliasing.msaa_sample_count.is_some();
        let layout_key = if msaa {
            self.multisampled_bind_group_layout_key
        } else {
            self.singlesampled_bind_group_layout_key
        };
        let entries = vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::TextureView(Cow::Borrowed(
                    &ctx.render_texture_views.visibility_data,
                )),
            ),
            BindGroupEntry::new(
                1,
                BindGroupResource::Buffer(BufferBinding::new(
                    ctx.meshes.meta.material_gpu_buffer(),
                )),
            ),
            BindGroupEntry::new(
                2,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.materials.gpu_buffer)),
            ),
            BindGroupEntry::new(
                3,
                BindGroupResource::Buffer(BufferBinding::new(
                    &ctx.material_classify_buffers.buffer,
                )),
            ),
        ];

        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(layout_key)?,
            Some("Material Classify"),
            entries,
        );
        self.bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }
}

async fn create_bind_group_layout_key(
    ctx: &mut RenderPassInitContext<'_>,
    multisampled_geometry: bool,
) -> Result<BindGroupLayoutKey> {
    let entries = vec![
        // visibility_data — uint texture; MSAA variant is multisampled.
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
        // material_mesh_metas — storage RO.
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // materials_data — storage RO.
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // classify_output — storage RW (atomics).
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::Storage),
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
