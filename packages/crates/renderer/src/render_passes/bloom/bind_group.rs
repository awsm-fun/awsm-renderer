//! Bloom bind groups.
//!
//! One shared bind-group layout for all three steps — they are structurally
//! identical (sampled color texture + linear sampler + `BloomParams` uniform +
//! `rgba16float` storage-write target):
//! - **prefilter**: composite (full-res) → pyramid mip 0.
//! - **downsample**: pyramid mip N-1 → pyramid mip N (one bind group per
//!   transition, since each mip has its own single-level storage view).
//! - **combine**: pyramid `view_all` (all mips) → full-res `bloom` target.

use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    BufferBindingLayout, BufferBindingType, SamplerBindingLayout, SamplerBindingType,
    StorageTextureAccess, StorageTextureBindingLayout, TextureBindingLayout,
};
use awsm_renderer_core::buffers::BufferBinding;
use awsm_renderer_core::sampler::{AddressMode, FilterMode, SamplerDescriptor};
use awsm_renderer_core::texture::{TextureFormat, TextureSampleType, TextureViewDimension};

use crate::bind_group_layout::{
    BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey,
};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::render_passes::bloom::texture::BloomTexture;
use crate::render_passes::RenderPassInitContext;

pub struct BloomBindGroups {
    /// Shared layout for prefilter / downsample / combine (identical shapes).
    pub layout_key: BindGroupLayoutKey,
    /// Linear, clamp-to-edge sampler used to fetch color (filterable
    /// rgba16float) across every step.
    sampler: web_sys::GpuSampler,
    /// Prefilter bind group (composite → pyramid mip 0). Rebuilt on resize /
    /// texture recreate.
    prefilter_bind_group: Option<web_sys::GpuBindGroup>,
    /// One bind group per pyramid transition `N-1 → N`. Length =
    /// `mip_count - 1` after `recreate`.
    downsample_bind_groups: Vec<web_sys::GpuBindGroup>,
    /// Combine bind group (pyramid `view_all` → full-res bloom target).
    combine_bind_group: Option<web_sys::GpuBindGroup>,
}

impl BloomBindGroups {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let layout_key = create_layout(ctx)?;
        let sampler = ctx.gpu.create_sampler(Some(
            &SamplerDescriptor {
                label: Some("Bloom Linear Sampler"),
                mag_filter: Some(FilterMode::Linear),
                min_filter: Some(FilterMode::Linear),
                address_mode_u: Some(AddressMode::ClampToEdge),
                address_mode_v: Some(AddressMode::ClampToEdge),
                address_mode_w: Some(AddressMode::ClampToEdge),
                ..SamplerDescriptor::default()
            }
            .into(),
        ));
        Ok(Self {
            layout_key,
            sampler,
            prefilter_bind_group: None,
            downsample_bind_groups: Vec::new(),
            combine_bind_group: None,
        })
    }

    pub fn prefilter(&self) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.prefilter_bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Bloom Prefilter".to_string()))
    }

    pub fn downsample_at(
        &self,
        mip_transition: usize,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.downsample_bind_groups
            .get(mip_transition)
            .ok_or_else(|| {
                AwsmBindGroupError::NotFound(format!(
                    "Bloom Downsample mip transition {}",
                    mip_transition
                ))
            })
    }

    pub fn combine(&self) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.combine_bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Bloom Combine".to_string()))
    }

    /// Rebuilds all bloom bind groups against the current `BloomTexture`, the
    /// live composite / bloom render-texture views, and the params uniform.
    /// Called on viewport resize and any `TextureViewRecreate` event.
    pub fn recreate(
        &mut self,
        ctx: &BindGroupRecreateContext<'_>,
        tex: &BloomTexture,
        params_buffer: &web_sys::GpuBuffer,
    ) -> Result<()> {
        let layout = ctx.bind_group_layouts.get(self.layout_key)?;

        // Prefilter — composite (full-res) → pyramid mip 0.
        {
            let entries = vec![
                BindGroupEntry::new(
                    0,
                    BindGroupResource::TextureView(Cow::Borrowed(
                        &ctx.render_texture_views.composite,
                    )),
                ),
                BindGroupEntry::new(1, BindGroupResource::Sampler(&self.sampler)),
                BindGroupEntry::new(2, BindGroupResource::Buffer(BufferBinding::new(params_buffer))),
                BindGroupEntry::new(
                    3,
                    BindGroupResource::TextureView(Cow::Borrowed(&tex.views_per_mip[0])),
                ),
            ];
            let descriptor = BindGroupDescriptor::new(layout, Some("Bloom Prefilter"), entries);
            self.prefilter_bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        }

        // Downsample — one bind group per pyramid transition N-1 → N.
        self.downsample_bind_groups.clear();
        for n in 1..tex.mip_count as usize {
            let entries = vec![
                BindGroupEntry::new(
                    0,
                    BindGroupResource::TextureView(Cow::Borrowed(&tex.views_per_mip[n - 1])),
                ),
                BindGroupEntry::new(1, BindGroupResource::Sampler(&self.sampler)),
                BindGroupEntry::new(2, BindGroupResource::Buffer(BufferBinding::new(params_buffer))),
                BindGroupEntry::new(
                    3,
                    BindGroupResource::TextureView(Cow::Borrowed(&tex.views_per_mip[n])),
                ),
            ];
            let descriptor = BindGroupDescriptor::new(layout, Some("Bloom Downsample"), entries);
            self.downsample_bind_groups
                .push(ctx.gpu.create_bind_group(&descriptor.into()));
        }

        // Combine — pyramid `view_all` (all mips) → full-res bloom target.
        {
            let entries = vec![
                BindGroupEntry::new(
                    0,
                    BindGroupResource::TextureView(Cow::Borrowed(&tex.view_all)),
                ),
                BindGroupEntry::new(1, BindGroupResource::Sampler(&self.sampler)),
                BindGroupEntry::new(2, BindGroupResource::Buffer(BufferBinding::new(params_buffer))),
                BindGroupEntry::new(
                    3,
                    BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.bloom)),
                ),
            ];
            let descriptor = BindGroupDescriptor::new(layout, Some("Bloom Combine"), entries);
            self.combine_bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        }

        Ok(())
    }
}

/// Shared layout: sampled color texture (filterable) + linear sampler +
/// `BloomParams` uniform + `rgba16float` storage-write target.
fn create_layout(ctx: &mut RenderPassInitContext<'_>) -> Result<BindGroupLayoutKey> {
    let entries = vec![
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Texture(
                TextureBindingLayout::new()
                    .with_view_dimension(TextureViewDimension::N2d)
                    .with_sample_type(TextureSampleType::Float),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Sampler(
                SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Filtering),
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
            resource: BindGroupLayoutResource::StorageTexture(
                StorageTextureBindingLayout::new(TextureFormat::Rgba16float)
                    .with_view_dimension(TextureViewDimension::N2d)
                    .with_access(StorageTextureAccess::WriteOnly),
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
