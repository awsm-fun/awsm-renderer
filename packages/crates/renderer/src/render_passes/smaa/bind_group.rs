//! SMAA bind groups.
//!
//! Two layouts. The edges layout has 2 entries: the HDR composite
//! (unfilterable float, textureLoad) and the edges storage texture. The
//! weights layout has 5 entries: the edges texture (FILTERABLE — the
//! reference's bilinear fetch tricks depend on linear filtering), a linear
//! clamp sampler, AreaTex and SearchTex (the reference's precomputed pattern
//! textures, embedded from the canonical distribution), and the weights
//! storage texture.
//!
//! Bind groups are (re)built by the `FunctionToCall::Smaa` arm of the central
//! bind-group ledger whenever texture views recreate (resize / enable).

use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    SamplerBindingLayout, SamplerBindingType, StorageTextureAccess, StorageTextureBindingLayout,
    TextureBindingLayout,
};
use awsm_renderer_core::sampler::{AddressMode, FilterMode, SamplerDescriptor};
use awsm_renderer_core::texture::{TextureFormat, TextureSampleType, TextureViewDimension};

use crate::bind_group_layout::{
    BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey,
};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::render_passes::smaa::texture::SmaaTextures;
use crate::render_passes::RenderPassInitContext;

pub struct SmaaBindGroups {
    pub edges_layout_key: BindGroupLayoutKey,
    pub weights_layout_key: BindGroupLayoutKey,
    /// Linear clamp sampler for the reference bilinear-access tricks (edges)
    /// and the AreaTex/SearchTex lookups (which land on texel centers).
    sampler: web_sys::GpuSampler,
    edges_bind_group: Option<web_sys::GpuBindGroup>,
    weights_bind_group: Option<web_sys::GpuBindGroup>,
}

impl SmaaBindGroups {
    pub fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let edges_layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, edges_layout_cache_key())?;
        let weights_layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, weights_layout_cache_key())?;
        let sampler = ctx.gpu.create_sampler(Some(
            &SamplerDescriptor {
                label: Some("SMAA Linear Sampler"),
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
            edges_layout_key,
            weights_layout_key,
            sampler,
            edges_bind_group: None,
            weights_bind_group: None,
        })
    }

    pub fn edges(&self) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.edges_bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("SMAA Edges".to_string()))
    }

    pub fn weights(&self) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.weights_bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("SMAA Weights".to_string()))
    }

    /// Rebuild both bind groups against the current composite view + SMAA
    /// textures.
    pub fn recreate(
        &mut self,
        ctx: &BindGroupRecreateContext<'_>,
        textures: &SmaaTextures,
    ) -> Result<()> {
        let edges_layout = ctx.bind_group_layouts.get(self.edges_layout_key)?;
        self.edges_bind_group = Some(
            ctx.gpu.create_bind_group(
                &BindGroupDescriptor::new(
                    edges_layout,
                    Some("SMAA Edges"),
                    vec![
                        BindGroupEntry::new(
                            0,
                            BindGroupResource::TextureView(Cow::Borrowed(
                                &ctx.render_texture_views.composite,
                            )),
                        ),
                        BindGroupEntry::new(
                            1,
                            BindGroupResource::TextureView(Cow::Borrowed(&textures.edges_view)),
                        ),
                    ],
                )
                .into(),
            ),
        );

        let weights_layout = ctx.bind_group_layouts.get(self.weights_layout_key)?;
        self.weights_bind_group = Some(
            ctx.gpu.create_bind_group(
                &BindGroupDescriptor::new(
                    weights_layout,
                    Some("SMAA Weights"),
                    vec![
                        BindGroupEntry::new(
                            0,
                            BindGroupResource::TextureView(Cow::Borrowed(&textures.edges_view)),
                        ),
                        BindGroupEntry::new(1, BindGroupResource::Sampler(&self.sampler)),
                        BindGroupEntry::new(
                            2,
                            BindGroupResource::TextureView(Cow::Borrowed(&textures.area_view)),
                        ),
                        BindGroupEntry::new(
                            3,
                            BindGroupResource::TextureView(Cow::Borrowed(&textures.search_view)),
                        ),
                        BindGroupEntry::new(
                            4,
                            BindGroupResource::TextureView(Cow::Borrowed(&textures.weights_view)),
                        ),
                    ],
                )
                .into(),
            ),
        );

        Ok(())
    }
}

fn edges_layout_cache_key() -> BindGroupLayoutCacheKey {
    BindGroupLayoutCacheKey {
        entries: vec![
            // Composite (textureLoad only).
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
            // Edges storage target.
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::StorageTexture(
                    StorageTextureBindingLayout::new(TextureFormat::Rgba8unorm)
                        .with_view_dimension(TextureViewDimension::N2d)
                        .with_access(StorageTextureAccess::WriteOnly),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
        ],
    }
}

fn weights_layout_cache_key() -> BindGroupLayoutCacheKey {
    let filterable_tex = || BindGroupLayoutCacheKeyEntry {
        resource: BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::Float),
        ),
        visibility_vertex: false,
        visibility_fragment: false,
        visibility_compute: true,
    };
    BindGroupLayoutCacheKey {
        entries: vec![
            // Edges (filterable — bilinear access tricks).
            filterable_tex(),
            // Linear sampler.
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Sampler(
                    SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Filtering),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
            // AreaTex.
            filterable_tex(),
            // SearchTex.
            filterable_tex(),
            // Weights storage target.
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::StorageTexture(
                    StorageTextureBindingLayout::new(TextureFormat::Rgba8unorm)
                        .with_view_dimension(TextureViewDimension::N2d)
                        .with_access(StorageTextureAccess::WriteOnly),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
        ],
    }
}
