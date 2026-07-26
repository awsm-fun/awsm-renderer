//! Bind group layouts + recreation for the volumetrics compute pass.
//!
//! Three groups, which is what makes this pass possible at all: the adapter
//! ceiling is `maxBindGroups = 4` and both material passes are already at it
//! (the transparent pass can't bind shadows for exactly that reason). A compute
//! pass has its own budget, so volumetrics spends three and keeps one spare.
//!
//!   **group 0 — volume** (per stage; the two stages differ only in which
//!   volume they read and which they write):
//!     0 camera_raw        uniform
//!     1 volumetric_params uniform
//!     2 src_volume        texture_3d<f32>          (inject: unused dummy;
//!                                                   integrate: the scatter volume)
//!     3 dst_volume        texture_storage_3d write (inject: scatter;
//!                                                   integrate: integrated)
//!
//!   **group 1 — lights**: the fixed 4-entry shape every froxel consumer uses,
//!   copied from `material_prep`'s `create_lights_bind_group_layout_key` so the
//!   volume walks lights through the same `froxel_walk` SSOT the surfaces do.
//!
//!   **group 2 — shadows**: `shadow_bind_group_layout_entries(true)` verbatim
//!   (compute visibility). Sharing the layout is the point — the volume calls
//!   the same `sample_shadow_descriptor` the surfaces call, so it cannot
//!   disagree with the geometry about where the shadows are.

use awsm_renderer_core::{
    bind_groups::{
        BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
        BufferBindingLayout, BufferBindingType, StorageTextureAccess, StorageTextureBindingLayout,
        TextureBindingLayout,
    },
    buffers::BufferBinding,
    texture::{TextureFormat, TextureSampleType, TextureViewDimension},
};
use std::borrow::Cow;

use crate::{
    bind_group_layout::{
        BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey,
    },
    bind_groups::{AwsmBindGroupError, BindGroupRecreateContext},
    error::Result,
    render_passes::{
        shared::material::bind_group::{
            build_shadow_bind_group_entries, shadow_bind_group_layout_entries,
        },
        RenderPassInitContext,
    },
};

/// Layouts + the cached per-stage bind groups.
pub struct VolumetricsBindGroups {
    /// Shared by both stages — same shape, different textures bound.
    pub volume_layout_key: BindGroupLayoutKey,
    pub lights_layout_key: BindGroupLayoutKey,
    pub shadows_layout_key: BindGroupLayoutKey,
    inject: Option<web_sys::GpuBindGroup>,
    integrate: Option<web_sys::GpuBindGroup>,
    lights: Option<web_sys::GpuBindGroup>,
    shadows: Option<web_sys::GpuBindGroup>,
}

impl VolumetricsBindGroups {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let volume_entries = vec![
            // 0 camera_raw
            uniform_entry(),
            // 1 volumetric_params
            uniform_entry(),
            // 2 src_volume — sampled. UnfilterableFloat: the integrate stage
            // reads froxels by exact index (textureLoad), never interpolated.
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Texture(
                    TextureBindingLayout::new()
                        .with_view_dimension(TextureViewDimension::N3d)
                        .with_sample_type(TextureSampleType::UnfilterableFloat),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
            // 3 dst_volume — storage write.
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::StorageTexture(
                    StorageTextureBindingLayout::new(TextureFormat::Rgba16float)
                        .with_view_dimension(TextureViewDimension::N3d)
                        .with_access(StorageTextureAccess::WriteOnly),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
        ];
        let volume_layout_key = ctx.bind_group_layouts.get_key(
            ctx.gpu,
            BindGroupLayoutCacheKey {
                entries: volume_entries,
            },
        )?;

        // The 4-entry shape shared by opaque / prep / transparent. Kept
        // identical on purpose: the WGSL that reads it is the shared
        // `light_access.wgsl` + `froxel_walk.wgsl` include pair.
        let lights_layout_key = ctx.bind_group_layouts.get_key(
            ctx.gpu,
            BindGroupLayoutCacheKey {
                entries: vec![
                    // 0 lights_info
                    uniform_entry(),
                    // 1 lights (uniform array<LightPacked, MAX_PUNCTUAL_LIGHTS>)
                    uniform_entry(),
                    // 2 lights_storage (the cull pass's per-froxel lists)
                    storage_read_entry(),
                    // 3 cull_params
                    uniform_entry(),
                ],
            },
        )?;

        let shadows_layout_key = ctx.bind_group_layouts.get_key(
            ctx.gpu,
            BindGroupLayoutCacheKey {
                entries: shadow_bind_group_layout_entries(true),
            },
        )?;

        Ok(Self {
            volume_layout_key,
            lights_layout_key,
            shadows_layout_key,
            inject: None,
            integrate: None,
            lights: None,
            shadows: None,
        })
    }

    pub fn inject(&self) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.inject
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Volumetrics Inject".to_string()))
    }

