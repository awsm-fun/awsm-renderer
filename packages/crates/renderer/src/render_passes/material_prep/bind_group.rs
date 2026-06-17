//! Bind group layout + recreation for the material prep compute pass
//! (Plan B, docs/plans/deferred-shared-prep-pass.md).
//!
//! Three bind groups; layouts must stay in lockstep with the WGSL emitted by
//! [`super::shader::template::ShaderTemplateMaterialPrepBindGroups`]:
//!
//! group(0) — main:
//!   0 visibility_data_tex   — uint texture (triangle id + meta offset). MSAA
//!     variant is multisampled.
//!   1 barycentric_tex       — uint texture (bary weights + instance id). MSAA
//!     variant is multisampled.
//!   2 visibility_data       — storage RO, the merged geometry pool.
//!   3 material_mesh_metas   — storage RO, per-mesh metadata table.
//!   4 uv_out                — storage texture array (rg32float, write).
//!   5 vcolor_out            — storage texture array (rgba32float, write).
//!   6 depth_tex             — depth texture (Stage 3b — world-pos from depth).
//!   7 normal_tangent_tex    — float texture (Stage 3b — surface normal).
//!   8 camera_raw            — uniform (Stage 3b — unproject + view_z + sscs).
//!   9 shadow_visibility_out — storage texture array (rgba8unorm, write) — the
//!     packed per-pixel shadow-visibility output.
//!
//! group(1) — lights (mirror material_opaque's light group):
//!   0 lights_info (Uniform), 1 lights (Uniform), 2 lights_storage
//!   (ReadOnlyStorage), 3 cull_params (Uniform).
//!
//! group(2) — shadows (`shadow_bind_group_layout_entries(true)` — the 10 shadow
//!   sampling bindings prep needs as a shadow sampler).
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
use crate::render_passes::shared::material::bind_group::{
    build_shadow_bind_group_entries, shadow_bind_group_layout_entries,
};
use crate::render_passes::RenderPassInitContext;

/// Bind group layout(s) + cached bind groups for the prep pass.
pub struct MaterialPrepBindGroups {
    pub multisampled_bind_group_layout_key: BindGroupLayoutKey,
    pub singlesampled_bind_group_layout_key: BindGroupLayoutKey,
    pub lights_bind_group_layout_key: BindGroupLayoutKey,
    pub shadows_bind_group_layout_key: BindGroupLayoutKey,
    bind_group: Option<web_sys::GpuBindGroup>,
    lights_bind_group: Option<web_sys::GpuBindGroup>,
    shadows_bind_group: Option<web_sys::GpuBindGroup>,
}

impl MaterialPrepBindGroups {
    /// Creates the bind group layouts for the prep pass (both MSAA-geometry
    /// variants for group 0, plus the shared lights + shadows layouts). The
    /// bind groups themselves are built lazily via [`Self::recreate`] when the
    /// renderer's recreate events fire.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let multisampled_bind_group_layout_key = create_main_bind_group_layout_key(ctx, true)?;
        let singlesampled_bind_group_layout_key = create_main_bind_group_layout_key(ctx, false)?;
        let lights_bind_group_layout_key = create_lights_bind_group_layout_key(ctx)?;
        let shadows_bind_group_layout_key = ctx.bind_group_layouts.get_key(
            ctx.gpu,
            BindGroupLayoutCacheKey {
                entries: shadow_bind_group_layout_entries(true),
            },
        )?;

