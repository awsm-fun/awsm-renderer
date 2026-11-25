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
//! - then, numbered in this order and present only when used
//!   ([`TaaBindings`]): previous velocity (`TaaDisocclusion` motion test),
//!   the reactive mask (`reactive` variant, phase 3), this frame's
//!   visibility and last frame's (the object-id test).
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

/// The binding numbers of the optional resources (positional, after the ten
/// fixed ones): a resource the variant does not use has no binding at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaaBindings {
    pub prev_velocity: Option<u32>,
    pub reactive: Option<u32>,
    pub visibility: Option<u32>,
    pub prev_visibility: Option<u32>,
}

impl TaaBindings {
    pub fn new(reactive: bool, disocclusion: crate::taa::TaaDisocclusion) -> Self {
        let mut next = 10u32;
        let mut take = |on: bool| {
            on.then(|| {
                next += 1;
                next - 1
            })
        };
        let prev_velocity = take(disocclusion.motion());
        let reactive = take(reactive);
        let visibility = take(disocclusion.id());
        let prev_visibility = take(disocclusion.id());
        Self {
            prev_velocity,
            reactive,
            visibility,
            prev_visibility,
        }
    }
}

pub struct TaaBindGroups {
    pub layout_key: BindGroupLayoutKey,
    bindings: TaaBindings,
    /// Linear, clamp-to-edge: the Catmull-Rom history fetch is five bilinear
    /// taps, and its outer taps may land just past the edge.
    sampler: web_sys::GpuSampler,
    /// Indexed by the current history index (see the module doc).
    groups: [Option<web_sys::GpuBindGroup>; 2],
}

impl TaaBindGroups {
    pub fn new(
        ctx: &mut RenderPassInitContext<'_>,
        reactive: bool,
        disocclusion: crate::taa::TaaDisocclusion,
    ) -> Result<Self> {
        let bindings = TaaBindings::new(reactive, disocclusion);
        let layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, layout_cache_key(bindings))?;
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
            bindings,
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
        dof_pre: Option<&web_sys::GpuTextureView>,
    ) -> Result<()> {
        let views = ctx.render_texture_views;
        // The resolve's input: this frame's HDR render, or (DoF before the
        // resolve) the same image blurred, also render size.
        let color = dof_pre.unwrap_or(&views.composite);
        // Allocated with the velocity target whenever TAA is on.
        let reactive = if self.bindings.reactive.is_some() {
            Some(
                views
                    .reactive
                    .as_ref()
                    .ok_or_else(|| AwsmBindGroupError::NotFound("TAA reactive".to_string()))?,
            )
        } else {
            None
        };
        // The geometry pass allocates `velocity` whenever TAA is on, so a
        // missing view is a lifecycle bug, not a state to paper over.
        let velocity = views
            .velocity
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("TAA velocity".to_string()))?;
        let layout = ctx.bind_group_layouts.get(self.layout_key)?;
        for (curr, group) in self.groups.iter_mut().enumerate() {
            let prev = 1 - curr;
            let mut entries = vec![
                BindGroupEntry::new(
                    0,
                    BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
                ),
                BindGroupEntry::new(
                    1,
                    BindGroupResource::Buffer(BufferBinding::new(params_buffer)),
                ),
                BindGroupEntry::new(2, BindGroupResource::TextureView(Cow::Borrowed(color))),
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
            let missing = |what: &str| AwsmBindGroupError::NotFound(format!("TAA {what}"));
            if let Some(b) = self.bindings.prev_velocity {
                let view = textures
                    .prev_velocity_view()
                    .ok_or_else(|| missing("previous velocity"))?;
                entries.push(BindGroupEntry::new(
                    b,
                    BindGroupResource::TextureView(Cow::Borrowed(view)),
                ));
            }
            if let (Some(b), Some(reactive)) = (self.bindings.reactive, reactive) {
                entries.push(BindGroupEntry::new(
                    b,
                    BindGroupResource::TextureView(Cow::Borrowed(reactive)),
                ));
            }
            if let Some(b) = self.bindings.visibility {
                entries.push(BindGroupEntry::new(
                    b,
                    BindGroupResource::TextureView(Cow::Borrowed(&views.visibility_data)),
                ));
            }
            if let Some(b) = self.bindings.prev_visibility {
                let view = textures
                    .prev_visibility_view()
                    .ok_or_else(|| missing("previous visibility"))?;
                entries.push(BindGroupEntry::new(
                    b,
                    BindGroupResource::TextureView(Cow::Borrowed(view)),
                ));
            }
            let descriptor = BindGroupDescriptor::new(layout, Some("TAA Resolve"), entries);
            *group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        }
        Ok(())
    }
}

fn layout_cache_key(bindings: TaaBindings) -> BindGroupLayoutCacheKey {
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
    let uint = || {
        compute_only(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::Uint)
                .with_multisampled(false),
        ))
    };
    // The optional ones, in `TaaBindings` order (positional).
    if bindings.prev_velocity.is_some() {
        entries.push(load_only());
    }
    if bindings.reactive.is_some() {
        entries.push(load_only());
    }
    if bindings.visibility.is_some() {
        entries.push(uint());
    }
    if bindings.prev_visibility.is_some() {
        entries.push(uint());
    }
    BindGroupLayoutCacheKey { entries }
}
