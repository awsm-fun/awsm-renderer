//! Bind group layout + recreation for the occlusion cull pass.
//!
//! Single bind group:
//!   0 camera          uniform (CameraRaw)
//!   1 hzb_tex         sampled R32Float texture (full mip chain)
//!   2 instances       storage[RO] OcclusionInstance array
//!   3 visible         storage[RW] u32 array
//!   4 params          uniform (active_count)

use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    BufferBindingLayout, BufferBindingType, TextureBindingLayout,
};
use awsm_renderer_core::buffers::BufferBinding;
use awsm_renderer_core::texture::{TextureSampleType, TextureViewDimension};

use crate::bind_group_layout::{BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::{bind_group_layout::BindGroupLayoutKey, render_passes::RenderPassInitContext};

pub struct OcclusionBindGroups {
    pub layout_key: BindGroupLayoutKey,
    bind_group: Option<web_sys::GpuBindGroup>,
}

impl OcclusionBindGroups {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let entries = vec![
            // camera
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
            // hzb_tex — sampled `r32float`
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
            // instances — storage RO
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::ReadOnlyStorage),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
            // visible_this_frame — storage RW
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new().with_binding_type(BufferBindingType::Storage),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
            // params — uniform (active_count)
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
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
            .ok_or_else(|| AwsmBindGroupError::NotFound("Occlusion".to_string()))
    }

    /// Rebuilds the bind group against the live HZB view and the
    /// occlusion buffers. Called when:
    /// - HZB texture was reallocated (`TextureViewRecreate`).
    /// - Occlusion buffers grew (`OcclusionBuffersResize`).
    ///
    /// Only invoked when `features.gpu_culling` is on (plan §16.F),
    /// so the gated context fields are always `Some` here.
    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        // Use the HZB's mip-0 view as the full-chain sampler view? The
        // existing per-mip views don't include the full chain. For the
        // cull's HZB lookup we need to sample at varying mip levels —
        // that requires the texture's *default* view (whole mip chain).
        // Both `hzb.views_per_mip[0]` (single-level) and a full-chain
        // view differ; the latter is what we want here.
        let hzb_full_view = ctx
            .hzb_full_view
            .as_ref()
            .expect("HZB view missing despite gpu_culling feature on");
        let occlusion_buffers = ctx
            .occlusion_buffers
            .expect("Occlusion buffers missing despite gpu_culling feature on");
        let entries = vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
            ),
            BindGroupEntry::new(
                1,
                BindGroupResource::TextureView(Cow::Borrowed(hzb_full_view)),
            ),
            BindGroupEntry::new(
                2,
                BindGroupResource::Buffer(BufferBinding::new(
                    &occlusion_buffers.instances_buffer,
                )),
            ),
            BindGroupEntry::new(
                3,
                BindGroupResource::Buffer(BufferBinding::new(
                    &occlusion_buffers.visible_buffer,
                )),
            ),
            BindGroupEntry::new(
                4,
                BindGroupResource::Buffer(BufferBinding::new(
                    &occlusion_buffers.params_buffer,
                )),
            ),
        ];
        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(self.layout_key)?,
            Some("Occlusion"),
            entries,
        );
        self.bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }
}
