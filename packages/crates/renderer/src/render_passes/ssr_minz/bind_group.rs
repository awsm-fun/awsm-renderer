//! SSR min-Z pyramid bind groups.
//!
//! Two bind-group layouts (mirrors `hzb::bind_group`, minus the MSAA
//! lazy-pool: we compile the single seed variant matching the live AA,
//! exactly like the SSR trace compiles its single depth-binding variant):
//! - **Seed**: depth_tex (sampled, MSAA-aware) + minz_mip0 (storage write).
//! - **Reduce**: src mip (sampled, mip N-1) + dst mip (storage write, mip N).
//!   One bind group per mip transition since each mip has its own
//!   single-level view.

use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    StorageTextureAccess, StorageTextureBindingLayout, TextureBindingLayout,
};
use awsm_renderer_core::texture::{TextureFormat, TextureSampleType, TextureViewDimension};

use crate::bind_group_layout::{BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::render_passes::ssr_minz::texture::SsrMinzTexture;
use crate::{bind_group_layout::BindGroupLayoutKey, render_passes::RenderPassInitContext};

pub struct SsrMinzBindGroups {
    pub seed_layout_key: BindGroupLayoutKey,
    pub reduce_layout_key: BindGroupLayoutKey,
    /// Seed bind group (depth → mip 0). Rebuilt when render textures
    /// resize or the pyramid texture is recreated.
    seed_bind_group: Option<web_sys::GpuBindGroup>,
    /// One bind group per mip transition `N-1 → N`. Length =
    /// `mip_count - 1` after `recreate` runs.
    reduce_bind_groups: Vec<web_sys::GpuBindGroup>,
}

impl SsrMinzBindGroups {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        // Single seed layout matching the live AA — the SSR trace reads depth
        // the same way, so there's exactly one variant to compile (no lazy
        // MSAA pool like the occlusion HZB needs).
        let multisampled = ctx.anti_aliasing.msaa_sample_count.is_some();
        let seed_layout_key = create_seed_layout(ctx, multisampled)?;
        let reduce_layout_key = create_reduce_layout(ctx)?;
        Ok(Self {
            seed_layout_key,
            reduce_layout_key,
            seed_bind_group: None,
            reduce_bind_groups: Vec::new(),
        })
    }

    pub fn seed(&self) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.seed_bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("SSR MinZ Seed".to_string()))
    }

    pub fn reduce_at(
        &self,
        mip_transition: usize,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.reduce_bind_groups.get(mip_transition).ok_or_else(|| {
            AwsmBindGroupError::NotFound(format!(
                "SSR MinZ Reduce mip transition {}",
                mip_transition
            ))
        })
    }

    /// Rebuilds both the seed bind group and the per-mip-transition reduce
    /// bind groups against the current `SsrMinzTexture` + depth texture view.
    /// Called on viewport resize (which recreated both the depth and pyramid
    /// textures) and any time `BindGroups` triggers a `TextureViewRecreate`.
    pub fn recreate(
        &mut self,
        ctx: &BindGroupRecreateContext<'_>,
        minz: &SsrMinzTexture,
    ) -> Result<()> {
        // Seed.
        let entries = vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.depth)),
            ),
            BindGroupEntry::new(
                1,
                BindGroupResource::TextureView(Cow::Borrowed(&minz.views_per_mip[0])),
            ),
        ];
        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(self.seed_layout_key)?,
            Some("SSR MinZ Seed"),
            entries,
        );
        self.seed_bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));

        // Reduce — one per mip transition.
        self.reduce_bind_groups.clear();
        for n in 1..minz.mip_count as usize {
            let entries = vec![
                BindGroupEntry::new(
                    0,
                    BindGroupResource::TextureView(Cow::Borrowed(&minz.views_per_mip[n - 1])),
                ),
                BindGroupEntry::new(
                    1,
                    BindGroupResource::TextureView(Cow::Borrowed(&minz.views_per_mip[n])),
                ),
            ];
            let descriptor = BindGroupDescriptor::new(
                ctx.bind_group_layouts.get(self.reduce_layout_key)?,
                Some("SSR MinZ Reduce"),
                entries,
            );
            self.reduce_bind_groups
                .push(ctx.gpu.create_bind_group(&descriptor.into()));
        }
        Ok(())
    }
}

fn create_seed_layout(
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

fn create_reduce_layout(ctx: &mut RenderPassInitContext<'_>) -> Result<BindGroupLayoutKey> {
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
