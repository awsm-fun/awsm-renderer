//! Bind group layout + recreation for the material decal pass.

use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    BufferBindingLayout, BufferBindingType, SamplerBindingLayout, SamplerBindingType,
    StorageTextureAccess, StorageTextureBindingLayout, TextureBindingLayout,
};
use awsm_renderer_core::buffers::BufferBinding;
use awsm_renderer_core::texture::{TextureSampleType, TextureViewDimension};

use crate::bind_group_layout::{BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::render_passes::shared::material::bind_group::{TexturePoolDeps, TexturePoolVisibility};
use crate::{bind_group_layout::BindGroupLayoutKey, render_passes::RenderPassInitContext};

/// Bind group layout + cached bind groups for the decal pass.
pub struct MaterialDecalBindGroups {
    pub main_layout_key_multisampled: BindGroupLayoutKey,
    pub main_layout_key_singlesampled: BindGroupLayoutKey,
    pub texture_pool_layout_key: BindGroupLayoutKey,
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    main_bind_group: Option<web_sys::GpuBindGroup>,
    texture_pool_bind_group: Option<web_sys::GpuBindGroup>,
}

impl MaterialDecalBindGroups {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let main_layout_key_multisampled = create_main_layout(ctx, true).await?;
        let main_layout_key_singlesampled = create_main_layout(ctx, false).await?;

        let TexturePoolDeps {
            bind_group_layout_key: texture_pool_layout_key,
            arrays_len: texture_pool_arrays_len,
            sampler_keys,
        } = TexturePoolDeps::new(ctx, TexturePoolVisibility::Compute)?;

        Ok(Self {
            main_layout_key_multisampled,
            main_layout_key_singlesampled,
            texture_pool_layout_key,
            texture_pool_arrays_len,
            texture_pool_samplers_len: sampler_keys.len() as u32,
            main_bind_group: None,
            texture_pool_bind_group: None,
        })
    }

    pub fn get_main(&self) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.main_bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Material Decal - Main".to_string()))
    }

    pub fn get_texture_pool(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.texture_pool_bind_group.as_ref().ok_or_else(|| {
            AwsmBindGroupError::NotFound("Material Decal - Texture Pool".to_string())
        })
    }

    /// Rebuilds the decal main bind group against current resources.
    pub fn recreate_main(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        // §16.4.D: the decal compute now always writes to `decal_color`
        // (single-sample storage). On MSAA-off the composite step blits
        // through to `transparent`; on MSAA the composite alpha-blits to
        // the multisampled target. Either way the bind-group shape is
        // identical — no MSAA-fork here.
        let msaa = ctx.anti_aliasing.msaa_sample_count.is_some();
        let layout_key = if msaa {
            self.main_layout_key_multisampled
        } else {
            self.main_layout_key_singlesampled
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
                BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.depth)),
            ),
            BindGroupEntry::new(
                2,
                BindGroupResource::TextureView(Cow::Borrowed(
                    &ctx.render_texture_views.opaque_full,
                )),
            ),
            BindGroupEntry::new(
                3,
                BindGroupResource::TextureView(Cow::Borrowed(
                    &ctx.render_texture_views.decal_color,
                )),
            ),
            BindGroupEntry::new(
                4,
                BindGroupResource::Buffer(BufferBinding::new(
                    ctx.meshes.meta.material_gpu_buffer(),
                )),
            ),
            BindGroupEntry::new(
                5,
                BindGroupResource::Buffer(BufferBinding::new(ctx.decals.gpu_buffer())),
            ),
            BindGroupEntry::new(
                6,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
            ),
        ];

        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(layout_key)?,
            Some("Material Decal - Main"),
            entries,
        );
        self.main_bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }

    pub fn recreate_texture_pool(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let mut entries = Vec::new();
        for view in ctx.textures.pool.texture_views() {
            entries.push(BindGroupEntry::new(
                entries.len() as u32,
                BindGroupResource::TextureView(Cow::Borrowed(view)),
            ));
        }
        for sampler_key in ctx.textures.pool_sampler_set.iter() {
            let sampler = ctx.textures.get_sampler(*sampler_key)?;
            entries.push(BindGroupEntry::new(
                entries.len() as u32,
                BindGroupResource::Sampler(sampler),
            ));
        }
        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(self.texture_pool_layout_key)?,
            Some("Material Decal - Texture Pool"),
            entries,
        );
        self.texture_pool_bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }
}

async fn create_main_layout(
    ctx: &mut RenderPassInitContext<'_>,
    multisampled_geometry: bool,
) -> Result<BindGroupLayoutKey> {
    let _ = SamplerBindingType::Filtering; // keep the import — sampler comes from texture pool
    let _ = SamplerBindingLayout::new();

    let entries = vec![
        // visibility_data
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
        // depth
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
        // opaque_tex_in (sampled)
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
        // transparent_tex_out (storage write)
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
        // material_mesh_metas
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // decals_buffer
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // camera (uniform)
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
