//! Decal classify pass bind group.
//!
//! Single bind group with:
//!   0  decals_buffer (storage RO; the same buffer the shading pass reads)
//!   1  camera_raw    (uniform; for `view_proj` to project AABBs)
//!   2  buckets       (storage RW; atomics + per-tile entry array)

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    BufferBindingLayout, BufferBindingType,
};
use awsm_renderer_core::buffers::BufferBinding;

use crate::bind_group_layout::{BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::{bind_group_layout::BindGroupLayoutKey, render_passes::RenderPassInitContext};

pub struct DecalClassifyBindGroups {
    pub layout_key: BindGroupLayoutKey,
    bind_group: Option<web_sys::GpuBindGroup>,
}

impl DecalClassifyBindGroups {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let entries = vec![
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::ReadOnlyStorage),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
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
            bind_group: None,
        })
    }

    pub fn get_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Decal Classify".to_string()))
    }

    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let entries = vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(ctx.decals.gpu_buffer())),
            ),
            BindGroupEntry::new(
                1,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
            ),
            BindGroupEntry::new(
                2,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.decal_classify_buffers.buffer)),
            ),
        ];
        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(self.layout_key)?,
            Some("Decal Classify"),
            entries,
        );
        self.bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }
}
