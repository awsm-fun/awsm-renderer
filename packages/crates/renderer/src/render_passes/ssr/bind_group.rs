//! SSR pass bind group + layout.
//!
//! One compute bind group (M1 inputs):
//! - 0 camera uniform (`CameraRaw`) — view/proj/inv_proj for reconstruction
//! - 1 `SsrParams` live-tuning uniform
//! - 2 depth (non-filterable, `textureLoad` only)
//! - 3 `normal_tangent` (packed octahedral world normal)
//! - 4 HDR color source = the RESOLVED single-sample `composite` target (SSR
//!   runs post-resolve so the color source is never multisampled)
//! - 5 storage-write reflection target = the `ssr` render texture (half-res
//!   by default via `resolution_scale`)
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
use crate::render_passes::RenderPassInitContext;

pub struct SsrBindGroups {
    pub layout_key: BindGroupLayoutKey,
    /// Spatial-resolve layout — the 9-tap edge-aware denoise between trace and
    /// composite: camera + raw trace output (sampled) + full-res depth +
    /// `ssr_resolved` storage write.
    pub resolve_layout_key: BindGroupLayoutKey,
    /// M3: whether the temporal variant is compiled — decides layout shape
    /// (entries 7/8/9), whether the linear `sampler` exists, and whether one or
    /// two parity bind groups are built. Derived from `ssr.temporal` at `new()`.
    temporal: bool,
    /// Hi-Z trace variant (gpu_culling capability) — the trace bind group
    /// carries the HZB pyramid as its last entry.
    hzb: bool,
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
    /// Spatial-resolve bind group. `None` until the first `recreate`. Parity-
    /// independent (it reads the `ssr` trace target, not the history pair), so
    /// a single group suffices for both temporal and non-temporal variants.
    resolve_bind_group: Option<web_sys::GpuBindGroup>,
}

impl SsrBindGroups {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        // Under MSAA the depth + normal G-buffer targets are multisampled.
        let multisampled = ctx.anti_aliasing.msaa_sample_count.is_some();
        let temporal = ctx.post_processing.ssr.temporal;
        let layout_key = create_layout(ctx, multisampled, temporal, ctx.features.gpu_culling)?;
        let resolve_layout_key = create_resolve_layout(ctx, multisampled)?;
        // The temporal reprojected history fetch (binding 7) is a genuinely
        // filtered sample — unlike every other SSR input, which is integer
        // `textureLoad`. Create its linear sampler only for the temporal variant.
        let sampler = if temporal {
            Some(
                ctx.gpu.create_sampler(Some(
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
                )),
            )
        } else {
            None
        };
        Ok(Self {
            layout_key,
            resolve_layout_key,
            hzb: ctx.features.gpu_culling,
            temporal,
            sampler,
            trace_bind_groups: [None, None],
            resolve_bind_group: None,
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

    /// The spatial-resolve bind group (parity-independent, single group).
    pub fn resolve(&self) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.resolve_bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("SSR Resolve".to_string()))
    }

