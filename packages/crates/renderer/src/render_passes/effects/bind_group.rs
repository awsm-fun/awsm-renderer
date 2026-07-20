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
    /// 1×1 zero texture bound at the SMAA-weights slot while SMAA is off
    /// (keeps the layout shape stable across the toggle at 4 bytes of VRAM).
    dummy_weights_view: Option<web_sys::GpuTextureView>,
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

        let dummy_tex = ctx.gpu.create_texture(
            &awsm_renderer_core::texture::TextureDescriptor::new(
                awsm_renderer_core::texture::TextureFormat::Rgba8unorm,
                awsm_renderer_core::texture::Extent3d::new(1, Some(1), None),
                awsm_renderer_core::texture::TextureUsage::new().with_texture_binding(),
            )
            .with_label("Effects SMAA Dummy Weights")
            .into(),
        )?;
        let dummy_weights_view = dummy_tex.create_view().map_err(|e| {
            awsm_renderer_core::error::AwsmCoreError::create_texture_view(format!("{e:?}").into())
        })?;

        Ok(Self {
            multisampled_bind_group_layout_key,
            singlesampled_bind_group_layout_key,
            bind_group: None,
            dummy_weights_view: Some(dummy_weights_view),
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
    /// `smaa_weights_view` is the SMAA pre-pass's weights texture when SMAA is
    /// enabled; `None` binds the internal 1×1 zero dummy.
    pub fn recreate(
        &mut self,
        ctx: &BindGroupRecreateContext<'_>,
        smaa_weights_view: Option<&web_sys::GpuTextureView>,
    ) -> Result<()> {
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
        let weights_view = smaa_weights_view
            .or(self.dummy_weights_view.as_ref())
            .expect("dummy weights view exists after new()");
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(weights_view)),
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
            // SMAA blend-weights texture (1x1 zero dummy when SMAA is off).
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
        ],
    }
}
