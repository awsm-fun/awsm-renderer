use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    BufferBindingLayout, BufferBindingType, SamplerBindingLayout, SamplerBindingType,
    StorageTextureAccess, StorageTextureBindingLayout, TextureBindingLayout,
};
use awsm_renderer_core::buffers::BufferBinding;
use awsm_renderer_core::texture::{TextureSampleType, TextureViewDimension};
use indexmap::IndexSet;

use crate::bind_group_layout::{BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::render_passes::shared::material::bind_group::{TexturePoolDeps, TexturePoolVisibility};
use crate::textures::SamplerKey;
use crate::{bind_group_layout::BindGroupLayoutKey, render_passes::RenderPassInitContext};

/// Bind group index for material-opaque core textures.
pub const MATERIAL_OPAQUE_CORE_TEXTURES_START_GROUP: u32 = 1;
/// Binding index for material-opaque core textures.
pub const MATERIAL_OPAQUE_CORE_TEXTURES_START_BINDING: u32 = 0;

/// Bind group layout keys and cached bind groups for opaque materials.
pub struct MaterialOpaqueBindGroups {
    pub multisampled_main_bind_group_layout_key: BindGroupLayoutKey,
    pub singlesampled_main_bind_group_layout_key: BindGroupLayoutKey,
    pub lights_bind_group_layout_key: BindGroupLayoutKey,
    pub texture_pool_textures_bind_group_layout_key: BindGroupLayoutKey,
    pub texture_pool_arrays_len: u32,
    pub texture_pool_sampler_keys: IndexSet<SamplerKey>,
    // this is set via `recreate` mechanism
    _main_bind_group: Option<web_sys::GpuBindGroup>,
    _lights_bind_group: Option<web_sys::GpuBindGroup>,
    _texture_bind_group: Option<web_sys::GpuBindGroup>,
}

impl MaterialOpaqueBindGroups {
    /// Creates bind group layouts for the opaque material pass.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let multisampled_main_bind_group_layout_key =
            create_main_bind_group_layout_key(ctx, true).await?;
        let singlesampled_main_bind_group_layout_key =
            create_main_bind_group_layout_key(ctx, false).await?;

        // lights
        let light_entries = vec![
            // info
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
            // punctual lights
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::ReadOnlyStorage),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
        ];

        let lights_bind_group_layout_key = ctx.bind_group_layouts.get_key(
            ctx.gpu,
            BindGroupLayoutCacheKey {
                entries: light_entries,
            },
        )?;

        // Texture Pool
        let TexturePoolDeps {
            bind_group_layout_key: texture_pool_textures_bind_group_layout_key,
            arrays_len: texture_pool_arrays_len,
            sampler_keys: texture_pool_sampler_keys,
        } = TexturePoolDeps::new(ctx, TexturePoolVisibility::Compute)?;

        Ok(Self {
            singlesampled_main_bind_group_layout_key,
            multisampled_main_bind_group_layout_key,
            lights_bind_group_layout_key,
            texture_pool_textures_bind_group_layout_key,
            texture_pool_arrays_len,
            texture_pool_sampler_keys,
            _main_bind_group: None,
            _lights_bind_group: None,
            _texture_bind_group: None,
        })
    }

    /// Rebuilds texture-pool-related layouts while preserving other state.
    pub fn clone_because_texture_pool_changed(
        &self,
        ctx: &mut RenderPassInitContext<'_>,
    ) -> Result<Self> {
        let TexturePoolDeps {
            bind_group_layout_key: texture_pool_textures_bind_group_layout_key,
            arrays_len: texture_pool_arrays_len,
            sampler_keys: texture_pool_sampler_keys,
        } = TexturePoolDeps::new(ctx, TexturePoolVisibility::Compute)?;

        let mut _self = Self {
            multisampled_main_bind_group_layout_key: self.multisampled_main_bind_group_layout_key,
            singlesampled_main_bind_group_layout_key: self.singlesampled_main_bind_group_layout_key,
            lights_bind_group_layout_key: self.lights_bind_group_layout_key,
            texture_pool_textures_bind_group_layout_key,
            texture_pool_arrays_len,
            texture_pool_sampler_keys,
            _main_bind_group: self._main_bind_group.clone(),
            _lights_bind_group: self._lights_bind_group.clone(),
            _texture_bind_group: None,
        };

        Ok(_self)
    }

    /// Returns the live bind groups used for rendering.
    pub fn get_bind_groups(
        &self,
    ) -> std::result::Result<
        (
            &web_sys::GpuBindGroup,
            &web_sys::GpuBindGroup,
            &web_sys::GpuBindGroup,
        ),
        AwsmBindGroupError,
    > {
        match (
            &self._main_bind_group,
            &self._lights_bind_group,
            &self._texture_bind_group,
        ) {
            (Some(main_bind_group), Some(lights_bind_group), Some(texture_bind_group)) => {
                Ok((main_bind_group, lights_bind_group, texture_bind_group))
            }
            (None, _, _) => Err(AwsmBindGroupError::NotFound(
                "Material Opaque - Main".to_string(),
            )),
            (_, None, _) => Err(AwsmBindGroupError::NotFound(
                "Material Opaque - Lights".to_string(),
            )),
            (_, _, None) => Err(AwsmBindGroupError::NotFound(
                "Material Opaque - Texture Pool".to_string(),
            )),
        }
    }

    /// Recreates the main bind group for the opaque material pass.
    pub fn recreate_main(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let mut entries = Vec::new();

        // Visibility data texture
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(
                &ctx.render_texture_views.visibility_data,
            )),
        ));
        // Barycentric texture
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.barycentric)),
        ));
        // Depth texture
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.depth)),
        ));
        // geometry normal texture
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.normal_tangent)),
        ));
        // placeholder derivatives texture
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(
                &ctx.render_texture_views.barycentric_derivatives,
            )),
        ));
        // visibility data
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(
                ctx.meshes.visibility_geometry_data_gpu_buffer(),
            )),
        ));
        // Mesh Meta (for this pass, different than geometry pass)
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(ctx.meshes.meta.material_gpu_buffer())),
        ));
        // Material data buffer
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&ctx.materials.gpu_buffer)),
        ));
        // Attribute index buffer
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(
                ctx.meshes.custom_attribute_index_gpu_buffer(),
            )),
        ));
        // Attribute data buffer
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(
                ctx.meshes.custom_attribute_data_gpu_buffer(),
            )),
        ));
        // transforms
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&ctx.transforms.gpu_buffer)),
        ));
        // normal matrices
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&ctx.transforms.normals_gpu_buffer)),
        ));
        // texture transforms
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(
                &ctx.textures.texture_transforms_gpu_buffer,
            )),
        ));
        // camera
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
        ));

        //skybox
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(&ctx.environment.skybox.texture_view)),
        ));
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Sampler(&ctx.environment.skybox.sampler),
        ));

        // IBL filtered env
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(
                &ctx.lights.ibl.prefiltered_env.texture_view,
            )),
        ));
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Sampler(&ctx.lights.ibl.prefiltered_env.sampler),
        ));

        // IBL irradiance
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(&ctx.lights.ibl.irradiance.texture_view)),
        ));
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Sampler(&ctx.lights.ibl.irradiance.sampler),
        ));

        // BRDF lut
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(&ctx.lights.brdf_lut.view)),
        ));
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Sampler(&ctx.lights.brdf_lut.sampler),
        ));
        // Opaque color render texture (storage texture for compute write)
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.opaque)),
        ));
        // Per-instance attribute storage buffer (color/alpha/size tint applied
        // after shading; sentinel `instance_id == U32_MAX` means identity).
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(ctx.instances.gpu_attribute_buffer())),
        ));

        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts
                .get(if ctx.anti_aliasing.msaa_sample_count.is_some() {
                    self.multisampled_main_bind_group_layout_key
                } else {
                    self.singlesampled_main_bind_group_layout_key
                })?,
            Some("Material Opaque - Main"),
            entries,
        );

        self._main_bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));

        Ok(())
    }

    /// Recreates the light bind group for the opaque material pass.
    pub fn recreate_lights(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let mut entries = Vec::new();

        // Lights info
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&ctx.lights.gpu_info_buffer)),
        ));

        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&ctx.lights.gpu_punctual_buffer)),
        ));

        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts
                .get(self.lights_bind_group_layout_key)?,
            Some("Material Opaque - Lights"),
            entries,
        );

        self._lights_bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));

        Ok(())
    }

    /// Recreates the texture pool bind group for the opaque material pass.
    pub fn recreate_texture_pool(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let mut entries = Vec::new();

        for view in ctx.textures.pool.texture_views() {
            entries.push(BindGroupEntry::new(
                entries.len() as u32,
                BindGroupResource::TextureView(Cow::Borrowed(view)),
            ));
        }

        for sampler_key in self.texture_pool_sampler_keys.iter() {
            let sampler = ctx.textures.get_sampler(*sampler_key)?;

            entries.push(BindGroupEntry::new(
                entries.len() as u32,
                BindGroupResource::Sampler(sampler),
            ));
        }

        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts
                .get(self.texture_pool_textures_bind_group_layout_key)?,
            Some("Material Opaque - Texture Pool"),
            entries,
        );

        self._texture_bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));

        Ok(())
    }
}

