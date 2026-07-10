//! SSR pass bind group + layout.
//!
//! One compute bind group (M1 inputs):
//! - 0 camera uniform (`CameraRaw`) — view/proj/inv_proj for reconstruction
//! - 1 `SsrParams` live-tuning uniform
//! - 2 depth (non-filterable, `textureLoad` only)
//! - 3 `normal_tangent` (packed octahedral world normal)
//! - 4 HDR color source = the RESOLVED single-sample `composite` target (SSR
//!   runs post-resolve so the color source is never multisampled)
//! - 5 storage-write reflection target = the full-res `ssr` render texture
//! - 6 material-owned `reflection_descriptor` (M2a): RGB = reflectivity color
//!   (0 = opt out), A = spread. Always single-sample (written full-res by
//!   `material_opaque` at sample 0)
//!
//! Rebuilt on resize / texture-view recreate via [`SsrBindGroups::recreate`],
//! dispatched from `bind_groups.rs` (`FunctionToCall::Ssr`).

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
use crate::render_passes::ssr::shader::cache_key::SsrTrace;
use crate::render_passes::RenderPassInitContext;

pub struct SsrBindGroups {
    pub layout_key: BindGroupLayoutKey,
    /// M3: whether the temporal variant is compiled — decides layout shape
    /// (entries 7/8/9), whether the linear `sampler` exists, and whether one or
    /// two parity bind groups are built. Derived from `ssr.temporal` at `new()`.
    temporal: bool,
    /// M2c: whether the Hi-Z (min-Z pyramid) trace variant is compiled — adds
    /// the pyramid binding (entry 10) to the layout + each bind group. Derived
    /// from `SsrTrace::PRODUCTION` so it stays in lockstep with the compiled
    /// shader (`SsrPipelines::m1_key`). When true, `recreate` needs the min-Z
    /// pyramid's `view_all` (threaded on `BindGroupRecreateContext`); if that
    /// view is absent (SSR disabled → no pyramid pass), the trace bind group is
    /// left unbuilt — the trace only dispatches when SSR is enabled, which is
    /// exactly when the pyramid exists.
    hiz: bool,
    /// Linear, clamp-to-edge sampler for the reprojected history fetch (binding
    /// 8). `None` unless temporal — the non-temporal path binds no sampler
    /// (everything is integer `textureLoad`), so it allocates nothing new.
    sampler: Option<web_sys::GpuSampler>,
    /// Trace bind groups. `None` until the first `recreate`. Non-temporal uses
    /// only slot 0. Temporal builds BOTH parity groups (indexed by the current
    /// history index / `curr_index`): slot 0 = ping_pong (write history[0], read
    /// history[1]); slot 1 = the reverse. The render pass selects by
    /// `ping_pong()` (see [`Self::trace`]).
    trace_bind_groups: [Option<web_sys::GpuBindGroup>; 2],
}