    pub fn integrate(&self) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.integrate
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Volumetrics Integrate".to_string()))
    }

    pub fn lights(&self) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.lights
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Volumetrics Lights".to_string()))
    }

    pub fn shadows(&self) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.shadows
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Volumetrics Shadows".to_string()))
    }

    /// Rebuilds all four groups against the current volumes + params.
    pub fn recreate(
        &mut self,
        ctx: &BindGroupRecreateContext<'_>,
        texture: &super::texture::VolumetricsTexture,
        params: &web_sys::GpuBuffer,
    ) -> Result<()> {
        let layout = ctx.bind_group_layouts.get(self.volume_layout_key)?;

        // `inject` binds the scatter volume as its DESTINATION and has no real
        // source — but the layout is shared with `integrate`, so slot 2 needs
        // something. Binding the destination's own sample view is safe because
        // the inject shader never reads it (WebGPU only forbids simultaneous
        // read+write through the same *view*, and a shader that doesn't sample
        // can't race with itself).
        let mut inject_entries = self.common_entries(ctx, params);
        inject_entries.push(BindGroupEntry::new(
            2,
            BindGroupResource::TextureView(Cow::Borrowed(&texture.scatter_sample_view)),
        ));
        inject_entries.push(BindGroupEntry::new(
            3,
            BindGroupResource::TextureView(Cow::Borrowed(&texture.scatter_storage_view)),
        ));
        self.inject = Some(ctx.gpu.create_bind_group(
            &BindGroupDescriptor::new(layout, Some("Volumetrics Inject"), inject_entries).into(),
        ));

        let mut integrate_entries = self.common_entries(ctx, params);
        integrate_entries.push(BindGroupEntry::new(
            2,
            BindGroupResource::TextureView(Cow::Borrowed(&texture.scatter_sample_view)),
        ));
        integrate_entries.push(BindGroupEntry::new(
            3,
            BindGroupResource::TextureView(Cow::Borrowed(&texture.integrated_storage_view)),
        ));
        self.integrate = Some(
            ctx.gpu.create_bind_group(
                &BindGroupDescriptor::new(layout, Some("Volumetrics Integrate"), integrate_entries)
                    .into(),
            ),
        );

        let lights_entries = vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.lights.gpu_info_buffer)),
            ),
            BindGroupEntry::new(
                1,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.lights.gpu_punctual_buffer)),
            ),
            BindGroupEntry::new(
                2,
                BindGroupResource::Buffer(BufferBinding::new(
                    &ctx.light_culling_buffers.storage_buffer,
                )),
            ),
            BindGroupEntry::new(
                3,
                BindGroupResource::Buffer(BufferBinding::new(
                    &ctx.light_culling_buffers.params_buffer,
                )),
            ),
        ];
        self.lights = Some(
            ctx.gpu.create_bind_group(
                &BindGroupDescriptor::new(
                    ctx.bind_group_layouts.get(self.lights_layout_key)?,
                    Some("Volumetrics Lights"),
                    lights_entries,
                )
                .into(),
            ),
        );

        self.shadows = Some(
            ctx.gpu.create_bind_group(
                &BindGroupDescriptor::new(
                    ctx.bind_group_layouts.get(self.shadows_layout_key)?,
                    Some("Volumetrics Shadows"),
                    build_shadow_bind_group_entries(ctx.shadows),
                )
                .into(),
            ),
        );

        Ok(())
    }

    /// Bindings 0 + 1, identical across both stages.
    fn common_entries<'a>(
        &self,
        ctx: &'a BindGroupRecreateContext<'_>,
        params: &'a web_sys::GpuBuffer,
    ) -> Vec<BindGroupEntry<'a>> {
        vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
            ),
            BindGroupEntry::new(1, BindGroupResource::Buffer(BufferBinding::new(params))),
        ]
    }
}

fn uniform_entry() -> BindGroupLayoutCacheKeyEntry {
    BindGroupLayoutCacheKeyEntry {
        resource: BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
        ),
        visibility_vertex: false,
        visibility_fragment: false,
        visibility_compute: true,
    }
}

fn storage_read_entry() -> BindGroupLayoutCacheKeyEntry {
    BindGroupLayoutCacheKeyEntry {
        resource: BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
        ),
        visibility_vertex: false,
        visibility_fragment: false,
        visibility_compute: true,
    }
}