async fn create_main_bind_group_layout_key(
    ctx: &mut RenderPassInitContext<'_>,
    multisampled_geometry: bool,
) -> Result<BindGroupLayoutKey> {
    let entries = vec![
        // Visibility data texture
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
        // Barycentric texture — RGBA16uint: RG channels hold barycentric.xy
        // as u16 fixed-point (* 65535), BA channels hold the per-fragment
        // instance_id as a packed u32 (high u16 in B, low u16 in A; joined
        // via the same `join32` helper used for tri_id/material_offset).
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
        // Geometry normal texture (world-space normals from geometry pass)
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Texture(
                TextureBindingLayout::new()
                    .with_view_dimension(TextureViewDimension::N2d)
                    .with_sample_type(TextureSampleType::UnfilterableFloat)
                    .with_multisampled(multisampled_geometry),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // Barycentric derivatives texture
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Texture(
                TextureBindingLayout::new()
                    .with_view_dimension(TextureViewDimension::N2d)
                    .with_sample_type(TextureSampleType::UnfilterableFloat)
                    .with_multisampled(multisampled_geometry),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // Visibility data buffer (positions, triangle-id, barycentric) for mipmap computation
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // Mesh Meta (for this pass, different than geometry pass)
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // Material data buffer
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // Attribute index buffer
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // Attribute data buffer
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // Transform buffer
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // Normal matrices buffer
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // Texture transforms buffer
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
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
        // Skybox texture
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Texture(
                TextureBindingLayout::new().with_view_dimension(TextureViewDimension::Cube),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // Skybox sampler
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Sampler(
                SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Filtering),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // IBL prefiltered env texture
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Texture(
                TextureBindingLayout::new().with_view_dimension(TextureViewDimension::Cube),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // IBL prefiltered env sampler
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Sampler(
                SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Filtering),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // IBL irradiance env texture
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Texture(
                TextureBindingLayout::new().with_view_dimension(TextureViewDimension::Cube),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // IBL irradiance env sampler
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Sampler(
                SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Filtering),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // Brdf lut texture
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Texture(
                TextureBindingLayout::new().with_view_dimension(TextureViewDimension::N2d),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // Brdf lut sampler
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Sampler(
                SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Filtering),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // Opaque color render texture (storage texture for compute write)
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
        // Per-instance attribute storage buffer (read by shading pass for tint).
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
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