impl SsrBindGroups {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        // Under MSAA the depth + normal G-buffer targets are multisampled.
        let multisampled = ctx.anti_aliasing.msaa_sample_count.is_some();
        let temporal = ctx.post_processing.ssr.temporal;
        // M2c: keyed off the shared production-trace constant, NOT a
        // post_processing field (trace mode isn't runtime-configurable) — so
        // the layout matches the shader `SsrPipelines::m1_key` compiles.
        let hiz = SsrTrace::PRODUCTION.is_hiz();
        let layout_key = create_layout(ctx, multisampled, temporal, hiz)?;
        // The temporal reprojected history fetch (binding 7) is a genuinely
        // filtered sample — unlike every other SSR input, which is integer
        // `textureLoad`. Create its linear sampler only for the temporal variant.
        let sampler = if temporal {
            Some(ctx.gpu.create_sampler(Some(
                &SamplerDescriptor {
                    label: Some("SSR History Linear Sampler"),
                    mag_filter: Some(FilterMode::Linear),
                    min_filter: Some(FilterMode::Linear),
                    address_mode_u: Some(AddressMode::ClampToEdge),
                    address_mode_v: Some(AddressMode::ClampToEdge),
                    address_mode_w: Some(AddressMode::ClampToEdge),
                    ..SamplerDescriptor::default()
                }
                .into(),
            )))
        } else {
            None
        };
        Ok(Self {
            layout_key,
            temporal,
            hiz,
            sampler,
            trace_bind_groups: [None, None],
        })
    }

    /// Selects the trace bind group for this frame. Non-temporal → the single
    /// slot-0 group. Temporal → the parity group whose write target is the
    /// current history index: `ping_pong` ⇒ `curr_index == 0` ⇒ slot 0, else
    /// slot 1 — matching `RenderTextureViews::{curr_index, prev_index}`.
    pub fn trace(
        &self,
        ping_pong: bool,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        let idx = if self.temporal && !ping_pong { 1 } else { 0 };
        self.trace_bind_groups[idx]
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("SSR Trace".to_string()))
    }

    pub fn recreate(
        &mut self,
        ctx: &BindGroupRecreateContext<'_>,
        params_buffer: &web_sys::GpuBuffer,
    ) -> Result<()> {
        let layout = ctx.bind_group_layouts.get(self.layout_key)?;
        // M2c Hi-Z: the min-Z pyramid's all-mips view (binding 10). Threaded on
        // the recreate context (a clone, like `hzb_full_view`). When the Hi-Z
        // variant is compiled but the pyramid pass is absent (SSR disabled →
        // no pyramid), skip building the trace bind group entirely: the layout
        // requires binding 10, and the trace never dispatches while SSR is off,
        // so an unbuilt group loses nothing.
        let minz_view = ctx.ssr_minz_full_view.as_ref();
        if self.hiz && minz_view.is_none() {
            self.trace_bind_groups = [None, None];
            return Ok(());
        }
        // Binding numbers are POSITIONAL (layout entry index == binding), so the
        // pyramid entry is appended AFTER the temporal 7/8/9 block in
        // `create_layout` to leave those untouched. That puts it at binding 7
        // when non-temporal, 10 when temporal — the WGSL declares the matching
        // number off its own `temporal` flag.
        let minz_binding: u32 = if self.temporal { 10 } else { 7 };
        // Bindings 0-6 (+ 10 under Hi-Z) are identical every frame + across
        // parity. Rebuilt fresh per call (Cow-borrowed views) since the entries
        // borrow `ctx`.
        let base_entries = || {
            let mut base = vec![
                BindGroupEntry::new(
                    0,
                    BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
                ),
                BindGroupEntry::new(1, BindGroupResource::Buffer(BufferBinding::new(params_buffer))),
                BindGroupEntry::new(
                    2,
                    BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.depth)),
                ),
                BindGroupEntry::new(
                    3,
                    BindGroupResource::TextureView(Cow::Borrowed(
                        &ctx.render_texture_views.normal_tangent,
                    )),
                ),
                BindGroupEntry::new(
                    4,
                    BindGroupResource::TextureView(Cow::Borrowed(
                        &ctx.render_texture_views.composite,
                    )),
                ),
                BindGroupEntry::new(
                    5,
                    BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.ssr)),
                ),
                BindGroupEntry::new(
                    6,
                    BindGroupResource::TextureView(Cow::Borrowed(
                        &ctx.render_texture_views.reflection_descriptor,
                    )),
                ),
            ];
            // min-Z pyramid (all mips), read via textureLoad(lod) during the
            // Hi-Z descent. Bound on EVERY variant (non-temporal + both temporal
            // parity groups) at `minz_binding` (7 non-temporal / 10 temporal) so
            // it composes with the temporal 7/8/9 bindings without collision.
            // `minz_view` is guaranteed `Some` here — the `hiz && none` case
            // returned early above.
            if self.hiz {
                if let Some(view) = minz_view {
                    base.push(BindGroupEntry::new(
                        minz_binding,
                        BindGroupResource::TextureView(Cow::Borrowed(view)),
                    ));
                }
            }
            base
        };

        if self.temporal {
            let sampler = self.sampler.as_ref().ok_or_else(|| {
                AwsmBindGroupError::NotFound("SSR History Sampler".to_string())
            })?;
            let history = &ctx.render_texture_views.ssr_history;
            // Build BOTH parity bind groups now — the history views swap by
            // frame parity every frame but `recreate` only runs on resize /
            // TextureViewRecreate, so a per-frame rebuild is out of the question.
            // `curr` is the write index; `prev` (the other) is the read.
            let mut groups: [Option<web_sys::GpuBindGroup>; 2] = [None, None];
            for curr in 0..2usize {
                let prev = 1 - curr;
                let mut entries = base_entries();
                // 7 — previous-frame history (filtered read).
                entries.push(BindGroupEntry::new(
                    7,
                    BindGroupResource::TextureView(Cow::Borrowed(&history[prev])),
                ));
                // 8 — linear sampler for the reprojected fetch.
                entries.push(BindGroupEntry::new(8, BindGroupResource::Sampler(sampler)));
                // 9 — this-frame history (storage write).
                entries.push(BindGroupEntry::new(
                    9,
                    BindGroupResource::TextureView(Cow::Borrowed(&history[curr])),
                ));
                let descriptor = BindGroupDescriptor::new(layout, Some("SSR Trace"), entries);
                groups[curr] = Some(ctx.gpu.create_bind_group(&descriptor.into()));
            }
            self.trace_bind_groups = groups;
        } else {
            let descriptor = BindGroupDescriptor::new(layout, Some("SSR Trace"), base_entries());
            self.trace_bind_groups =
                [Some(ctx.gpu.create_bind_group(&descriptor.into())), None];
        }
        Ok(())
    }
}

