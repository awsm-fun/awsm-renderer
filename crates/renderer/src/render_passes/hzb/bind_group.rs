//! HZB bind groups.
//!
//! Two bind-group layouts:
//! - **Seed**: depth_tex (sampled) + hzb_mip0 (storage write).
//!   MSAA-variant aware.
//! - **Reduce**: src mip (sampled, mip N-1) + dst mip (storage write,
//!   mip N). One bind group per mip transition since each mip has
//!   its own single-level view.

use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    StorageTextureAccess, StorageTextureBindingLayout, TextureBindingLayout,
};
use awsm_renderer_core::texture::{TextureFormat, TextureSampleType, TextureViewDimension};

use crate::bind_group_layout::{BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::render_passes::hzb::texture::HzbTexture;
use crate::{bind_group_layout::BindGroupLayoutKey, render_passes::RenderPassInitContext};

pub struct HzbBindGroups {
    pub seed_layout_key_msaa: BindGroupLayoutKey,
    pub seed_layout_key_single: BindGroupLayoutKey,
    pub reduce_layout_key: BindGroupLayoutKey,
    /// Seed bind group (depth → mip 0). Rebuilt when render textures
    /// resize or the HZB texture is recreated.
    seed_bind_group: Option<web_sys::GpuBindGroup>,
    /// One bind group per mip transition `N-1 → N`. Length =
    /// `mip_count - 1` after `recreate_reduce_bind_groups` runs.
    reduce_bind_groups: Vec<web_sys::GpuBindGroup>,
}

impl HzbBindGroups {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let seed_layout_key_msaa = create_seed_layout(ctx, true).await?;
        let seed_layout_key_single = create_seed_layout(ctx, false).await?;
        let reduce_layout_key = create_reduce_layout(ctx).await?;
        Ok(Self {
            seed_layout_key_msaa,
            seed_layout_key_single,
            reduce_layout_key,
            seed_bind_group: None,
            reduce_bind_groups: Vec::new(),
        })
    }

    pub fn seed(&self) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.seed_bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("HZB Seed".to_string()))
    }

    pub fn reduce_at(
        &self,
        mip_transition: usize,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.reduce_bind_groups.get(mip_transition).ok_or_else(|| {
            AwsmBindGroupError::NotFound(format!("HZB Reduce mip transition {}", mip_transition))
        })
    }

    /// Rebuilds both the seed bind group and the per-mip-transition
    /// reduce bind groups against the current `HzbTexture` + depth
    /// texture view. Called on viewport resize (which recreated both
    /// the depth and HZB textures) and any time `BindGroups`
    /// triggers a `TextureViewRecreate` event.
    pub fn recreate(
        &mut self,
        ctx: &BindGroupRecreateContext<'_>,
        hzb: &HzbTexture,
    ) -> Result<()> {
        // Seed.
        let seed_layout = if ctx.anti_aliasing.msaa_sample_count.is_some() {
            self.seed_layout_key_msaa
        } else {
            self.seed_layout_key_single
        };
        let entries = vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.depth)),
            ),
            BindGroupEntry::new(
                1,
                BindGroupResource::TextureView(Cow::Borrowed(&hzb.views_per_mip[0])),
            ),
        ];
        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(seed_layout)?,
            Some("HZB Seed"),
            entries,
        );
        self.seed_bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));

        // Reduce — one per mip transition.
        self.reduce_bind_groups.clear();
        for n in 1..hzb.mip_count as usize {
            let entries = vec![
                BindGroupEntry::new(
                    0,
                    BindGroupResource::TextureView(Cow::Borrowed(&hzb.views_per_mip[n - 1])),
                ),
                BindGroupEntry::new(
                    1,
                    BindGroupResource::TextureView(Cow::Borrowed(&hzb.views_per_mip[n])),
                ),
            ];
            let descriptor = BindGroupDescriptor::new(
                ctx.bind_group_layouts.get(self.reduce_layout_key)?,
                Some("HZB Reduce"),
                entries,
            );
            self.reduce_bind_groups
                .push(ctx.gpu.create_bind_group(&descriptor.into()));
        }
        Ok(())
    }
}

async fn create_seed_layout(
    ctx: &mut RenderPassInitContext<'_>,
    multisampled_geometry: bool,
) -> Result<BindGroupLayoutKey> {
    let entries = vec![
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
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::StorageTexture(
                StorageTextureBindingLayout::new(TextureFormat::R32float)
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

async fn create_reduce_layout(ctx: &mut RenderPassInitContext<'_>) -> Result<BindGroupLayoutKey> {
    let entries = vec![
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
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::StorageTexture(
                StorageTextureBindingLayout::new(TextureFormat::R32float)
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
