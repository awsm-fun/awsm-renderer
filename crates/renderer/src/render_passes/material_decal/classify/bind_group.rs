//! Decal classify pass bind group.
//!
//! Single bind group with:
//!   0  decals_buffer (storage RO; the same buffer the shading pass reads)
//!   1  camera_raw    (uniform; for `view_proj` to project AABBs)
//!   2  buckets       (storage RW; atomics + per-tile entry array)
//!   3  hzb_texture   (sampled texture; HZB occlusion gate).
//!                    Only present when both `features.gpu_culling`
//!                    and `features.decals` are on — picked at
//!                    construction via `hzb_enabled`.

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    BufferBindingLayout, BufferBindingType, TextureBindingLayout,
};
use awsm_renderer_core::buffers::BufferBinding;
use awsm_renderer_core::texture::{TextureSampleType, TextureViewDimension};
use std::borrow::Cow;

use crate::bind_group_layout::{BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::{bind_group_layout::BindGroupLayoutKey, render_passes::RenderPassInitContext};

pub struct DecalClassifyBindGroups {
    pub layout_key: BindGroupLayoutKey,
    /// True when the classify shader includes the HZB occlusion gate.
    /// Matches `features.gpu_culling` at construction time — both the
    /// layout and the pipeline carry the matching HZB binding when set.
    pub hzb_enabled: bool,
    bind_group: Option<web_sys::GpuBindGroup>,
}

impl DecalClassifyBindGroups {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let hzb_enabled = ctx.features.gpu_culling;
        let mut entries = vec![
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
        if hzb_enabled {
            // HZB sampled texture. UnfilterableFloat keeps the
            // binding compatible with the renderer's `r32float` HZB
            // (no filterable sampler needed; the shader uses
            // `textureLoad`).
            entries.push(BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Texture(
                    TextureBindingLayout::new()
                        .with_view_dimension(TextureViewDimension::N2d)
                        .with_sample_type(TextureSampleType::UnfilterableFloat),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            });
        }
        let layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?;
        Ok(Self {
            layout_key,
            hzb_enabled,
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
        let decals = ctx
            .decals
            .expect("Decals subsystem missing despite decals feature on");
        let decal_classify_buffers = ctx
            .decal_classify_buffers
            .expect("decal classify buffers missing despite decals feature on");
        let mut entries = vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(decals.gpu_buffer())),
            ),
            BindGroupEntry::new(
                1,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
            ),
            BindGroupEntry::new(
                2,
                BindGroupResource::Buffer(BufferBinding::new(&decal_classify_buffers.buffer)),
            ),
        ];
        if self.hzb_enabled {
            let hzb_view = ctx
                .hzb_full_view
                .as_ref()
                .expect("HZB view missing despite gpu_culling feature on");
            entries.push(BindGroupEntry::new(
                3,
                BindGroupResource::TextureView(Cow::Borrowed(hzb_view)),
            ));
        }
        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(self.layout_key)?,
            Some("Decal Classify"),
            entries,
        );
        self.bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }
}
