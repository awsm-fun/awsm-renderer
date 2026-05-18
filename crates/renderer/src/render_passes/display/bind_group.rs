//! Display pass bind group setup.

use std::borrow::Cow;

use crate::{
    bind_group_layout::{
        BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey,
    },
    bind_groups::{AwsmBindGroupError, BindGroupRecreateContext},
    error::Result,
    render_passes::RenderPassInitContext,
};
use awsm_renderer_core::{
    bind_groups::{
        BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
        BufferBindingLayout, BufferBindingType, TextureBindingLayout,
    },
    buffers::{BufferBinding, BufferDescriptor, BufferUsage},
    texture::{TextureSampleType, TextureViewDimension},
};

/// 16-byte uniform block uploaded to the display pass.
///
/// Currently just exposure_scale (a single f32) plus padding — uniform
/// buffer bindings need to be 16-byte aligned, so we round up.
pub const DISPLAY_UNIFORM_SIZE: usize = 16;

/// Bind group layout and cached bind group for the display pass.
pub struct DisplayBindGroups {
    pub bind_group_layout_key: BindGroupLayoutKey,
    pub uniform_buffer: web_sys::GpuBuffer,
    // this is set via `recreate` mechanism
    _bind_group: Option<web_sys::GpuBindGroup>,
}

impl DisplayBindGroups {
    /// Creates the display bind group layout.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_group_layout_cache_key = BindGroupLayoutCacheKey {
            entries: vec![
                BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::Texture(
                        TextureBindingLayout::new()
                            .with_view_dimension(TextureViewDimension::N2d)
                            .with_sample_type(TextureSampleType::Float),
                    ),
                    visibility_vertex: true,
                    visibility_fragment: true,
                    visibility_compute: false,
                },
                BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                    ),
                    visibility_vertex: false,
                    visibility_fragment: true,
                    visibility_compute: false,
                },
            ],
        };

        let bind_group_layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, bind_group_layout_cache_key)?;

        let uniform_buffer = ctx.gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Display Uniform"),
                DISPLAY_UNIFORM_SIZE,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;

        Ok(Self {
            bind_group_layout_key,
            uniform_buffer,
            _bind_group: None,
        })
    }

    /// Returns the active display bind group.
    pub fn get_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self._bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Display".to_string()))
    }

    /// Recreates the bind group for the current render textures.
    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(self.bind_group_layout_key)?,
            Some("Display"),
            vec![
                BindGroupEntry::new(
                    0,
                    BindGroupResource::TextureView(Cow::Borrowed(
                        &ctx.render_texture_views.effects,
                    )),
                ),
                BindGroupEntry::new(
                    1,
                    BindGroupResource::Buffer(BufferBinding::new(&self.uniform_buffer)),
                ),
            ],
        );

        self._bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));

        Ok(())
    }
}
