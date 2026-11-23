//! TAA resolve bind groups (layout mirrors `taa_wgsl/resolve.wgsl`).
//!
//! - 0 camera uniform (`CameraRaw`: jitter, prev/inverse matrices)
//! - 1 `TaaParams` uniform
//! - 2 `composite`: lit HDR after SSR, render size (textureLoad)
//! - 3 scene depth, render size, single-sample (MSAA is off under TAA)
//! - 4 `velocity`, render size (textureLoad)
//! - 5 previous depth, last frame's render size (`texture_depth_2d`)
//! - 6 previous history, post size (FILTERABLE: the Catmull-Rom fetch)
//! - 7 linear clamp-to-edge sampler for that fetch
//! - 8 this frame's history (storage write)
//! - 9 `out`, linear HDR at post size (storage write)
//! - 10 reactive mask, render size: only in the `reactive` variant (phase 3)
//!
//! Two groups, one per history parity: group `i` writes `history[i]` and
//! reads `history[1 - i]`; the pass picks by its own history index, which
//! flips only after a submitted resolve (`ResolveBookkeeping`). Rebuilt via
//! the central ledger (`FunctionToCall::Taa`) when a bound view changes.

use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    BufferBindingLayout, BufferBindingType, SamplerBindingLayout, SamplerBindingType,
    StorageTextureAccess, StorageTextureBindingLayout, TextureBindingLayout,
};
use awsm_renderer_core::buffers::BufferBinding;
use awsm_renderer_core::sampler::{AddressMode, FilterMode, SamplerDescriptor};
use awsm_renderer_core::texture::{TextureSampleType, TextureViewDimension};

use crate::bind_group_layout::{
    BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey,
};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::render_passes::taa::texture::{TaaTextures, TAA_COLOR_FORMAT};
use crate::render_passes::RenderPassInitContext;

pub struct TaaBindGroups {
    pub layout_key: BindGroupLayoutKey,
    /// Whether the layout carries the reactive mask (binding 10).
    reactive: bool,
    /// Linear, clamp-to-edge: the Catmull-Rom history fetch is five bilinear
    /// taps, and its outer taps may land just past the edge.
    sampler: web_sys::GpuSampler,
    /// Indexed by the current history index (see the module doc).
    groups: [Option<web_sys::GpuBindGroup>; 2],
}

impl TaaBindGroups {
    pub fn new(ctx: &mut RenderPassInitContext<'_>, reactive: bool) -> Result<Self> {
        let layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, layout_cache_key(reactive))?;
        let sampler = ctx.gpu.create_sampler(Some(
            &SamplerDescriptor {
                label: Some("TAA History Sampler"),
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
            reactive,
            sampler,
            groups: [None, None],
        })
    }

    /// The group that writes `history[curr_index]`.
    pub fn group(
        &self,
        curr_index: usize,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.groups
            .get(curr_index)
            .and_then(Option::as_ref)
            .ok_or_else(|| AwsmBindGroupError::NotFound(format!("TAA Resolve {curr_index}")))
    }

    /// Rebuild both parity groups against the live render-texture views and
    /// the pass's own textures.
    pub fn recreate(
        &mut self,
        ctx: &BindGroupRecreateContext<'_>,
        textures: &TaaTextures,
        params_buffer: &web_sys::GpuBuffer,
    ) -> Result<()> {
        if self.reactive {
            // No reactive mask exists before plan phase 3; the pass only
            // builds the non-reactive variant until then. Phase 3: bind
            // `views.reactive` at binding 10 (the layout already has it).
            return Err(AwsmBindGroupError::NotFound("TAA reactive mask".to_string()).into());
        }
        let views = ctx.render_texture_views;
        // The geometry pass allocates `velocity` whenever TAA is on, so a
        // missing view is a lifecycle bug, not a state to paper over.
        let velocity = views
            .velocity
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("TAA velocity".to_string()))?;
        let layout = ctx.bind_group_layouts.get(self.layout_key)?;
        for (curr, group) in self.groups.iter_mut().enumerate() {
            let prev = 1 - curr;
            let entries = vec![
                BindGroupEntry::new(
                    0,
                    BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
                ),
                BindGroupEntry::new(
                    1,
                    BindGroupResource::Buffer(BufferBinding::new(params_buffer)),
                ),
                BindGroupEntry::new(
                    2,
                    BindGroupResource::TextureView(Cow::Borrowed(&views.composite)),
                ),
                BindGroupEntry::new(
                    3,
                    BindGroupResource::TextureView(Cow::Borrowed(&views.depth)),
                ),
                BindGroupEntry::new(4, BindGroupResource::TextureView(Cow::Borrowed(velocity))),
                BindGroupEntry::new(
                    5,
                    BindGroupResource::TextureView(Cow::Borrowed(textures.prev_depth_view())),
                ),
                BindGroupEntry::new(
                    6,
                    BindGroupResource::TextureView(Cow::Borrowed(&textures.history_views[prev])),
                ),
                BindGroupEntry::new(7, BindGroupResource::Sampler(&self.sampler)),
                BindGroupEntry::new(
                    8,
                    BindGroupResource::TextureView(Cow::Borrowed(&textures.history_views[curr])),
                ),
                BindGroupEntry::new(
                    9,
                    BindGroupResource::TextureView(Cow::Borrowed(&textures.out_view)),
                ),
            ];
            let descriptor = BindGroupDescriptor::new(layout, Some("TAA Resolve"), entries);
            *group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        }
        Ok(())
    }
}

fn layout_cache_key(reactive: bool) -> BindGroupLayoutCacheKey {
    let compute_only = |resource: BindGroupLayoutResource| BindGroupLayoutCacheKeyEntry {
        resource,
        visibility_vertex: false,
        visibility_fragment: false,
        visibility_compute: true,
    };
    let uniform = || {
        compute_only(BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
        ))
    };
    let load_only = || {
        compute_only(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::UnfilterableFloat)
                .with_multisampled(false),
        ))
    };
    let depth = || {
        compute_only(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::Depth)
                .with_multisampled(false),
        ))
    };
    let storage_write = || {
        compute_only(BindGroupLayoutResource::StorageTexture(
            StorageTextureBindingLayout::new(TAA_COLOR_FORMAT)
                .with_view_dimension(TextureViewDimension::N2d)
                .with_access(StorageTextureAccess::WriteOnly),
        ))
    };
    let mut entries = vec![
        // 0 camera, 1 params
        uniform(),
        uniform(),
        // 2 composite
        load_only(),
        // 3 depth
        depth(),
        // 4 velocity
        load_only(),
        // 5 previous depth
        depth(),
        // 6 previous history (filterable)
        compute_only(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::Float),
        )),
        // 7 history sampler
        compute_only(BindGroupLayoutResource::Sampler(
            SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Filtering),
        )),
        // 8 this frame's history, 9 out
        storage_write(),
        storage_write(),
    ];
    if reactive {
        // 10 reactive mask
        entries.push(load_only());
    }
    BindGroupLayoutCacheKey { entries }
}
