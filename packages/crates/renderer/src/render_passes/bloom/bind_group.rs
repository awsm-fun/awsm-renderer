//! Bloom bind groups.
//!
//! Two layouts:
//! - the shared 4-entry layout (sampled color texture + linear sampler +
//!   `BloomParams` uniform + `rgba16float` storage-write target) used by
//!   **prefilter** (composite → pyramid mip 0), **downsample** (pyramid mip
//!   N-1 → mip N, one bind group per transition) and **combine** (accumulated
//!   up-pyramid → full-res `bloom` target);
//! - the 5-entry **upsample** layout, which adds a second sampled texture:
//!   the coarse accumulated source (mip N) plus the down-pyramid base (mip
//!   N-1, `textureLoad`) feeding the up-pyramid mip N-1 storage target.

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
    /// Upsample layout — the shared shape plus a second sampled texture (the
    /// down-pyramid accumulation base).
    pub upsample_layout_key: BindGroupLayoutKey,
    /// Linear, clamp-to-edge sampler used to fetch color (filterable
    /// rgba16float) across every step.
    sampler: web_sys::GpuSampler,
    /// Prefilter bind group (composite → pyramid mip 0). Rebuilt on resize /
    /// texture recreate.
    prefilter_bind_group: Option<web_sys::GpuBindGroup>,
    /// One bind group per pyramid transition `N-1 → N`. Length =
    /// `mip_count - 1` after `recreate`.
    downsample_bind_groups: Vec<web_sys::GpuBindGroup>,
    /// One bind group per upsample DESTINATION mip `d` (finest-first index;
    /// the dispatch loop walks them coarsest → finest). Length =
    /// `mip_count - 1` after `recreate`.
    upsample_bind_groups: Vec<web_sys::GpuBindGroup>,
    /// Combine bind group (accumulated up-pyramid → full-res bloom target).
    combine_bind_group: Option<web_sys::GpuBindGroup>,
}

impl BloomBindGroups {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let layout_key = create_layout(ctx)?;
        let upsample_layout_key = create_upsample_layout(ctx)?;
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
            upsample_layout_key,
            sampler,
            prefilter_bind_group: None,
            downsample_bind_groups: Vec::new(),
            upsample_bind_groups: Vec::new(),
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

    /// Upsample bind group writing up-pyramid mip `dst_mip` (from source mip
    /// `dst_mip + 1`). Dispatch order is coarsest → finest, i.e. descending
    /// `dst_mip`.
    pub fn upsample_at(
        &self,
        dst_mip: usize,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.upsample_bind_groups.get(dst_mip).ok_or_else(|| {
            AwsmBindGroupError::NotFound(format!("Bloom Upsample dst mip {}", dst_mip))
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
                BindGroupEntry::new(
                    2,
                    BindGroupResource::Buffer(BufferBinding::new(params_buffer)),
                ),
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
                BindGroupEntry::new(
                    2,
                    BindGroupResource::Buffer(BufferBinding::new(params_buffer)),
                ),
                BindGroupEntry::new(
                    3,
                    BindGroupResource::TextureView(Cow::Borrowed(&tex.views_per_mip[n])),
                ),
            ];
            let descriptor = BindGroupDescriptor::new(layout, Some("Bloom Downsample"), entries);
            self.downsample_bind_groups
                .push(ctx.gpu.create_bind_group(&descriptor.into()));
        }

        // Upsample — one bind group per DESTINATION mip d (executed in
        // descending d, coarsest → finest). The coarse source is the DOWN
        // pyramid's coarsest level on the first step and the accumulated UP
        // level otherwise; the accumulation base is down-pyramid mip d; the
        // write target is up-pyramid mip d. Every sampled view is mip-scoped,
        // so the sampled and storage subresources stay disjoint.
        let upsample_layout = ctx.bind_group_layouts.get(self.upsample_layout_key)?;
        self.upsample_bind_groups.clear();
        for d in 0..tex.mip_count.saturating_sub(1) as usize {
            let coarse_src = if d as u32 == tex.mip_count - 2 {
                &tex.views_per_mip[d + 1]
            } else {
                &tex.up_views_per_mip[d + 1]
            };
            let entries = vec![
                BindGroupEntry::new(0, BindGroupResource::TextureView(Cow::Borrowed(coarse_src))),
                BindGroupEntry::new(1, BindGroupResource::Sampler(&self.sampler)),
                BindGroupEntry::new(
                    2,
                    BindGroupResource::Buffer(BufferBinding::new(params_buffer)),
                ),
                BindGroupEntry::new(
                    3,
                    BindGroupResource::TextureView(Cow::Borrowed(&tex.views_per_mip[d])),
                ),
                BindGroupEntry::new(
                    4,
                    BindGroupResource::TextureView(Cow::Borrowed(&tex.up_views_per_mip[d])),
                ),
            ];
            let descriptor =
                BindGroupDescriptor::new(upsample_layout, Some("Bloom Upsample"), entries);
            self.upsample_bind_groups
                .push(ctx.gpu.create_bind_group(&descriptor.into()));
        }

        // Combine — accumulated up-pyramid → full-res bloom target. With a
        // single-level pyramid there is nothing to upsample (the up pyramid
        // is never written), so read the down pyramid directly; either way
        // the all-mips view keeps `textureNumLevels` == contributing levels
        // for the shader's scatter-weight normalization.
        {
            let combine_src = if tex.mip_count > 1 {
                &tex.up_view_all
            } else {
                &tex.view_all
            };
            let entries = vec![
                BindGroupEntry::new(
                    0,
                    BindGroupResource::TextureView(Cow::Borrowed(combine_src)),
                ),
                BindGroupEntry::new(1, BindGroupResource::Sampler(&self.sampler)),
                BindGroupEntry::new(
                    2,
                    BindGroupResource::Buffer(BufferBinding::new(params_buffer)),
                ),
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

/// Upsample layout: coarse sampled texture (mip N, filterable) + linear
/// sampler + `BloomParams` uniform + down-pyramid base texture (mip N-1,
/// `textureLoad`) + `rgba16float` storage-write target (up-pyramid mip N-1).
fn create_upsample_layout(ctx: &mut RenderPassInitContext<'_>) -> Result<BindGroupLayoutKey> {
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
