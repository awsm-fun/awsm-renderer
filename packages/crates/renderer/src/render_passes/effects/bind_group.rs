//! Effects pass bind group setup.

use std::borrow::Cow;

use crate::{
    bind_group_layout::{
        BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey,
    },
    bind_groups::{AwsmBindGroupError, BindGroupRecreateContext},
    error::Result,
    render_passes::RenderPassInitContext,
    render_textures::RenderTextureFormats,
};
use awsm_renderer_core::{
    bind_groups::{
        BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
        BufferBindingLayout, BufferBindingType, StorageTextureAccess, StorageTextureBindingLayout,
        TextureBindingLayout,
    },
    buffers::BufferBinding,
    texture::{TextureSampleType, TextureViewDimension},
};

/// Bind group layouts and cached bind groups for the effects pass.
#[derive(Default)]
pub struct EffectsBindGroups {
    pub multisampled_bind_group_layout_key: BindGroupLayoutKey,
    pub singlesampled_bind_group_layout_key: BindGroupLayoutKey,
    // this is set via `recreate` mechanism
    bind_group: Option<web_sys::GpuBindGroup>,
}

impl EffectsBindGroups {
    /// Creates bind group layouts for the effects pass.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let singlesampled_bind_group_layout_cache_key =
            bind_group_layout_cache_key(ctx.render_texture_formats, false);

        let multisampled_bind_group_layout_cache_key =
            bind_group_layout_cache_key(ctx.render_texture_formats, true);

        let singlesampled_bind_group_layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, singlesampled_bind_group_layout_cache_key)?;

        let multisampled_bind_group_layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, multisampled_bind_group_layout_cache_key)?;

        Ok(Self {
            multisampled_bind_group_layout_key,
            singlesampled_bind_group_layout_key,
            bind_group: None,
        })
    }

    /// Returns the effects bind group.
    pub fn get_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Effects".to_string()))
    }

    /// Recreates bind groups for the current render textures.
    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let mut entries = Vec::new();

        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.composite)),
        ));
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
        ));
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.depth)),
        ));
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.bloom)),
        ));
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.effects)),
        ));
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&ctx.frame_globals.gpu_buffer)),
        ));

        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts
                .get(if ctx.anti_aliasing.has_msaa_checked()? {
                    self.multisampled_bind_group_layout_key
                } else {
                    self.singlesampled_bind_group_layout_key
                })?,
            Some("Effects"),
            entries,
        );

        self.bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));

        Ok(())
    }
}

fn bind_group_layout_cache_key(
    render_texture_formats: &RenderTextureFormats,
    multisampled_geometry: bool,
) -> BindGroupLayoutCacheKey {
    BindGroupLayoutCacheKey {
        entries: vec![
            // Composite texture
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Texture(
                    TextureBindingLayout::new()
                        .with_view_dimension(TextureViewDimension::N2d)
                        .with_sample_type(TextureSampleType::UnfilterableFloat),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
            // Camera uniform gives us inverse matrices + frustum rays for depth reprojection.
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
            // Depth texture
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Texture(
                    TextureBindingLayout::new()
                        .with_view_dimension(TextureViewDimension::N2d)
                        .with_sample_type(TextureSampleType::Depth)
                        .with_multisampled(multisampled_geometry),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
            // Bloom or Effects texture (readable - depends on ping-pong which one)
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Texture(
                    TextureBindingLayout::new()
                        .with_view_dimension(TextureViewDimension::N2d)
                        .with_sample_type(TextureSampleType::UnfilterableFloat),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
            // Bloom or Effects texture (writable - depends on ping-pong which one)
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::StorageTexture(
                    StorageTextureBindingLayout::new(render_texture_formats.color)
                        .with_view_dimension(TextureViewDimension::N2d)
                        .with_access(StorageTextureAccess::WriteOnly),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
            // FrameGlobals uniform.
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
        ],
    }
}
