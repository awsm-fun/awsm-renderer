//! Transparent material bind group setup.

use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    BufferBindingLayout, BufferBindingType, SamplerBindingLayout, SamplerBindingType,
    TextureBindingLayout,
};
use awsm_renderer_core::buffers::BufferBinding;
use awsm_renderer_core::texture::TextureViewDimension;
use indexmap::IndexSet;

use crate::bind_group_layout::{BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::meshes::meta::geometry_meta::GEOMETRY_MESH_META_BYTE_ALIGNMENT;
use crate::meshes::meta::material_meta::MATERIAL_MESH_META_BYTE_ALIGNMENT;
use crate::render_passes::shared::material::bind_group::{
    build_shadow_bind_group_entries, shadow_bind_group_layout_entries, TexturePoolDeps,
    TexturePoolVisibility,
};
use crate::textures::SamplerKey;
use crate::{bind_group_layout::BindGroupLayoutKey, render_passes::RenderPassInitContext};

/// Bind group layout keys and cached bind groups for transparent materials.
///
/// As of 16.B, the transparent pass runs with 3 user bind groups + the
/// shadow bind group at slot 1 — that's 4 total, exactly at the
/// `maxBindGroups = 4` adapter limit. The former standalone `lights`
/// group has been folded into `main` (all of its entries — IBL,
/// BRDF LUT, lights_info, lights — are global per-frame).
pub struct MaterialTransparentBindGroups {
    pub main_bind_group_layout_key: BindGroupLayoutKey,
    pub mesh_material_bind_group_layout_key: BindGroupLayoutKey,
    pub texture_pool_textures_bind_group_layout_key: BindGroupLayoutKey,
    pub shadows_bind_group_layout_key: BindGroupLayoutKey,
    pub texture_pool_arrays_len: u32,
    pub texture_pool_sampler_keys: IndexSet<SamplerKey>,

    _main_bind_group: Option<web_sys::GpuBindGroup>,
    _mesh_material_bind_group: Option<web_sys::GpuBindGroup>,
    _texture_bind_group: Option<web_sys::GpuBindGroup>,
    _shadows_bind_group: Option<web_sys::GpuBindGroup>,
}

impl MaterialTransparentBindGroups {
    /// Creates bind group layouts for transparent materials.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let TexturePoolDeps {
            bind_group_layout_key: texture_pool_textures_bind_group_layout_key,
            arrays_len: texture_pool_arrays_len,
            sampler_keys: texture_pool_sampler_keys,
        } = TexturePoolDeps::new(ctx, TexturePoolVisibility::Render)?;

        let entries = vec![
            // Camera
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            // Transform
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::ReadOnlyStorage),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            // Materials
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::ReadOnlyStorage),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            // Morph weights
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::ReadOnlyStorage),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            // Morph values
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::ReadOnlyStorage),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            // Skin matrices
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::ReadOnlyStorage),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            // Skin weights
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::ReadOnlyStorage),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            // Texture transforms
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::ReadOnlyStorage),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            // Opaque texture
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Texture(
                    TextureBindingLayout::new().with_view_dimension(TextureViewDimension::N2d),
                ),
                visibility_vertex: false,
                visibility_fragment: true,
                visibility_compute: false,
            },
            // Per-instance attribute storage buffer — read by both vertex
            // (to plumb `instance_id` forward) and fragment (to apply the
            // per-instance tint). Mirrors the opaque pass's binding 23.
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::ReadOnlyStorage),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            // ── Lights block (was a separate bind group prior to 16.B,
            // folded into `main` so slot 1 frees up for shadows; every
            // binding here is global per frame, identical to the
            // opaque pass's "lights" group. Bindings 10..=17.)
            // IBL prefiltered env texture (cube)
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Texture(
                    TextureBindingLayout::new().with_view_dimension(TextureViewDimension::Cube),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Sampler(
                    SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Filtering),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Texture(
                    TextureBindingLayout::new().with_view_dimension(TextureViewDimension::Cube),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Sampler(
                    SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Filtering),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Texture(
                    TextureBindingLayout::new().with_view_dimension(TextureViewDimension::N2d),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Sampler(
                    SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Filtering),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            // lights_info uniform
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            // lights — uniform (matches opaque pass; same fixed-size
            // allocation feeding both passes).
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            // FrameGlobals uniform (renderer-wide per-frame state).
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            },
            // Extras pool — variable-length per-material data backing
            // custom-material `BufferSlot` declarations on transparents.
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::ReadOnlyStorage),
                ),
                visibility_vertex: false,
                visibility_fragment: true,
                visibility_compute: false,
            },
            // GPU light-culling `cull_params` uniform — read by the
            // shading-time froxel index calculation in the shared
            // `apply_lighting_per_froxel*` helpers.
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                ),
                visibility_vertex: false,
                visibility_fragment: true,
                visibility_compute: false,
            },
            // GPU light-culling `froxel_storage` (combined per-froxel
            // count + indices). Bound read-only here; the cull pass
            // binds the same buffer RW with an atomic-array WGSL view.
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::ReadOnlyStorage),
                ),
                visibility_vertex: false,
                visibility_fragment: true,
                visibility_compute: false,
            },
        ];

        let main_bind_group_layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?;

        // Mesh meta
        let mesh_material_bind_group_layout_key = ctx.bind_group_layouts.get_key(
            ctx.gpu,
            BindGroupLayoutCacheKey {
                entries: vec![
                    // GeometryMeshMeta
                    BindGroupLayoutCacheKeyEntry {
                        resource: BindGroupLayoutResource::Buffer(
                            BufferBindingLayout::new()
                                .with_binding_type(BufferBindingType::Uniform)
                                .with_dynamic_offset(true),
                        ),
                        visibility_vertex: true,
                        visibility_fragment: true,
                        visibility_compute: false,
                    },
                    // MaterialMeshMeta
                    BindGroupLayoutCacheKeyEntry {
                        resource: BindGroupLayoutResource::Buffer(
                            BufferBindingLayout::new()
                                .with_binding_type(BufferBindingType::Uniform)
                                .with_dynamic_offset(true),
                        ),
                        visibility_vertex: true,
                        visibility_fragment: true,
                        visibility_compute: false,
                    },
                ],
            },
        )?;

        // Lights inlined into `main` above; no standalone group needed.

        let shadows_bind_group_layout_key = ctx.bind_group_layouts.get_key(
            ctx.gpu,
            BindGroupLayoutCacheKey {
                entries: shadow_bind_group_layout_entries(false),
            },
        )?;

        Ok(Self {
            main_bind_group_layout_key,
            mesh_material_bind_group_layout_key,

            texture_pool_textures_bind_group_layout_key,
            shadows_bind_group_layout_key,
            texture_pool_arrays_len,
            texture_pool_sampler_keys,

            _main_bind_group: None,
            _mesh_material_bind_group: None,
            _texture_bind_group: None,
            _shadows_bind_group: None,
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
        } = TexturePoolDeps::new(ctx, TexturePoolVisibility::Render)?;

        let _self = Self {
            main_bind_group_layout_key: self.main_bind_group_layout_key,
            mesh_material_bind_group_layout_key: self.mesh_material_bind_group_layout_key,
            texture_pool_textures_bind_group_layout_key,
            shadows_bind_group_layout_key: self.shadows_bind_group_layout_key,
            texture_pool_arrays_len,
            texture_pool_sampler_keys,
            _main_bind_group: self._main_bind_group.clone(),
            _mesh_material_bind_group: self._mesh_material_bind_group.clone(),
            _texture_bind_group: None,
            _shadows_bind_group: self._shadows_bind_group.clone(),
        };

        Ok(_self)
    }

    /// Returns the live bind groups used for rendering, in dispatch
    /// slot order: `(main @0, shadows @1, texture_pool @2,
    /// mesh_material @3)`. Lights got folded into `main`, so the
    /// shadow bind group now lives at slot 1 (was the lights slot
    /// pre-16.B).
    pub fn get_bind_groups(
        &self,
    ) -> std::result::Result<
        (
            &web_sys::GpuBindGroup,
            &web_sys::GpuBindGroup,
            &web_sys::GpuBindGroup,
            &web_sys::GpuBindGroup,
        ),
        AwsmBindGroupError,
    > {
        match (
            &self._main_bind_group,
            &self._shadows_bind_group,
            &self._texture_bind_group,
            &self._mesh_material_bind_group,
        ) {
            (
                Some(main_bind_group),
                Some(shadows_bind_group),
                Some(texture_bind_group),
                Some(mesh_material_bind_group),
            ) => Ok((
                main_bind_group,
                shadows_bind_group,
                texture_bind_group,
                mesh_material_bind_group,
            )),
            (None, _, _, _) => Err(AwsmBindGroupError::NotFound(
                "Material Transparent - Main".to_string(),
            )),
            (_, None, _, _) => Err(AwsmBindGroupError::NotFound(
                "Material Transparent - Shadows".to_string(),
            )),
            (_, _, None, _) => Err(AwsmBindGroupError::NotFound(
                "Material Transparent - Texture Pool".to_string(),
            )),
            (_, _, _, None) => Err(AwsmBindGroupError::NotFound(
                "Material Transparent - Mesh Material".to_string(),
            )),
        }
    }

    /// Recreates the shadow bind group for transparent materials.
    /// Bound at slot 1 by `MaterialTransparentRenderPass::render`
    /// (after 16.B folded lights into `main`, freeing the slot).
    /// Transparent is at the adapter's `maxBindGroups = 4` ceiling
    /// — adding any *further* bind group would exceed budget without
    /// consolidating something else first.
    pub fn recreate_shadows(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let entries = build_shadow_bind_group_entries(ctx.shadows);

        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts
                .get(self.shadows_bind_group_layout_key)?,
            Some("Material Transparent - Shadows"),
            entries,
        );

        self._shadows_bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));

        Ok(())
    }

    /// Recreates the main bind group for transparent materials.
    pub fn recreate_main(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let mut entries = Vec::new();

        // camera
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
        ));

        // transform
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&ctx.transforms.gpu_buffer)),
        ));

        // materials
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&ctx.materials.gpu_buffer)),
        ));

        // morph weights
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(
                &ctx.meshes.morphs.geometry.gpu_buffer_weights,
            )),
        ));
        // morph values
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(
                &ctx.meshes.morphs.geometry.gpu_buffer_values,
            )),
        ));
        // skin matrices
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&ctx.meshes.skins.matrices_gpu_buffer)),
        ));
        // skin weights
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(
                &ctx.meshes.skins.joint_index_weights_gpu_buffer,
            )),
        ));
        // texture transforms
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(
                &ctx.textures.texture_transforms_gpu_buffer,
            )),
        ));
        // opaque texture — full mip chain so screen-space transmission can
        // sample pre-blurred neighborhoods at an explicit mip level.
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.opaque_full)),
        ));
        // Per-instance attribute storage buffer — matches the opaque path's
        // tint logic. Vertex shader reads it to plumb `instance_id` forward
        // (via meta.instance_attr_base + instance_index); fragment shader
        // reads it to apply per-instance color × tint.rgb + alpha × tint.a.
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(ctx.instances.gpu_attribute_buffer())),
        ));

        // ── Lights block (was a separate group; bindings 10..=17) ────
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
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(&ctx.lights.ibl.irradiance.texture_view)),
        ));
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Sampler(&ctx.lights.ibl.irradiance.sampler),
        ));
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(&ctx.lights.brdf_lut.view)),
        ));
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Sampler(&ctx.lights.brdf_lut.sampler),
        ));
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&ctx.lights.gpu_info_buffer)),
        ));
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&ctx.lights.gpu_punctual_buffer)),
        ));
        // FrameGlobals — rides alongside camera on the same group.
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&ctx.frame_globals.gpu_buffer)),
        ));
        // Extras pool — variable-length per-material data for custom
        // transparent materials.
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&ctx.extras_pool.buffer)),
        ));
        // GPU light-culling `cull_params` uniform.
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(
                &ctx.light_culling_buffers.params_buffer,
            )),
        ));
        // GPU light-culling `froxel_storage` (combined count + indices).
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(
                &ctx.light_culling_buffers.storage_buffer,
            )),
        ));

        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts
                .get(self.main_bind_group_layout_key)?,
            Some("Material Transparent - Main"),
            entries,
        );

        self._main_bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));

        Ok(())
    }

    /// Recreates the mesh/material bind group for transparent materials.
    pub fn recreate_mesh_material(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let mut entries = Vec::new();

        // geometry meta
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(
                BufferBinding::new(ctx.meshes.meta.geometry_gpu_buffer())
                    .with_size(GEOMETRY_MESH_META_BYTE_ALIGNMENT),
            ),
        ));

        // material meta
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(
                BufferBinding::new(ctx.meshes.meta.material_gpu_buffer())
                    .with_size(MATERIAL_MESH_META_BYTE_ALIGNMENT),
            ),
        ));

        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts
                .get(self.mesh_material_bind_group_layout_key)?,
            Some("Material Transparent - Mesh Material"),
            entries,
        );

        self._mesh_material_bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));

        Ok(())
    }

    // `recreate_lights` removed in 16.B — its entries are now built
    // by `recreate_main` (bindings 10..=17 of the merged `main`
    // group). Callers that previously invoked `recreate_lights`
    // should call `recreate_main` instead; the `BindGroupCreate`
    // enum still has a `LightsChange` variant for upstream signal
    // compatibility but it routes to `recreate_main` on this pass.

    /// Recreates the texture pool bind group for transparent materials.
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
            Some("Material Transparent - Texture Pool"),
            entries,
        );

        self._texture_bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));

        Ok(())
    }
}
