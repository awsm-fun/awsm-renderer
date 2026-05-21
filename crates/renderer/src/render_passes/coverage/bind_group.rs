//! Coverage compute pass bind group.
//!
//! Single bind group with:
//!   0  visibility_data (sampled texture; multisampled when MSAA)
//!   1  mesh_pixel_counts (storage RW; atomic u32 per slot)

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

pub struct CoverageBindGroups {
    pub layout_key: BindGroupLayoutKey,
    /// `true` when this variant binds the multisampled
    /// visibility-data texture (MSAA path). Recorded at construction
    /// from the current anti-aliasing config; rebuilt by the
    /// `AntiAliasingChange` recreate signal.
    pub multisampled: bool,
    bind_group: Option<web_sys::GpuBindGroup>,
}

impl CoverageBindGroups {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>, multisampled: bool) -> Result<Self> {
        let entries = vec![
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Texture(
                    TextureBindingLayout::new()
                        .with_view_dimension(TextureViewDimension::N2d)
                        .with_sample_type(TextureSampleType::Uint)
                        .with_multisampled(multisampled),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new().with_binding_type(BufferBindingType::Storage),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
        ];
        let layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?;
        Ok(Self {
            layout_key,
            multisampled,
            bind_group: None,
        })
    }

    pub fn get_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Coverage".to_string()))
    }

    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let coverage_buffers = ctx
            .coverage_buffers
            .expect("coverage buffers missing despite coverage pass active");
        let entries = vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::TextureView(Cow::Borrowed(
                    &ctx.render_texture_views.visibility_data,
                )),
            ),
            BindGroupEntry::new(
                1,
                BindGroupResource::Buffer(BufferBinding::new(&coverage_buffers.counts_buffer)),
            ),
        ];
        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(self.layout_key)?,
            Some("Coverage"),
            entries,
        );
        self.bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }
}