        Ok(Self {
            multisampled_bind_group_layout_key,
            singlesampled_bind_group_layout_key,
            lights_bind_group_layout_key,
            shadows_bind_group_layout_key,
            bind_group: None,
            lights_bind_group: None,
            shadows_bind_group: None,
        })
    }

    /// Returns the live group(0) prep bind group.
    pub fn get_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Material Prep".to_string()))
    }

    /// Returns the live group(1) lights bind group.
    pub fn get_lights_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.lights_bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Material Prep - Lights".to_string()))
    }

    /// Returns the live group(2) shadows bind group.
    pub fn get_shadows_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.shadows_bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Material Prep - Shadows".to_string()))
    }

    /// (Re)builds all three prep bind groups. Called from
    /// [`crate::bind_groups::BindGroups`] in response to any of prep's recreate
    /// events (texture-view recreate, mesh-meta / geometry-pool resize, lights /
    /// shadows / froxel-buffer recreate). Rebuilding all three on every trigger
    /// keeps the wiring simple and is cheap (bind-group creation only).
    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        self.recreate_main(ctx)?;
        self.recreate_lights(ctx)?;
        self.recreate_shadows(ctx)?;
        Ok(())
    }

    fn recreate_main(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
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
        let shadow_visibility_out =
            ctx.render_texture_views
                .prep_shadow_visibility
                .as_ref()
                .ok_or_else(|| {
                    AwsmBindGroupError::NotFound(
                        "Material Prep - shadow_visibility_out".to_string(),
                    )
                })?;

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
            // 6 depth_tex.
            BindGroupEntry::new(
                6,
                BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.depth)),
            ),
            // 7 normal_tangent_tex.
            BindGroupEntry::new(
                7,
                BindGroupResource::TextureView(Cow::Borrowed(
                    &ctx.render_texture_views.normal_tangent,
                )),
            ),
            // 8 camera_raw — uniform.
            BindGroupEntry::new(
                8,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
            ),
            // 9 shadow_visibility_out — storage texture array (write).
            BindGroupEntry::new(
                9,
                BindGroupResource::TextureView(Cow::Borrowed(shadow_visibility_out)),
            ),
        ];

        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(layout_key)?,
            Some("Material Prep"),
            entries,
        );
        self.bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }

    fn recreate_lights(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let entries = vec![
            // 0 lights_info.
            BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.lights.gpu_info_buffer)),
            ),
            // 1 lights (punctual uniform array).
            BindGroupEntry::new(
                1,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.lights.gpu_punctual_buffer)),
            ),
            // 2 lights_storage (merged mesh + froxel slices).
            BindGroupEntry::new(
                2,
                BindGroupResource::Buffer(BufferBinding::new(
                    &ctx.light_culling_buffers.storage_buffer,
                )),
            ),
            // 3 cull_params uniform.
            BindGroupEntry::new(
                3,
                BindGroupResource::Buffer(BufferBinding::new(
                    &ctx.light_culling_buffers.params_buffer,
                )),
            ),
        ];

        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(self.lights_bind_group_layout_key)?,
            Some("Material Prep - Lights"),
            entries,
        );
        self.lights_bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }

    fn recreate_shadows(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let entries = build_shadow_bind_group_entries(ctx.shadows);

        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts
                .get(self.shadows_bind_group_layout_key)?,
            Some("Material Prep - Shadows"),
            entries,
        );
        self.shadows_bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }
}

fn create_main_bind_group_layout_key(
    ctx: &mut RenderPassInitContext<'_>,
    multisampled_geometry: bool,
) -> Result<BindGroupLayoutKey> {
    let compute = |resource: BindGroupLayoutResource| BindGroupLayoutCacheKeyEntry {
        resource,
        visibility_vertex: false,
        visibility_fragment: false,
        visibility_compute: true,
    };

    let entries = vec![
        // 0 visibility_data_tex — uint texture; MSAA variant is multisampled.
        compute(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::Uint)
                .with_multisampled(multisampled_geometry),
        )),
        // 1 barycentric_tex — uint texture; MSAA variant is multisampled.
        compute(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::Uint)
                .with_multisampled(multisampled_geometry),
        )),
        // 2 visibility_data — merged geometry pool (storage RO).
        compute(BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
        )),
        // 3 material_mesh_metas — storage RO.
        compute(BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
        )),
        // 4 uv_out — storage texture ARRAY (rg32float, write).
        compute(BindGroupLayoutResource::StorageTexture(
            StorageTextureBindingLayout::new(TextureFormat::Rg32float)
                .with_view_dimension(TextureViewDimension::N2dArray)
                .with_access(StorageTextureAccess::WriteOnly),
        )),
        // 5 vcolor_out — storage texture ARRAY (rgba32float, write).
        compute(BindGroupLayoutResource::StorageTexture(
            StorageTextureBindingLayout::new(TextureFormat::Rgba32float)
                .with_view_dimension(TextureViewDimension::N2dArray)
                .with_access(StorageTextureAccess::WriteOnly),
        )),
        // 6 depth_tex — depth texture; MSAA variant is multisampled (matches the
        // geometry pass's depth target). Prep reads sample 0 either way.
        compute(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::Depth)
                .with_multisampled(multisampled_geometry),
        )),
        // 7 normal_tangent_tex — float texture; MSAA variant is multisampled.
        compute(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::UnfilterableFloat)
                .with_multisampled(multisampled_geometry),
        )),
        // 8 camera_raw — uniform.
        compute(BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
        )),
        // 9 shadow_visibility_out — storage texture ARRAY (rgba8unorm, write).
        compute(BindGroupLayoutResource::StorageTexture(
            StorageTextureBindingLayout::new(TextureFormat::Rgba8unorm)
                .with_view_dimension(TextureViewDimension::N2dArray)
                .with_access(StorageTextureAccess::WriteOnly),
        )),
    ];

    Ok(ctx
        .bind_group_layouts
        .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?)
}

fn create_lights_bind_group_layout_key(
    ctx: &mut RenderPassInitContext<'_>,
) -> Result<BindGroupLayoutKey> {
    let compute = |binding_type: BufferBindingType| BindGroupLayoutCacheKeyEntry {
        resource: BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(binding_type),
        ),
        visibility_vertex: false,
        visibility_fragment: false,
        visibility_compute: true,
    };

    let entries = vec![
        // 0 lights_info (Uniform).
        compute(BufferBindingType::Uniform),
        // 1 lights (Uniform array).
        compute(BufferBindingType::Uniform),
        // 2 lights_storage (ReadOnlyStorage).
        compute(BufferBindingType::ReadOnlyStorage),
        // 3 cull_params (Uniform).
        compute(BufferBindingType::Uniform),
    ];

    Ok(ctx
        .bind_group_layouts
        .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?)
}