    pub fn recreate(
        &mut self,
        ctx: &BindGroupRecreateContext<'_>,
        params_buffer: &web_sys::GpuBuffer,
    ) -> Result<()> {
        let layout = ctx.bind_group_layouts.get(self.layout_key)?;
        // Bindings 0-6 are identical every frame + across parity. Rebuilt fresh
        // per call (Cow-borrowed views) since the entries borrow `ctx`.
        let base_entries = || {
            vec![
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
            ]
        };

        if self.temporal {
            let sampler = self
                .sampler
                .as_ref()
                .ok_or_else(|| AwsmBindGroupError::NotFound("SSR History Sampler".to_string()))?;
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
                // 10 — HZB pyramid (Hi-Z trace variant only).
                if self.hzb {
                    let hzb_view = ctx
                        .hzb_full_view
                        .as_ref()
                        .expect("HZB view missing despite gpu_culling feature on");
                    entries.push(BindGroupEntry::new(
                        10,
                        BindGroupResource::TextureView(Cow::Borrowed(hzb_view)),
                    ));
                }
                let descriptor = BindGroupDescriptor::new(layout, Some("SSR Trace"), entries);
                groups[curr] = Some(ctx.gpu.create_bind_group(&descriptor.into()));
            }
            self.trace_bind_groups = groups;
        } else {
            let mut entries = base_entries();
            // 7 — HZB pyramid (Hi-Z trace variant only; binding index shifts
            // with `temporal` exactly like the layout + shader template).
            if self.hzb {
                let hzb_view = ctx
                    .hzb_full_view
                    .as_ref()
                    .expect("HZB view missing despite gpu_culling feature on");
                entries.push(BindGroupEntry::new(
                    7,
                    BindGroupResource::TextureView(Cow::Borrowed(hzb_view)),
                ));
            }
            let descriptor = BindGroupDescriptor::new(layout, Some("SSR Trace"), entries);
            self.trace_bind_groups = [Some(ctx.gpu.create_bind_group(&descriptor.into())), None];
        }

        // Spatial resolve: camera + raw trace output (sampled) + full-res
        // depth + `ssr_resolved` storage write. Rebuilt alongside the trace
        // groups on every recreate (same TextureViewRecreate lifecycle).
        let resolve_layout = ctx.bind_group_layouts.get(self.resolve_layout_key)?;
        let resolve_entries = vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
            ),
            BindGroupEntry::new(
                1,
                BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.ssr)),
            ),
            BindGroupEntry::new(
                2,
                BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.depth)),
            ),
            BindGroupEntry::new(
                3,
                BindGroupResource::TextureView(Cow::Borrowed(
                    &ctx.render_texture_views.ssr_resolved,
                )),
            ),
        ];
        let resolve_descriptor =
            BindGroupDescriptor::new(resolve_layout, Some("SSR Resolve"), resolve_entries);
        self.resolve_bind_group = Some(ctx.gpu.create_bind_group(&resolve_descriptor.into()));
        Ok(())
    }
}

fn create_layout(
    ctx: &mut RenderPassInitContext<'_>,
    multisampled: bool,
    temporal: bool,
    hzb: bool,
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

    // Hi-Z trace variant: the HZB pyramid (rg32float, .g = closest bound),
    // sampled across mips via textureLoad — appended LAST so binding indices
    // shift with `temporal` exactly as the shader template's nested-if does.
    if hzb {
        entries.push(compute_only(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::UnfilterableFloat),
        )));
    }

    Ok(ctx
        .bind_group_layouts
        .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?)
}

/// Layout for the spatial-resolve compute (the edge-aware denoise between
/// trace and composite). Every input is integer `textureLoad` — no sampler.
fn create_resolve_layout(
    ctx: &mut RenderPassInitContext<'_>,
    multisampled: bool,
) -> Result<BindGroupLayoutKey> {
    let compute_only = |resource: BindGroupLayoutResource| BindGroupLayoutCacheKeyEntry {
        resource,
        visibility_vertex: false,
        visibility_fragment: false,
        visibility_compute: true,
    };
    let entries = vec![
        // 0 — camera uniform (CameraRaw) for depth → view-Z linearization.
        compute_only(BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
        )),
        // 1 — raw SSR trace output (rgba16float, single-sample, textureLoad).
        compute_only(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::UnfilterableFloat)
                .with_multisampled(false),
        )),
        // 2 — full-res depth (non-filterable; multisampled under MSAA,
        // mirroring the trace's depth binding).
        compute_only(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::Depth)
                .with_multisampled(multisampled),
        )),
        // 3 — resolved reflection target (storage write).
        compute_only(BindGroupLayoutResource::StorageTexture(
            StorageTextureBindingLayout::new(TextureFormat::Rgba16float)
                .with_view_dimension(TextureViewDimension::N2d)
                .with_access(StorageTextureAccess::WriteOnly),
        )),
    ];
    Ok(ctx
        .bind_group_layouts
        .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?)
}