fn create_layout(
    ctx: &mut RenderPassInitContext<'_>,
    multisampled: bool,
    temporal: bool,
    hiz: bool,
) -> Result<BindGroupLayoutKey> {
    let compute_only = |resource: BindGroupLayoutResource| BindGroupLayoutCacheKeyEntry {
        resource,
        visibility_vertex: false,
        visibility_fragment: false,
        visibility_compute: true,
    };
    let mut entries = vec![
        // 0 — camera uniform
        compute_only(BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
        )),
        // 1 — SsrParams uniform
        compute_only(BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
        )),
        // 2 — depth (non-filterable, sampled as depth via textureLoad)
        compute_only(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::Depth)
                .with_multisampled(multisampled),
        )),
        // 3 — normal_tangent (float)
        compute_only(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::UnfilterableFloat)
                .with_multisampled(multisampled),
        )),
        // 4 — HDR color source (float)
        compute_only(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::UnfilterableFloat),
        )),
        // 5 — reflection target (storage write)
        compute_only(BindGroupLayoutResource::StorageTexture(
            StorageTextureBindingLayout::new(TextureFormat::Rgba16float)
                .with_view_dimension(TextureViewDimension::N2d)
                .with_access(StorageTextureAccess::WriteOnly),
        )),
        // 6 — material-owned reflection descriptor (M2a). Single-sample
        // (material_opaque writes it full-res at sample 0), read via textureLoad.
        compute_only(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::UnfilterableFloat)
                .with_multisampled(false),
        )),
    ];

    // M3 temporal reprojection adds a filtered history-read + its linear
    // sampler + a history-WRITE storage target. These exist ONLY on the
    // temporal variant → a distinct layout key, so the non-temporal layout is
    // byte-identical to today.
    if temporal {
        // 7 — previous-frame reflection history (filterable Float, sampled).
        entries.push(compute_only(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::Float),
        )));
        // 8 — linear filtering sampler for the reprojected history fetch.
        entries.push(compute_only(BindGroupLayoutResource::Sampler(
            SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Filtering),
        )));
        // 9 — this-frame reflection history (storage write).
        entries.push(compute_only(BindGroupLayoutResource::StorageTexture(
            StorageTextureBindingLayout::new(TextureFormat::Rgba16float)
                .with_view_dimension(TextureViewDimension::N2d)
                .with_access(StorageTextureAccess::WriteOnly),
        )));
    }

    // M2c Hi-Z: min-Z pyramid (single-sample, unfilterable float, read via
    // integer textureLoad at a chosen LOD). Appended LAST — after the optional
    // temporal 7/8/9 block — so binding numbers land at 7 (non-temporal) / 10
    // (temporal), leaving the existing bindings byte-identical. A distinct
    // layout key per (temporal, hiz) combination keeps the non-Hi-Z layout
    // unchanged.
    if hiz {
        entries.push(compute_only(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::UnfilterableFloat)
                .with_multisampled(false),
        )));
    }

    Ok(ctx
        .bind_group_layouts
        .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?)
}
