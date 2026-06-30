//! `Shadows` — the per-renderer shadow subsystem.
//!
//! Owns every GPU resource for shadow generation and sampling, plus
//! the per-frame `write_gpu` that fits cascades / packs descriptors /
//! reconciles temporal throttle state. The free helpers it calls live
//! in `helpers.rs`; the public `AwsmRenderer` setter API lives in
//! `api.rs`.

use awsm_renderer_core::{
    bind_groups::{
        BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
        BufferBindingLayout, BufferBindingType,
    },
    buffers::{BufferBinding, BufferDescriptor, BufferUsage},
    compare::CompareFunction,
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
    sampler::{FilterMode, SamplerDescriptor},
    texture::{Extent3d, TextureDescriptor, TextureFormat, TextureUsage},
};
use glam::Mat4;
use slotmap::SecondaryMap;

use crate::{
    bind_group_layout::{
        BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey, BindGroupLayouts,
    },
    bind_groups::BindGroups,
    debug::AwsmRendererLogging,
    lights::LightKey,
    pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey, PipelineLayouts},
    pipelines::{
        compute_pipeline::ComputePipelineKey,
        render_pipeline::{RenderPipelineCacheKey, RenderPipelineKey},
        Pipelines,
    },
    render_passes::geometry::bind_group::GeometryBindGroups,
    render_textures::RenderTextureFormats,
    shaders::{ShaderCacheKey, Shaders},
    shadows::{
        cascade,
        config::ShadowsConfig,
        consts::{
            clamp_point_shadow_resolution, MAX_SHADOW_DESCRIPTORS, MAX_SHADOW_VIEWS,
            POINT_SHADOW_NEAR, SHADOW_ATLAS_MAX_SIZE, SHADOW_DESCRIPTOR_BYTES,
            SHADOW_GLOBALS_BYTES, SHADOW_INDEX_NONE, SHADOW_VIEW_BYTES, SHADOW_VIEW_STRIDE,
        },
        error::AwsmShadowError,
        evsm,
        evsm::{EvsmDescriptors, EvsmPass},
        helpers::{
            build_cascade_layer_views, build_cube_face_views, build_evsm_blur_bind_group,
            build_evsm_moment_write_bind_group, create_cascade_array_view,
            create_cube_2d_array_view, create_cube_array_view, extract_near_far,
            shadow_pipeline_cache_key, view_projection_drift,
            write_shadow_cascade_array_descriptor, write_shadow_descriptor, write_shadow_view_slot,
            SHADOW_DESCRIPTOR_UNIFORM_BYTES,
        },
        light_shadow::{EvsmCutoff, LightShadowParams},
        record::{
            EvsmDispatchEntry, LightShadowRecord, LightShadowView, ShadowAlloc, ShadowViewThrottle,
        },
    },
};

/// Owns every GPU resource for shadow generation and sampling.
pub struct Shadows {
    /// Renderer-wide configuration. Replace via [`Shadows::set_config`].
    pub config: ShadowsConfig,
    /// Depth atlas used for PCF and PCSS sampling.
    pub atlas_texture: web_sys::GpuTexture,
    /// Default view of the atlas.
    pub atlas_view: web_sys::GpuTextureView,
    /// Atlas resolution in texels (square).
    pub atlas_size: u32,
    /// EVSM atlas (`RGBA16F`) — moments storage for far directional
    /// cascades. Sized at `config.evsm_atlas_size`. Usage includes
    /// `STORAGE_BINDING` for the blur compute passes plus
    /// `RENDER_ATTACHMENT` for the moment-writer fragment pass.
    pub evsm_atlas_texture: web_sys::GpuTexture,
    /// Default sample-side view of the EVSM atlas. Bound at shadow
    /// group slot 4 of every receiver pipeline.
    pub evsm_atlas_view: web_sys::GpuTextureView,
    /// Active per-side dimension of the EVSM atlas in texels.
    pub evsm_atlas_size: u32,
    /// Ping-pong texture for the separable Gaussian blur. Same size as
    /// `evsm_atlas_texture`; never sampled at receiver time.
    pub evsm_blur_pingpong_texture: web_sys::GpuTexture,
    /// Default view of the ping-pong texture.
    pub evsm_blur_pingpong_view: web_sys::GpuTextureView,
    /// EVSM compute pipelines + per-cascade params buffer.
    pub evsm_pass: EvsmPass,
    /// Per-frame EVSM cascade list — `(descriptor_index, pcf_rect,
    /// evsm_rect)` for the dispatch loop. `pcf_rect` is in shadow_atlas
    /// texels; `evsm_rect` is in evsm_atlas texels.
    pub evsm_dispatch_queue: Vec<EvsmDispatchEntry>,
    /// Persistent bind group for the moment-write compute pass.
    /// Bindings: 0=shadow_atlas (depth), 1=evsm_atlas (storage write),
    /// 2=params (uniform, dynamic offset). Same group is used for
    /// every EVSM cascade; per-cascade context comes via dynamic
    /// offset.
    pub evsm_moment_write_bind_group: web_sys::GpuBindGroup,
    /// Persistent bind group for the horizontal blur half-pass.
    /// 0=evsm_atlas (read), 1=ping-pong (storage write), 2=params.
    pub evsm_blur_h_bind_group: web_sys::GpuBindGroup,
    /// Persistent bind group for the vertical blur half-pass.
    /// 0=ping-pong (read), 1=evsm_atlas (storage write), 2=params.
    pub evsm_blur_v_bind_group: web_sys::GpuBindGroup,
    /// 2D-array depth texture, one layer per directional-cascade view.
    /// Spot lights stay on `atlas_texture`; cascades migrated here so
    /// each cascade gets its own per-layer render attachment — a
    /// throttled cascade can skip its depth pass without touching
    /// other cascades' contents (which used to be impossible because
    /// `LoadOp::Clear` on the shared 2D atlas was attachment-wide).
    pub cascade_array_texture: web_sys::GpuTexture,
    /// Sampling-side `texture_depth_2d_array` view spanning every
    /// layer — bound at the shadow group slot the receiver shader
    /// reads via `textureSampleCompareLevel`.
    pub cascade_array_view: web_sys::GpuTextureView,
    /// One 2D depth view per cascade layer for use as a render
    /// attachment. Indexed by the cascade layer index.
    pub cascade_layer_views: Vec<web_sys::GpuTextureView>,
    /// Per-side dimension of every cascade-array layer in texels.
    /// Mirrors `config.cascade_resolution`.
    pub cascade_resolution: u32,
    /// Max number of simultaneous directional cascades. Mirrors
    /// `config.cascade_array_max_layers`.
    pub cascade_max_layers: u32,
    /// Cubemap array used for point-light shadows.
    pub cube_array_texture: web_sys::GpuTexture,
    /// 2D-array view of `cube_array_texture` used by cube PCSS for
    /// raw depth reads (`textureLoad`). Cube samplers don't expose
    /// `textureLoad`, but the same texture viewed as a 2D-array
    /// (layer = `slot * 6 + face`) does. Bound at slot 9 of the
    /// shadow group.
    pub cube_2d_array_view: web_sys::GpuTextureView,
    /// Cube-array view spanning every slice — used as the
    /// `texture_depth_cube_array` binding in the material-opaque
    /// shading pass.
    pub cube_array_view: web_sys::GpuTextureView,
    /// One 2D-array depth view per cube face (6 per slot). Indexed
    /// as `slot * 6 + face`. Used as the render attachment when
    /// generating each face's shadow map.
    pub cube_face_views: Vec<web_sys::GpuTextureView>,
    /// Active per-face cube shadow resolution in texels (square).
    /// Mirrors `config.point_shadow_resolution` clamped via
    /// `clamp_point_shadow_resolution` (≥ `MIN_POINT_SHADOW_RESOLUTION`,
    /// ≤ `SHADOW_ATLAS_MAX_SIZE`). Power-of-two isn't enforced — WebGPU
    /// is fine with arbitrary sizes — but non-POT values waste a bit of
    /// memory on the depth-texture tail. Read in `write_gpu` as the
    /// cube viewport.
    pub cube_resolution: u32,
    /// Per-slot owner. `None` means the slot is free; `Some(key)`
    /// means it currently holds the shadow for that point light.
    pub cube_slots: Vec<Option<LightKey>>,
    /// Storage buffer of per-shadow descriptors. Kept for forward
    /// compatibility with the plan's storage-buffer layout; the
    /// material-opaque bind group reads from `descriptors_uniform`
    /// instead so we stay under the storage-buffer-per-stage limit.
    pub descriptors_buffer: web_sys::GpuBuffer,
    /// Uniform buffer of per-shadow descriptors read by the shading
    /// passes. Fixed size: `MAX_SHADOW_DESCRIPTORS` entries.
    pub descriptors_uniform: web_sys::GpuBuffer,
    /// Uniform buffer of shadow globals (atlas sizes, EVSM params,
    /// SSCS flags) read by the shading passes.
    pub globals_buffer: web_sys::GpuBuffer,
    /// Per-pass uniform buffer of the current shadow view's matrix +
    /// bias parameters. Rewritten before each render pass.
    pub shadow_view_buffer: web_sys::GpuBuffer,
    /// Comparison sampler for `textureSampleCompare` on the atlases.
    pub sampler_comparison: web_sys::GpuSampler,
    /// Linear filterable sampler for EVSM moment sampling.
    pub sampler_filterable: web_sys::GpuSampler,

    /// Per-light authored shadow parameters. `pub(super)` so the
    /// `AwsmRenderer` setter API in the sibling `api.rs` can insert /
    /// look up entries without going through a forwarder; nothing
    /// outside `shadows::*` should ever touch this directly.
    pub(super) params: SecondaryMap<LightKey, LightShadowParams>,
    /// O(1) cache of `cube_slots[idx] == Some(light_key)`. Each point
    /// light's slot is stable across frames (re-assigned only on
    /// cube-pool resize or first acquisition), so caching the index
    /// avoids the two linear walks the previous code did each frame.
    /// Validated against `cube_slots[idx]` on lookup — a stale entry
    /// (slot reassigned to a different light, or pool recreated) falls
    /// back to the linear search.
    cube_slot_for_light: SecondaryMap<LightKey, u32>,
    /// Per-light, per-frame fitted record (cascade fit, atlas rect,
    /// descriptor index). Rebuilt every `write_gpu` call.
    records: SecondaryMap<LightKey, LightShadowRecord>,
    /// Throttle state per view, persisted across the `records`
    /// rebuild. Indexed by light key; each entry is a `Vec` parallel
    /// to `LightShadowRecord::views`.
    throttle: SecondaryMap<LightKey, Vec<ShadowViewThrottle>>,
    /// Number of descriptors currently active in `descriptors_uniform`.
    active_descriptor_count: u32,
    /// Number of view slots used in `shadow_view_buffer` this frame.
    /// One per render pass (per cascade / spot / cube face).
    active_view_count: u32,

    /// Bind-group layout for slot 0 of the shadow generation pipeline
    /// — a single `ShadowView` uniform. Held for diagnostic /
    /// recreation use; the bind group itself is created eagerly in
    /// `new`.
    #[allow(dead_code)]
    shadow_view_bind_group_layout_key: BindGroupLayoutKey,
    /// Cached shadow_view bind group.
    shadow_view_bind_group: web_sys::GpuBindGroup,
    /// Shadow generation pipeline layouts — `[shadow_view,
    /// transforms, meta, animation]`. Forked by `@group(2)` meta
    /// binding shape: `*_storage` for the non-instanced shadow
    /// pipelines (storage-array meta indexed by `instance_index`),
    /// `*_uniform` for instanced shadow pipelines (uniform binding
    /// with a per-draw dynamic offset). Held for parity with other
    /// passes; the pipelines themselves are built once in `new`.
    #[allow(dead_code)]
    shadow_pipeline_layout_key_storage: PipelineLayoutKey,
    #[allow(dead_code)]
    shadow_pipeline_layout_key_uniform: PipelineLayoutKey,
    /// Depth-only shadow pipeline (non-instancing). `None` until
    /// [`Shadows::ensure_pipelines_compiled`] runs (Block B.2) —
    /// deferred until the first shadow-casting light is added.
    shadow_pipeline_no_instancing: Option<RenderPipelineKey>,
    /// Depth-only shadow pipeline (instancing). See
    /// `shadow_pipeline_no_instancing` for deferred-compile semantics.
    shadow_pipeline_instancing: Option<RenderPipelineKey>,
    /// Depth-only shadow pipeline used for cube-face passes
    /// (non-instancing). Identical to the 2D variant except `front_face`
    /// is `Cw` to compensate for the Y-flip applied to the cube face
    /// projection — without that, front-face culling would invert and
    /// produce peter-panning on every point-light receiver. Deferred —
    /// see `shadow_pipeline_no_instancing`.
    shadow_pipeline_cube_no_instancing: Option<RenderPipelineKey>,
    /// Depth-only shadow pipeline used for cube-face passes (instancing).
    /// Deferred — see `shadow_pipeline_no_instancing`.
    shadow_pipeline_cube_instancing: Option<RenderPipelineKey>,
    /// Double-sided (`CullMode::None`) counterparts of the four pipelines
    /// above, selected for casters whose material is double-sided (thin /
    /// open geometry that has no back face for Front culling to keep). Same
    /// deferred-compile lifecycle. Order mirrors the single-sided four.
    shadow_pipeline_no_instancing_ds: Option<RenderPipelineKey>,
    shadow_pipeline_instancing_ds: Option<RenderPipelineKey>,
    shadow_pipeline_cube_no_instancing_ds: Option<RenderPipelineKey>,
    shadow_pipeline_cube_instancing_ds: Option<RenderPipelineKey>,
    /// Cached caster pipeline cache keys (8 entries: the four
    /// `(no_inst|inst, planar|cube)` single-sided variants followed by the
    /// same four double-sided (no-cull) variants). Held only while pipelines
    /// remain uncompiled — `ensure_pipelines_compiled` consumes them and
    /// clears the field once the resolved keys are installed. Empty Vec means
    /// "already compiled".
    pending_caster_cache_keys: Vec<RenderPipelineCacheKey>,
    /// Cached EVSM compute pipeline cache keys (3 entries: moment_write,
    /// blur_h, blur_v). Same lifecycle as `pending_caster_cache_keys`.
    pending_evsm_cache_keys: Vec<crate::pipelines::compute_pipeline::ComputePipelineCacheKey>,

    /// Frame counter used by temporal throttling (phase 11).
    pub frame_count: u64,
    /// §B static-shadow cache gate — the "casters static this frame" inputs the
    /// periodic-throttle suppression reads. `caster_moved_this_frame` is
    /// OR-accumulated across the per-frame `AwsmRenderer::update_transforms` calls
    /// (set when a *shadow-caster* mesh — `cast_shadows && !hud && !hidden` — moved)
    /// and read+reset by [`Self::take_shadow_static`]. `caster_set_revision` is
    /// bumped whenever a mesh's shadow flags toggle (`set_mesh_shadow_flags`);
    /// paired with the renderer's mesh count it forms the caster-set signature that
    /// catches add / remove / cast-toggle (any of which can stale a cached view
    /// whose view-projection didn't drift). Conservative: any uncertainty → not
    /// static → re-render.
    caster_moved_this_frame: bool,
    caster_set_revision: u64,
    caster_set_last_signature: Option<(usize, u64)>,
    /// Whether descriptors / globals need to be re-uploaded.
    pub dirty: bool,
    /// Set when a write_gpu pass detected atlas overflow. The next
    /// frame's write_gpu grows the atlas (and rebinds the opaque
    /// shadow bind group via `BindGroupCreate::ShadowsResourcesChange`).
    pending_atlas_grow: bool,
    /// Set when the cascade-placement loop overflowed the EVSM atlas
    /// (e.g. a SECOND shadowed directional light: the default atlas is
    /// exactly one default-resolution tile). The next write_gpu doubles
    /// `config.evsm_atlas_size` (capped at `SHADOW_ATLAS_MAX_SIZE`)
    /// through the same recreate path `set_config` uses — without this,
    /// every additional sun degraded to PCF with a per-frame warn.
    pending_evsm_grow: bool,
    /// One-shot latch so EVSM-overflow / layer-exhaustion warns fire
    /// once per episode instead of every frame (60 Hz log spam reaches
    /// the editor's console ring + tracing mirror).
    warned_evsm_overflow: bool,
    /// Set by `set_config` when a resource-shape config field changed.
    /// Processed at the top of the next `write_gpu` so users get a
    /// live update from the editor without having to reload the
    /// project.
    pending_resource_recreate: PendingResourceRecreate,
    /// Scratch buffer reused across `write_gpu` calls to avoid a per-
    /// frame heap allocation for the per-mesh caster AABB list. Capacity
    /// grows monotonically to the largest scene seen so far; `clear()`
    /// preserves capacity.
    caster_aabbs_scratch: Vec<crate::bounds::Aabb>,
    /// Scratch staging buffer for the per-frame descriptor pack
    /// before upload to `descriptors_uniform`. Sized to
    /// `SHADOW_DESCRIPTOR_UNIFORM_BYTES` once at construction;
    /// `fill(0)` between frames reuses the allocation.
    descriptor_bytes_scratch: Vec<u8>,
    /// Scratch staging buffer for per-view matrices uploaded into
    /// `shadow_view_buffer`. Sized to `SHADOW_VIEW_STRIDE *
    /// MAX_SHADOW_VIEWS` once at construction.
    view_bytes_scratch: Vec<u8>,
    /// Mapped-staging-ring uploaders (Phase 2.1).
    globals_uploader: crate::buffer::mapped_uploader::MappedUploader,
    descriptors_uploader: crate::buffer::mapped_uploader::MappedUploader,
    views_uploader: crate::buffer::mapped_uploader::MappedUploader,
}

/// Tracks which GPU resources need to be torn down + rebuilt because a
/// resource-shape config field changed since the last `write_gpu`. All
/// three resources are independent: a pure EVSM-atlas size bump
/// doesn't need to touch the PCF atlas or cube pool, and vice versa.
#[derive(Default, Copy, Clone)]
struct PendingResourceRecreate {
    /// `config.atlas_size` differs from `self.atlas_size`. Recreates
    /// the depth atlas texture + view and the moment-write bind group
    /// (which reads from the atlas).
    pcf_atlas: bool,
    /// `config.evsm_atlas_size` differs from `self.evsm_atlas_size`.
    /// Recreates the EVSM atlas + ping-pong textures and all three
    /// EVSM compute bind groups.
    evsm_atlas: bool,
    /// `config.max_point_shadows` or `config.point_shadow_resolution`
    /// changed. Recreates the cube-array texture, its views, and
    /// clears all slot owners so they get re-allocated next frame.
    cube_pool: bool,
    /// `config.cascade_resolution` or `config.cascade_array_max_layers`
    /// changed. Recreates the cascade-array texture, its 2D-array
    /// sampling view, and the per-layer render-attachment views; also
    /// rebuilds the EVSM moment-write bind group (it samples this
    /// texture for cascade-source depth).
    cascade_array: bool,
}

impl PendingResourceRecreate {
    fn any(&self) -> bool {
        self.pcf_atlas || self.evsm_atlas || self.cube_pool || self.cascade_array
    }
}

/// Pre-resolved layouts, GPU resource handles, and pipeline cache
/// keys for the shadow subsystem. Returned by
/// [`Shadows::build_descriptors`] and consumed by
/// [`Shadows::from_resolved`]. The 8 caster render-pipeline cache keys
/// fold into the cross-tail `RenderPipelines::ensure_keys` batch; the
/// EVSM block's 3 inline shaders + 3 compute pipelines fold into the
/// cross-tail compute pool. See [`EvsmDescriptors`] for the EVSM hand-
/// off shape.
pub struct ShadowsDescriptors {
    pub config: ShadowsConfig,
    // ── GPU resources ────────────────────────────────────────────────
    pub atlas_texture: web_sys::GpuTexture,
    pub atlas_view: web_sys::GpuTextureView,
    pub atlas_size: u32,
    pub evsm_atlas_texture: web_sys::GpuTexture,
    pub evsm_atlas_view: web_sys::GpuTextureView,
    pub evsm_atlas_size: u32,
    pub evsm_blur_pingpong_texture: web_sys::GpuTexture,
    pub evsm_blur_pingpong_view: web_sys::GpuTextureView,
    pub cascade_array_texture: web_sys::GpuTexture,
    pub cascade_array_view: web_sys::GpuTextureView,
    pub cascade_layer_views: Vec<web_sys::GpuTextureView>,
    pub cascade_resolution: u32,
    pub cascade_max_layers: u32,
    pub cube_array_texture: web_sys::GpuTexture,
    pub cube_array_view: web_sys::GpuTextureView,
    pub cube_2d_array_view: web_sys::GpuTextureView,
    pub cube_face_views: Vec<web_sys::GpuTextureView>,
    pub cube_resolution: u32,
    pub cube_slot_count: u32,
    pub descriptors_buffer: web_sys::GpuBuffer,
    pub descriptors_uniform: web_sys::GpuBuffer,
    pub globals_buffer: web_sys::GpuBuffer,
    pub shadow_view_buffer: web_sys::GpuBuffer,
    pub sampler_comparison: web_sys::GpuSampler,
    pub sampler_filterable: web_sys::GpuSampler,
    // ── caster layouts + bind group + cache keys ─────────────────────
    pub shadow_view_bind_group_layout_key: BindGroupLayoutKey,
    pub shadow_view_bind_group: web_sys::GpuBindGroup,
    pub shadow_pipeline_layout_key_storage: PipelineLayoutKey,
    pub shadow_pipeline_layout_key_uniform: PipelineLayoutKey,
    /// 8 caster pipeline cache keys: the four single-sided
    /// `(no_inst, planar)`, `(inst, planar)`, `(no_inst, cube)`, `(inst, cube)`
    /// followed by the same four as double-sided (no-cull) variants.
    pub caster_pipeline_cache_keys: Vec<RenderPipelineCacheKey>,
    // ── EVSM ─────────────────────────────────────────────────────────
    pub evsm: EvsmDescriptors,
}

impl ShadowsDescriptors {
    /// Two shader cache keys the caster pipelines depend on. Both
    /// are pre-warmed by `RenderPasses::new`'s cross-pass shader
    /// `ensure_keys` (the shadow caster shaders are added to that
    /// batch alongside the picker + line shader keys, see
    /// `render_passes.rs`'s phase-2 block). The orchestrator does
    /// NOT re-include them in the cross-tail shader batch — by the
    /// time tail descriptors run, the caster shaders are cache hits.
    /// This helper exists only as the standalone-`Shadows::new` path's
    /// own pre-warm and as a discoverable list for callers that want
    /// to know which shaders Shadows depends on.
    pub fn caster_shader_cache_keys() -> Vec<ShaderCacheKey> {
        vec![
            ShaderCacheKey::from(crate::shadows::shader::cache_key::ShaderCacheKeyShadow {
                instancing_transforms: false,
            }),
            ShaderCacheKey::from(crate::shadows::shader::cache_key::ShaderCacheKeyShadow {
                instancing_transforms: true,
            }),
        ]
    }
}

impl Shadows {
    /// Creates the shadow subsystem.
    ///
    /// Must be called after the geometry render pass has been built so
    /// the shadow pipeline can reuse the geometry pass's transform /
    /// meta / animation bind group layouts at slots 1..=3.
    ///
    /// Thin wrapper over [`Self::build_descriptors`] +
    /// [`Self::from_resolved`]. The cross-tail pooled startup path
    /// bypasses this and calls the two halves directly so the caster
    /// render pipelines + EVSM compute pipelines share one batched
    /// `RenderPipelines::ensure_keys` / `ComputePipelines::ensure_keys`
    /// with every other tail subsystem.
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        pipelines: &mut Pipelines,
        shaders: &mut Shaders,
        geometry_bind_groups: &GeometryBindGroups,
        render_texture_formats: &RenderTextureFormats,
        config: ShadowsConfig,
    ) -> Result<Self, AwsmShadowError> {
        let descs = Self::build_descriptors(
            gpu,
            bind_group_layouts,
            pipeline_layouts,
            shaders,
            geometry_bind_groups,
            render_texture_formats,
            config,
        )
        .await?;

        // Caster shader pre-warm (pooled in the orchestrator path, but
        // we still need to ensure here for the standalone case).
        shaders
            .ensure_keys(gpu, ShadowsDescriptors::caster_shader_cache_keys())
            .await?;

        // Caster render pipelines.
        let caster_resolved = pipelines
            .render
            .ensure_keys(
                gpu,
                shaders,
                pipeline_layouts,
                descs.caster_pipeline_cache_keys.clone(),
            )
            .await?;

        // EVSM: validate 3 inline shaders + ensure_keys 3 compute pipelines.
        let validation_results =
            futures::future::join_all(descs.evsm.validate_shader_futures()).await;
        for result in validation_results {
            result.map_err(AwsmShadowError::Core)?;
        }
        let evsm_shader_keys = [
            shaders.insert_uncached(descs.evsm.modules[0].clone()),
            shaders.insert_uncached(descs.evsm.modules[1].clone()),
            shaders.insert_uncached(descs.evsm.modules[2].clone()),
        ];
        let evsm_pipeline_cache_keys = descs.evsm.pipeline_cache_keys(evsm_shader_keys);
        let evsm_resolved = pipelines
            .compute
            .ensure_keys(gpu, shaders, pipeline_layouts, evsm_pipeline_cache_keys)
            .await?;

        Self::from_resolved(
            gpu,
            bind_group_layouts,
            descs,
            caster_resolved,
            evsm_resolved,
            Vec::new(),
            Vec::new(),
        )
    }

    /// Sync apart from a few `shaders.get_key` cache-hit lookups for
    /// the caster pipeline cache keys. Allocates every GPU resource
    /// the shadow subsystem owns, registers every bind-group + pipeline
    /// layout, and issues the 3 EVSM inline `compile_shader` calls.
    /// Returns [`ShadowsDescriptors`] for the orchestrator to fold into
    /// the cross-tail batches.
    #[allow(clippy::too_many_arguments)]
    pub async fn build_descriptors(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        shaders: &mut Shaders,
        geometry_bind_groups: &GeometryBindGroups,
        _render_texture_formats: &RenderTextureFormats,
        config: ShadowsConfig,
    ) -> Result<ShadowsDescriptors, AwsmShadowError> {
        warn_view_budget(&config);
        let atlas_size = config.atlas_size.max(1);
        let atlas_texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::Depth32float,
                Extent3d::new(atlas_size, Some(atlas_size), Some(1)),
                TextureUsage::new()
                    .with_render_attachment()
                    .with_texture_binding(),
            )
            .with_label("Shadow Atlas")
            .into(),
        )?;
        let atlas_view = atlas_texture
            .create_view()
            .map_err(AwsmCoreError::create_texture_view)?;

        // EVSM atlas — RGBA16F holds the four exponential moments
        // (pos_exp, pos_exp², neg_exp, neg_exp²) packed in `.rgba`.
        // Receivers do a single bilinear fetch + Chebyshev visibility
        // reconstruction instead of N comparison taps; the trade-off
        // is moment storage + a moment-write pass per EVSM cascade.
        //
        // Sized from `config.evsm_atlas_size` (default 2048², ~32 MB).
        // Usage = `RENDER_ATTACHMENT | TEXTURE_BINDING | STORAGE_BINDING`
        // so the moment-writer can render into it (fragment pipeline)
        // and the Gaussian-blur compute passes can read / write through
        // a storage view (post-blur it's bound back to the shadow group
        // for sampling).
        let evsm_atlas_size = config.evsm_atlas_size.max(1);
        let evsm_atlas_texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::Rgba16float,
                Extent3d::new(evsm_atlas_size, Some(evsm_atlas_size), Some(1)),
                TextureUsage::new()
                    .with_render_attachment()
                    .with_texture_binding()
                    .with_storage_binding(),
            )
            .with_label("Shadow EVSM Atlas")
            .into(),
        )?;
        // Same-size ping-pong texture for the separable Gaussian blur.
        // The horizontal blur reads from `evsm_atlas_texture`, writes
        // into this; the vertical blur reads back and writes into
        // `evsm_atlas_texture`. Storage-only — never sampled directly.
        let evsm_blur_pingpong_texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::Rgba16float,
                Extent3d::new(evsm_atlas_size, Some(evsm_atlas_size), Some(1)),
                TextureUsage::new()
                    .with_texture_binding()
                    .with_storage_binding(),
            )
            .with_label("Shadow EVSM Blur Ping-pong")
            .into(),
        )?;
        let evsm_blur_pingpong_view = evsm_blur_pingpong_texture
            .create_view()
            .map_err(AwsmCoreError::create_texture_view)?;
        let evsm_atlas_view = evsm_atlas_texture
            .create_view()
            .map_err(AwsmCoreError::create_texture_view)?;

        // Directional-cascade depth lives in its own 2D-array texture
        // (one layer per cascade) so each cascade has an independent
        // render attachment. The packed 2D atlas's attachment-wide
        // clear made throttling 2D cascades impossible — per-layer
        // attachments fix that by leaving non-throttled layers'
        // contents untouched across the frame.
        let cascade_resolution = config.cascade_resolution.max(16);
        let cascade_max_layers = config.cascade_array_max_layers.max(1);
        let cascade_array_texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::Depth32float,
                Extent3d::new(
                    cascade_resolution,
                    Some(cascade_resolution),
                    Some(cascade_max_layers),
                ),
                TextureUsage::new()
                    .with_render_attachment()
                    .with_texture_binding(),
            )
            .with_label("Shadow Cascade Array")
            .into(),
        )?;
        let cascade_array_view = create_cascade_array_view(&cascade_array_texture)?;
        let cascade_layer_views =
            build_cascade_layer_views(&cascade_array_texture, cascade_max_layers)?;

        let cube_slot_count = config.max_point_shadows.max(1);
        let cube_layer_count = cube_slot_count * 6;
        let cube_resolution = clamp_point_shadow_resolution(config.point_shadow_resolution);
        let cube_array_texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::Depth32float,
                Extent3d::new(
                    cube_resolution,
                    Some(cube_resolution),
                    Some(cube_layer_count),
                ),
                TextureUsage::new()
                    .with_render_attachment()
                    .with_texture_binding(),
            )
            .with_label("Shadow Cube Pool")
            .into(),
        )?;
        let cube_array_view = create_cube_array_view(&cube_array_texture)?;
        let cube_2d_array_view = create_cube_2d_array_view(&cube_array_texture)?;
        let cube_face_views = build_cube_face_views(&cube_array_texture, cube_layer_count)?;

        let descriptors_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Shadow Descriptors (storage)"),
                SHADOW_DESCRIPTOR_BYTES,
                BufferUsage::new().with_storage().with_copy_dst(),
            )
            .into(),
        )?;

        let descriptors_uniform = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Shadow Descriptors (uniform)"),
                *SHADOW_DESCRIPTOR_UNIFORM_BYTES,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;

        let globals_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Shadow Globals"),
                SHADOW_GLOBALS_BYTES,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;

        // N slots × 256 B stride. Each slot stores the per-view
        // matrix + bias floats for one shadow render pass. The bind
        // group uses dynamic offsets so we can write all slots in
        // `write_gpu` (once per frame) and select the right slot
        // per render pass without re-queueing buffer writes between
        // passes — `queue.writeBuffer` flushes all writes BEFORE any
        // command buffer executes, so per-pass writes to a single
        // slot would cause every pass to see the last-written value.
        let shadow_view_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Shadow Views"),
                SHADOW_VIEW_STRIDE * MAX_SHADOW_VIEWS as usize,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;

        // Clamp-to-edge on all three axes prevents the cube comparison
        // sampler from wrapping at face boundaries — WebGPU has no
        // "seamless cubemap" toggle, so the address mode IS the seam
        // policy. Without this, bilinear taps at a cube face edge can
        // read from the opposite face's coordinate space and produce
        // ghost shadows at the seam.
        let sampler_comparison = gpu.create_sampler(Some(
            &SamplerDescriptor {
                label: Some("Shadow Comparison Sampler"),
                compare: Some(CompareFunction::LessEqual),
                mag_filter: Some(FilterMode::Linear),
                min_filter: Some(FilterMode::Linear),
                address_mode_u: Some(awsm_renderer_core::sampler::AddressMode::ClampToEdge),
                address_mode_v: Some(awsm_renderer_core::sampler::AddressMode::ClampToEdge),
                address_mode_w: Some(awsm_renderer_core::sampler::AddressMode::ClampToEdge),
                ..SamplerDescriptor::default()
            }
            .into(),
        ));

        let sampler_filterable = gpu.create_sampler(Some(
            &SamplerDescriptor {
                label: Some("Shadow Filterable Sampler"),
                mag_filter: Some(FilterMode::Linear),
                min_filter: Some(FilterMode::Linear),
                ..SamplerDescriptor::default()
            }
            .into(),
        ));

        // Slot 0 of the shadow pipeline: a per-view uniform that the
        // render pass selects via dynamic offset (one slot per
        // active shadow descriptor).
        let shadow_view_bind_group_layout_key = bind_group_layouts.get_key(
            gpu,
            BindGroupLayoutCacheKey {
                entries: vec![BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new()
                            .with_binding_type(BufferBindingType::Uniform)
                            .with_dynamic_offset(true),
                    ),
                    visibility_vertex: true,
                    visibility_fragment: false,
                    visibility_compute: false,
                }],
            },
        )?;

        let shadow_view_bind_group = {
            let layout = bind_group_layouts.get(shadow_view_bind_group_layout_key)?;
            let entries = vec![BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(
                    BufferBinding::new(&shadow_view_buffer).with_size(SHADOW_VIEW_BYTES),
                ),
            )];
            let descriptor = BindGroupDescriptor::new(layout, Some("Shadow View"), entries);
            gpu.create_bind_group(&descriptor.into())
        };

        // Pipeline layout: [shadow_view, transforms, meta, animation].
        // Slots 1..=3 reuse the geometry pass's layouts so the same
        // model_transforms / geometry_mesh_meta / morph + skin buffers
        // are accessible verbatim from the shadow VS. `@group(2)`
        // forks by instancing: non-instanced shadow shaders use the
        // storage-array meta layout (indexed by `instance_index`),
        // instanced shaders use uniform-with-dynamic-offset.
        let shadow_pipeline_layout_key_storage = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![
                shadow_view_bind_group_layout_key,
                geometry_bind_groups.transforms.bind_group_layout_key,
                geometry_bind_groups.meta.storage_layout_key,
                geometry_bind_groups.animation.bind_group_layout_key,
            ]),
        )?;
        let shadow_pipeline_layout_key_uniform = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![
                shadow_view_bind_group_layout_key,
                geometry_bind_groups.transforms.bind_group_layout_key,
                geometry_bind_groups.meta.uniform_layout_key,
                geometry_bind_groups.animation.bind_group_layout_key,
            ]),
        )?;

        // Resolve the two caster shader keys against the pre-warmed
        // cache. RenderPasses::new emits both as part of its cross-pass
        // ensure_keys; in the standalone `Shadows::new` path the
        // wrapper above issues its own ensure_keys first. Either way
        // these are sync cache hits.
        let shader_no_instancing = shaders
            .get_key(
                gpu,
                crate::shadows::shader::cache_key::ShaderCacheKeyShadow {
                    instancing_transforms: false,
                },
            )
            .await?;
        let shader_instancing = shaders
            .get_key(
                gpu,
                crate::shadows::shader::cache_key::ShaderCacheKeyShadow {
                    instancing_transforms: true,
                },
            )
            .await?;
        // 8 caster variants: the four single-sided `(instancing, cube_face)`
        // combos (indices 0-3) followed by the same four with `double_sided`
        // (no-cull, indices 4-7). Keep this order in sync with the assignment
        // in `ensure_pipelines_compiled` / `from_resolved` and the
        // `shadow_pipeline_key` lookup.
        let caster_pipeline_cache_keys = vec![
            shadow_pipeline_cache_key(
                shader_no_instancing,
                shadow_pipeline_layout_key_storage,
                false,
                false,
                false,
            ),
            shadow_pipeline_cache_key(
                shader_instancing,
                shadow_pipeline_layout_key_uniform,
                true,
                false,
                false,
            ),
            shadow_pipeline_cache_key(
                shader_no_instancing,
                shadow_pipeline_layout_key_storage,
                false,
                true,
                false,
            ),
            shadow_pipeline_cache_key(
                shader_instancing,
                shadow_pipeline_layout_key_uniform,
                true,
                true,
                false,
            ),
            shadow_pipeline_cache_key(
                shader_no_instancing,
                shadow_pipeline_layout_key_storage,
                false,
                false,
                true,
            ),
            shadow_pipeline_cache_key(
                shader_instancing,
                shadow_pipeline_layout_key_uniform,
                true,
                false,
                true,
            ),
            shadow_pipeline_cache_key(
                shader_no_instancing,
                shadow_pipeline_layout_key_storage,
                false,
                true,
                true,
            ),
            shadow_pipeline_cache_key(
                shader_instancing,
                shadow_pipeline_layout_key_uniform,
                true,
                true,
                true,
            ),
        ];

        let evsm = EvsmPass::build_descriptors(gpu, bind_group_layouts, pipeline_layouts)?;

        Ok(ShadowsDescriptors {
            config,
            atlas_texture,
            atlas_view,
            atlas_size,
            evsm_atlas_texture,
            evsm_atlas_view,
            evsm_atlas_size,
            evsm_blur_pingpong_texture,
            evsm_blur_pingpong_view,
            cascade_array_texture,
            cascade_array_view,
            cascade_layer_views,
            cascade_resolution,
            cascade_max_layers,
            cube_array_texture,
            cube_array_view,
            cube_2d_array_view,
            cube_face_views,
            cube_resolution,
            cube_slot_count,
            descriptors_buffer,
            descriptors_uniform,
            globals_buffer,
            shadow_view_buffer,
            sampler_comparison,
            sampler_filterable,
            shadow_view_bind_group_layout_key,
            shadow_view_bind_group,
            shadow_pipeline_layout_key_storage,
            shadow_pipeline_layout_key_uniform,
            caster_pipeline_cache_keys,
            evsm,
        })
    }

    /// Folds the resolved caster + EVSM pipeline keys back into the
    /// typed [`Shadows`] handle. Sync; the orchestrator has already
    /// run the cross-tail batched `RenderPipelines::ensure_keys` +
    /// `ComputePipelines::ensure_keys` and the EVSM validate join.
    ///
    /// `caster_resolved` is in `caster_pipeline_cache_keys` order:
    /// `(no_inst, planar)`, `(inst, planar)`, `(no_inst, cube)`,
    /// `(inst, cube)`.
    ///
    /// Pass empty `caster_resolved` AND empty `evsm_resolved` slices to
    /// defer pipeline compile (Block B.1 + B.2 — lazy until the first
    /// shadow-casting light). In that case `pending_caster_cache_keys`
    /// MUST hold 8 entries in the same order and `pending_evsm_cache_keys`
    /// MUST hold 3 entries in moment_write → blur_h → blur_v order;
    /// [`Shadows::ensure_pipelines_compiled`] hands them to `ensure_keys`
    /// when the first shadow-caster lands. Non-pipeline GPU resources
    /// still materialise; pipeline-key `Option`s stay `None` and the
    /// dispatch sites warn-skip via
    /// `pipeline_scheduler::warn_pipeline_not_compiled`.
    pub fn from_resolved(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &BindGroupLayouts,
        descs: ShadowsDescriptors,
        caster_resolved: Vec<RenderPipelineKey>,
        evsm_resolved: Vec<ComputePipelineKey>,
        pending_caster_cache_keys: Vec<RenderPipelineCacheKey>,
        pending_evsm_cache_keys: Vec<crate::pipelines::compute_pipeline::ComputePipelineCacheKey>,
    ) -> Result<Self, AwsmShadowError> {
        let deferred = caster_resolved.is_empty() && evsm_resolved.is_empty();
        debug_assert!(
            deferred || (caster_resolved.len() == 8 && evsm_resolved.len() == 3),
            "Shadows::from_resolved: pass either (8 caster + 3 evsm) or (0 + 0); \
             got ({}, {})",
            caster_resolved.len(),
            evsm_resolved.len(),
        );
        debug_assert!(
            !deferred
                || (pending_caster_cache_keys.len() == 8
                    && pending_evsm_cache_keys.len() == 3),
            "Shadows::from_resolved deferred path requires 8 pending caster + 3 pending evsm cache keys"
        );
        let ShadowsDescriptors {
            config,
            atlas_texture,
            atlas_view,
            atlas_size,
            evsm_atlas_texture,
            evsm_atlas_view,
            evsm_atlas_size,
            evsm_blur_pingpong_texture,
            evsm_blur_pingpong_view,
            cascade_array_texture,
            cascade_array_view,
            cascade_layer_views,
            cascade_resolution,
            cascade_max_layers,
            cube_array_texture,
            cube_array_view,
            cube_2d_array_view,
            cube_face_views,
            cube_resolution,
            cube_slot_count,
            descriptors_buffer,
            descriptors_uniform,
            globals_buffer,
            shadow_view_buffer,
            sampler_comparison,
            sampler_filterable,
            shadow_view_bind_group_layout_key,
            shadow_view_bind_group,
            shadow_pipeline_layout_key_storage,
            shadow_pipeline_layout_key_uniform,
            caster_pipeline_cache_keys: _,
            evsm,
        } = descs;

        let evsm_pass = EvsmPass::from_resolved(evsm, evsm_resolved);

        // EVSM moment-write + blur bind groups depend on the layouts +
        // params buffer that EvsmPass now owns. Built here (sync, no
        // pipeline compile) so the typed `Shadows` can sample the
        // bind group at render time without re-resolving layouts.
        let evsm_moment_write_bind_group = build_evsm_moment_write_bind_group(
            gpu,
            bind_group_layouts,
            evsm_pass.moment_write_layout_key,
            &cascade_array_view,
            &evsm_atlas_view,
            &evsm_pass.params_buffer,
        )?;
        let evsm_blur_h_bind_group = build_evsm_blur_bind_group(
            gpu,
            bind_group_layouts,
            evsm_pass.blur_layout_key,
            &evsm_atlas_view,
            &evsm_blur_pingpong_view,
            &evsm_pass.params_buffer,
            "Shadow EVSM Blur H Bind Group",
        )?;
        let evsm_blur_v_bind_group = build_evsm_blur_bind_group(
            gpu,
            bind_group_layouts,
            evsm_pass.blur_layout_key,
            &evsm_blur_pingpong_view,
            &evsm_atlas_view,
            &evsm_pass.params_buffer,
            "Shadow EVSM Blur V Bind Group",
        )?;

        Ok(Self {
            config,
            atlas_texture,
            atlas_view,
            atlas_size,
            evsm_atlas_texture,
            evsm_atlas_view,
            cascade_array_texture,
            cascade_array_view,
            cascade_layer_views,
            cascade_resolution,
            cascade_max_layers,
            cube_array_texture,
            cube_array_view,
            cube_2d_array_view,
            cube_face_views,
            cube_resolution,
            cube_slots: vec![None; cube_slot_count as usize],
            evsm_atlas_size,
            evsm_blur_pingpong_texture,
            evsm_blur_pingpong_view,
            evsm_pass,
            evsm_dispatch_queue: Vec::new(),
            evsm_moment_write_bind_group,
            evsm_blur_h_bind_group,
            evsm_blur_v_bind_group,
            descriptors_buffer,
            descriptors_uniform,
            globals_buffer,
            shadow_view_buffer,
            sampler_comparison,
            sampler_filterable,
            params: SecondaryMap::new(),
            cube_slot_for_light: SecondaryMap::new(),
            records: SecondaryMap::new(),
            throttle: SecondaryMap::new(),
            active_descriptor_count: 0,
            active_view_count: 0,
            shadow_view_bind_group_layout_key,
            shadow_view_bind_group,
            shadow_pipeline_layout_key_storage,
            shadow_pipeline_layout_key_uniform,
            shadow_pipeline_no_instancing: caster_resolved.first().copied(),
            shadow_pipeline_instancing: caster_resolved.get(1).copied(),
            shadow_pipeline_cube_no_instancing: caster_resolved.get(2).copied(),
            shadow_pipeline_cube_instancing: caster_resolved.get(3).copied(),
            shadow_pipeline_no_instancing_ds: caster_resolved.get(4).copied(),
            shadow_pipeline_instancing_ds: caster_resolved.get(5).copied(),
            shadow_pipeline_cube_no_instancing_ds: caster_resolved.get(6).copied(),
            shadow_pipeline_cube_instancing_ds: caster_resolved.get(7).copied(),
            pending_caster_cache_keys,
            pending_evsm_cache_keys,
            frame_count: 0,
            caster_moved_this_frame: false,
            caster_set_revision: 0,
            caster_set_last_signature: None,
            dirty: true,
            pending_atlas_grow: false,
            pending_evsm_grow: false,
            warned_evsm_overflow: false,
            pending_resource_recreate: PendingResourceRecreate::default(),
            caster_aabbs_scratch: Vec::new(),
            descriptor_bytes_scratch: vec![0u8; *SHADOW_DESCRIPTOR_UNIFORM_BYTES],
            view_bytes_scratch: vec![0u8; SHADOW_VIEW_STRIDE * MAX_SHADOW_VIEWS as usize],
            globals_uploader: crate::buffer::mapped_uploader::MappedUploader::new("Shadow Globals"),
            descriptors_uploader: crate::buffer::mapped_uploader::MappedUploader::new(
                "Shadow Descriptors",
            ),
            views_uploader: crate::buffer::mapped_uploader::MappedUploader::new("Shadow Views"),
        })
    }

    /// Mapped-ring upload telemetry for the shadow buffers (globals +
    /// descriptors + views aggregated).
    pub fn upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        let mut s = self.globals_uploader.stats();
        let b = self.descriptors_uploader.stats();
        let c = self.views_uploader.stats();
        s.peak_ring_depth_used = s
            .peak_ring_depth_used
            .max(b.peak_ring_depth_used)
            .max(c.peak_ring_depth_used);
        s.fallback_count += b.fallback_count + c.fallback_count;
        s.map_async_wait_ms += b.map_async_wait_ms + c.map_async_wait_ms;
        s.bytes_uploaded_via_ring += b.bytes_uploaded_via_ring + c.bytes_uploaded_via_ring;
        s.bytes_uploaded_via_fallback +=
            b.bytes_uploaded_via_fallback + c.bytes_uploaded_via_fallback;
        s.bytes_uploaded_via_writebuffer +=
            b.bytes_uploaded_via_writebuffer + c.bytes_uploaded_via_writebuffer;
        s.resize_count += b.resize_count + c.resize_count;
        s
    }

    /// Replaces the renderer-wide config.
    ///
    /// Lightweight fields (SSCS toggle, debug flags, EVSM tuning) take
    /// effect on the next `write_gpu`. Resource-shape fields
    /// (`atlas_size`, `evsm_atlas_size`, `max_point_shadows`,
    /// `point_shadow_resolution`) trigger a tear-down + rebuild of
    /// the corresponding GPU textures + bind groups at the start of
    /// the next `write_gpu` — recreating GPU resources is not free
    /// (texture alloc + dependent-bind-group rebuild) so don't poke
    /// these at frame rate; from the editor inspector they're fine.
    pub fn set_config(&mut self, config: ShadowsConfig) {
        warn_view_budget(&config);
        let new_atlas = config.atlas_size.max(1);
        let new_evsm = config.evsm_atlas_size.max(1);
        let new_cube_count = config.max_point_shadows.max(1);
        let new_cube_res = clamp_point_shadow_resolution(config.point_shadow_resolution);
        let new_cascade_res = config.cascade_resolution.max(16);
        let new_cascade_layers = config.cascade_array_max_layers.max(1);
        if new_atlas != self.atlas_size {
            self.pending_resource_recreate.pcf_atlas = true;
        }
        if new_evsm != self.evsm_atlas_size {
            self.pending_resource_recreate.evsm_atlas = true;
        }
        if new_cube_count != self.cube_slots.len() as u32 || new_cube_res != self.cube_resolution {
            self.pending_resource_recreate.cube_pool = true;
        }
        if new_cascade_res != self.cascade_resolution
            || new_cascade_layers != self.cascade_max_layers
        {
            self.pending_resource_recreate.cascade_array = true;
        }
        self.config = config;
        self.dirty = true;
    }

    /// Returns a reference to the renderer-wide config.
    pub fn config(&self) -> &ShadowsConfig {
        &self.config
    }

    /// Number of lights currently registered as shadow casters
    /// (whether or not their `cast` flag is on).
    pub fn caster_count(&self) -> usize {
        self.params.values().filter(|p| p.cast).count()
    }

    /// `[0.0, 1.0]` — fraction of the 2D atlas occupied by active
    /// cascades + spots. Currently a coarse indicator: returns 1.0 if
    /// any caster is active, 0 otherwise.
    pub fn atlas_utilization(&self) -> f32 {
        if self.caster_count() > 0 {
            1.0
        } else {
            0.0
        }
    }

    /// Fraction of cube-array slots occupied. Currently a stub —
    /// the cube-pool allocator needs to surface its watermark for a
    /// meaningful value here.
    pub fn cube_pool_utilization(&self) -> f32 {
        0.0
    }

    /// Tear down and rebuild whichever GPU resources were marked dirty
    /// by `set_config`. Each block is independent — only the resources
    /// that actually changed get touched. After every successful path
    /// we mark `ShadowsResourcesChange` so the consumer-side opaque /
    /// transparent shadow bind groups get re-bound, and reset throttle
    /// state so previously-rendered cascades re-rasterise into the
    /// freshly-allocated texture (otherwise they would read stale or
    /// uninitialised memory and flicker).
    fn apply_pending_resource_recreate(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &BindGroupLayouts,
        bind_groups: &mut BindGroups,
    ) -> Result<(), AwsmShadowError> {
        let recreate = std::mem::take(&mut self.pending_resource_recreate);

        if recreate.pcf_atlas {
            let new_size = self.config.atlas_size.max(1);
            tracing::info!(
                "shadow PCF atlas resize (config) {} → {}",
                self.atlas_size,
                new_size
            );
            self.atlas_size = new_size;
            self.atlas_texture = gpu.create_texture(
                &TextureDescriptor::new(
                    TextureFormat::Depth32float,
                    Extent3d::new(self.atlas_size, Some(self.atlas_size), Some(1)),
                    TextureUsage::new()
                        .with_render_attachment()
                        .with_texture_binding(),
                )
                .with_label("Shadow Atlas")
                .into(),
            )?;
            self.atlas_view = self
                .atlas_texture
                .create_view()
                .map_err(AwsmCoreError::create_texture_view)?;
            // The 2D atlas only carries spot-light depth now; the
            // moment-write bind group reads from `cascade_array_view`
            // (cascade depth), so a pure PCF-atlas resize doesn't
            // require a moment-write rebind.
        }

        if recreate.evsm_atlas {
            let new_size = self.config.evsm_atlas_size.max(1);
            tracing::info!(
                "shadow EVSM atlas resize (config) {} → {}",
                self.evsm_atlas_size,
                new_size
            );
            self.evsm_atlas_size = new_size;
            self.evsm_atlas_texture = gpu.create_texture(
                &TextureDescriptor::new(
                    TextureFormat::Rgba16float,
                    Extent3d::new(new_size, Some(new_size), Some(1)),
                    TextureUsage::new()
                        .with_render_attachment()
                        .with_texture_binding()
                        .with_storage_binding(),
                )
                .with_label("Shadow EVSM Atlas")
                .into(),
            )?;
            self.evsm_atlas_view = self
                .evsm_atlas_texture
                .create_view()
                .map_err(AwsmCoreError::create_texture_view)?;
            self.evsm_blur_pingpong_texture = gpu.create_texture(
                &TextureDescriptor::new(
                    TextureFormat::Rgba16float,
                    Extent3d::new(new_size, Some(new_size), Some(1)),
                    TextureUsage::new()
                        .with_texture_binding()
                        .with_storage_binding(),
                )
                .with_label("Shadow EVSM Blur Ping-pong")
                .into(),
            )?;
            self.evsm_blur_pingpong_view = self
                .evsm_blur_pingpong_texture
                .create_view()
                .map_err(AwsmCoreError::create_texture_view)?;
            // All three EVSM bind groups reference at least one of the
            // recreated views, so rebuild all three.
            self.evsm_moment_write_bind_group = build_evsm_moment_write_bind_group(
                gpu,
                bind_group_layouts,
                self.evsm_pass.moment_write_layout_key,
                &self.cascade_array_view,
                &self.evsm_atlas_view,
                &self.evsm_pass.params_buffer,
            )?;
            self.evsm_blur_h_bind_group = build_evsm_blur_bind_group(
                gpu,
                bind_group_layouts,
                self.evsm_pass.blur_layout_key,
                &self.evsm_atlas_view,
                &self.evsm_blur_pingpong_view,
                &self.evsm_pass.params_buffer,
                "Shadow EVSM Blur H Bind Group",
            )?;
            self.evsm_blur_v_bind_group = build_evsm_blur_bind_group(
                gpu,
                bind_group_layouts,
                self.evsm_pass.blur_layout_key,
                &self.evsm_blur_pingpong_view,
                &self.evsm_atlas_view,
                &self.evsm_pass.params_buffer,
                "Shadow EVSM Blur V Bind Group",
            )?;
        }

        if recreate.cube_pool {
            let new_count = self.config.max_point_shadows.max(1);
            let new_res = clamp_point_shadow_resolution(self.config.point_shadow_resolution);
            tracing::info!(
                "shadow cube pool resize (config) {} × {}² → {} × {}²",
                self.cube_slots.len(),
                self.cube_resolution,
                new_count,
                new_res,
            );
            let new_layers = new_count * 6;
            self.cube_array_texture = gpu.create_texture(
                &TextureDescriptor::new(
                    TextureFormat::Depth32float,
                    Extent3d::new(new_res, Some(new_res), Some(new_layers)),
                    TextureUsage::new()
                        .with_render_attachment()
                        .with_texture_binding(),
                )
                .with_label("Shadow Cube Pool")
                .into(),
            )?;
            self.cube_array_view = create_cube_array_view(&self.cube_array_texture)?;
            self.cube_2d_array_view = create_cube_2d_array_view(&self.cube_array_texture)?;
            self.cube_face_views = build_cube_face_views(&self.cube_array_texture, new_layers)?;
            self.cube_resolution = new_res;
            // Slot ownership is keyed by index — when the pool size
            // changes (or any face is recreated), every previously-
            // resident shadow's contents are gone. Clear ownership so
            // the next descriptor pack re-allocates from scratch.
            self.cube_slots = vec![None; new_count as usize];
            // Slot ownership reset → drop the per-light index cache so
            // the next frame's lookup falls through to a fresh slot
            // search instead of trusting a stale slot_index.
            self.cube_slot_for_light.clear();
        }

        if recreate.cascade_array {
            let new_res = self.config.cascade_resolution.max(16);
            let new_layers = self.config.cascade_array_max_layers.max(1);
            tracing::info!(
                "shadow cascade-array resize (config) {} × {}² → {} × {}²",
                self.cascade_max_layers,
                self.cascade_resolution,
                new_layers,
                new_res,
            );
            self.cascade_resolution = new_res;
            self.cascade_max_layers = new_layers;
            self.cascade_array_texture = gpu.create_texture(
                &TextureDescriptor::new(
                    TextureFormat::Depth32float,
                    Extent3d::new(new_res, Some(new_res), Some(new_layers)),
                    TextureUsage::new()
                        .with_render_attachment()
                        .with_texture_binding(),
                )
                .with_label("Shadow Cascade Array")
                .into(),
            )?;
            self.cascade_array_view =
                crate::shadows::helpers::create_cascade_array_view(&self.cascade_array_texture)?;
            self.cascade_layer_views = crate::shadows::helpers::build_cascade_layer_views(
                &self.cascade_array_texture,
                new_layers,
            )?;
            // Moment-write reads from the cascade-array view — rebind
            // against the freshly-created view. Blur bind groups stay
            // valid (EVSM atlas + ping-pong only).
            self.evsm_moment_write_bind_group = build_evsm_moment_write_bind_group(
                gpu,
                bind_group_layouts,
                self.evsm_pass.moment_write_layout_key,
                &self.cascade_array_view,
                &self.evsm_atlas_view,
                &self.evsm_pass.params_buffer,
            )?;
        }

        // Re-rasterise only the views whose backing texture actually
        // changed. The throttle is parallel-indexed with the previous
        // frame's `records.views`, so each entry can be classified by
        // its `LightShadowView` flags. EVSM atlas resize doesn't need
        // its own invalidation pass — EVSM moments are re-computed
        // every frame from cascade depth, so any cascade-array
        // invalidation already covers it.
        let invalidate_2d = recreate.pcf_atlas;
        let invalidate_cube = recreate.cube_pool;
        let invalidate_cascade = recreate.cascade_array;
        if invalidate_2d || invalidate_cube || invalidate_cascade {
            for (key, entries) in self.throttle.iter_mut() {
                let prev_views = self.records.get(key).map(|r| r.views.as_slice());
                for (i, t) in entries.iter_mut().enumerate() {
                    let view = prev_views.and_then(|v| v.get(i));
                    let is_cube = view.map(|v| v.cube_layer.is_some()).unwrap_or(false);
                    let is_cascade = view.map(|v| v.cascade_layer.is_some()).unwrap_or(false);
                    let hit = if is_cube {
                        invalidate_cube
                    } else if is_cascade {
                        invalidate_cascade
                    } else {
                        invalidate_2d
                    };
                    if hit {
                        t.last_rendered_frame = u64::MAX;
                    }
                }
            }
        }
        bind_groups.mark_create(crate::bind_groups::BindGroupCreate::ShadowsResourcesChange);
        // `set_config` already flagged `dirty` before our pending
        // recreate flags were ever consulted, so the globals upload
        // is queued — but make the dependency explicit here too:
        // the PCF/PCSS shader reads `atlas_sizes` from globals and
        // any sample after this point must see the new size, even if
        // the recreate path is invoked from a non-`set_config` source
        // in the future.
        self.dirty = true;
        Ok(())
    }

    /// `true` if any shadow-casting light is currently active. The
    /// render graph short-circuits the entire shadow generation pass
    /// when this is `false`.
    pub fn any_active(&self) -> bool {
        self.caster_count() > 0
    }

    /// Returns the shadow descriptor index registered for a light, or
    /// `SHADOW_INDEX_NONE` if the light has no active shadow.
    pub fn descriptor_index_for_light(&self, key: LightKey) -> u32 {
        self.records
            .get(key)
            .map(|r| r.descriptor_base)
            .unwrap_or(SHADOW_INDEX_NONE)
    }

    /// Returns the shadow pipeline key for the given instancing mode,
    /// or `None` if the caster pipelines have not yet been compiled
    /// (Block B.2 — deferred until the first shadow-casting light).
    /// The dispatch site in `shadows::render_pass::record` checks
    /// [`Self::pipelines_compiled`] up front and warn-skips the whole
    /// pass via `pipeline_scheduler::warn_pipeline_not_compiled`, so
    /// inside the draw loop the `Option` is always `Some`.
    ///
    /// Cube (point) and 2D (cascade/spot) shadows use distinct pipelines:
    /// the cube pipeline has `front_face = Cw` to compensate for the
    /// Y-flip applied to the cube projection — see `write_gpu`.
    pub fn shadow_pipeline_key(
        &self,
        instancing: bool,
        cube_face: bool,
        double_sided: bool,
    ) -> Option<RenderPipelineKey> {
        match (double_sided, cube_face, instancing) {
            (false, true, true) => self.shadow_pipeline_cube_instancing,
            (false, true, false) => self.shadow_pipeline_cube_no_instancing,
            (false, false, true) => self.shadow_pipeline_instancing,
            (false, false, false) => self.shadow_pipeline_no_instancing,
            (true, true, true) => self.shadow_pipeline_cube_instancing_ds,
            (true, true, false) => self.shadow_pipeline_cube_no_instancing_ds,
            (true, false, true) => self.shadow_pipeline_instancing_ds,
            (true, false, false) => self.shadow_pipeline_no_instancing_ds,
        }
    }

    /// `true` once [`Self::ensure_pipelines_compiled`] has run — i.e.,
    /// the 8 caster render pipelines + 3 EVSM compute pipelines are
    /// GPU-resident. The first shadow-casting light triggers compile;
    /// subsequent calls are no-ops. Dispatch sites use this to
    /// short-circuit before issuing a warn-skip.
    pub fn pipelines_compiled(&self) -> bool {
        self.shadow_pipeline_no_instancing.is_some()
    }

    /// Compiles the shadow caster + EVSM pipelines if they aren't
    /// already. Idempotent — second and subsequent calls are no-ops.
    ///
    /// Block B.1 + B.2: this is the lazy-pool trigger point. Cold-boot
    /// constructs the `Shadows` struct with `None` pipeline keys plus
    /// 8 pending caster + 3 pending EVSM cache keys held on `self`.
    /// The first shadow-casting light (`shadows/api.rs::insert_light`,
    /// `set_light_shadow_params`, `update_light_shadow`) calls this to
    /// resolve the pipelines through batched `ensure_keys`.
    ///
    /// Caster pipelines don't depend on MSAA (they write depth only)
    /// so `set_anti_aliasing` does not need to recompile them; EVSM
    /// pipelines are likewise MSAA-invariant. Hence no MSAA-flip
    /// re-entry point is needed here — only the first shadow-caster.
    pub async fn ensure_pipelines_compiled(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        shaders: &Shaders,
        pipeline_layouts: &PipelineLayouts,
        render_pipelines: &mut crate::pipelines::render_pipeline::RenderPipelines,
        compute_pipelines: &mut crate::pipelines::compute_pipeline::ComputePipelines,
    ) -> Result<(), AwsmShadowError> {
        if self.pipelines_compiled() {
            return Ok(());
        }
        // The cache keys MUST have been seeded by `from_resolved`'s
        // deferred path. If they're empty here AND the pipelines
        // aren't compiled, the construction site fed us wrong inputs
        // — log and bail rather than silently doing nothing.
        if self.pending_caster_cache_keys.is_empty() || self.pending_evsm_cache_keys.is_empty() {
            tracing::error!(
                target: "awsm_renderer::pipeline_readiness",
                "Shadows::ensure_pipelines_compiled: pipelines not compiled yet but pending cache keys empty (caster: {}, evsm: {})",
                self.pending_caster_cache_keys.len(),
                self.pending_evsm_cache_keys.len(),
            );
            return Ok(());
        }
        tracing::info!(
            target: "awsm_renderer::boot_timing",
            "Shadows::ensure_pipelines_compiled: first shadow-caster — compiling 8 caster + 3 EVSM pipelines"
        );
        // Clone (not `take`) so a fallible `?` on either ensure_keys
        // await leaves the pending vectors intact for the next retry.
        // The previous `mem::take` flow emptied the pending vectors
        // BEFORE either await ran — if the caster batch rejected
        // (e.g. transient WebGPU device hiccup or a broken shader),
        // the `?` bailed with both pending vectors emptied AND every
        // pipeline option still `None`. The next ensure_pipelines_compiled
        // call hit the "pending cache keys empty" guard and returned
        // `Ok(())`, leaving shadows uncompiled for the rest of the
        // session.
        //
        // Cache keys are cheap (a few small u64-ish fields per entry,
        // 7 entries total). Cloning is preferable to the alternative
        // of `take`+restore-on-error since it avoids the gotcha where
        // an early-return between the two awaits leaves the vectors
        // half-emptied.
        let caster_keys = self.pending_caster_cache_keys.clone();
        let evsm_keys = self.pending_evsm_cache_keys.clone();

        let caster_resolved = render_pipelines
            .ensure_keys(gpu, shaders, pipeline_layouts, caster_keys)
            .await?;
        let evsm_resolved = compute_pipelines
            .ensure_keys(gpu, shaders, pipeline_layouts, evsm_keys)
            .await?;
        debug_assert_eq!(caster_resolved.len(), 8);
        debug_assert_eq!(evsm_resolved.len(), 3);
        self.shadow_pipeline_no_instancing = Some(caster_resolved[0]);
        self.shadow_pipeline_instancing = Some(caster_resolved[1]);
        self.shadow_pipeline_cube_no_instancing = Some(caster_resolved[2]);
        self.shadow_pipeline_cube_instancing = Some(caster_resolved[3]);
        self.shadow_pipeline_no_instancing_ds = Some(caster_resolved[4]);
        self.shadow_pipeline_instancing_ds = Some(caster_resolved[5]);
        self.shadow_pipeline_cube_no_instancing_ds = Some(caster_resolved[6]);
        self.shadow_pipeline_cube_instancing_ds = Some(caster_resolved[7]);
        self.evsm_pass.moment_write_pipeline_key = Some(evsm_resolved[0]);
        self.evsm_pass.blur_h_pipeline_key = Some(evsm_resolved[1]);
        self.evsm_pass.blur_v_pipeline_key = Some(evsm_resolved[2]);
        // Both batches landed — clear the pending vectors so the
        // next call's "already compiled" fast path takes over.
        self.pending_caster_cache_keys.clear();
        self.pending_evsm_cache_keys.clear();
        Ok(())
    }

    /// Returns the shadow_view bind group. Created eagerly in
    /// `Shadows::new` so the render pass only needs shared borrows.
    pub fn shadow_view_bind_group(&self) -> &web_sys::GpuBindGroup {
        &self.shadow_view_bind_group
    }

    /// Per-frame upload point. Refits cascades against the current
    /// camera, packs descriptors into the uniform buffer, and writes
    /// shadow globals when dirty.
    /// §B: note that a shadow-caster mesh moved this frame (OR-accumulated across
    /// the multiple per-frame `AwsmRenderer::update_transforms` calls; read + reset
    /// by [`Self::take_shadow_static`]).
    pub fn note_shadow_caster_moved(&mut self) {
        self.caster_moved_this_frame = true;
    }

    /// §B: bump the caster-set revision — call when a mesh's shadow flags toggle
    /// (an existing mesh joining/leaving the caster set without a count change).
    pub fn bump_shadow_caster_revision(&mut self) {
        self.caster_set_revision = self.caster_set_revision.wrapping_add(1);
    }

    /// §B: resolve the "casters static this frame" gate and reset the per-frame
    /// caster-moved accumulator. `mesh_count` is the renderer's total mesh count
    /// (the add/remove proxy for the caster set); `external_static` folds in the
    /// caller's camera-still + no-time-driven-material signals. Returns whether the
    /// periodic throttle may suppress a re-render this frame. Must be called once
    /// per frame, AFTER every `update_transforms` and BEFORE [`Self::write_gpu`].
    ///
    /// Conservative: any of {a caster moved, the caster set changed, camera moved,
    /// a time-driven shadow material present} ⇒ NOT static ⇒ periodic views still
    /// re-render. Forced re-renders (rect / layer / view-projection drift / config)
    /// always fire regardless — this only gates the *periodic* suppression.
    pub fn take_shadow_static(&mut self, mesh_count: usize, external_static: bool) -> bool {
        let signature = (mesh_count, self.caster_set_revision);
        let caster_set_changed = self.caster_set_last_signature != Some(signature);
        self.caster_set_last_signature = Some(signature);
        let static_now = external_static && !self.caster_moved_this_frame && !caster_set_changed;
        self.caster_moved_this_frame = false;
        static_now
    }

    pub fn write_gpu(
        &mut self,
        _logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &BindGroupLayouts,
        bind_groups: &mut BindGroups,
        camera: &crate::camera::CameraBuffer,
        lights: &crate::lights::Lights,
        scene_spatial: &crate::scene_spatial::SceneSpatial,
        // §B: when true, the periodic throttle may suppress a re-render this frame
        // (casters + camera static, no time-driven shadow material). Forced
        // re-renders still fire. Computed by `take_shadow_static`.
        shadow_static: bool,
    ) -> Result<(), AwsmShadowError> {
        // EVSM atlas auto-grow: a prior frame's cascade placement
        // overflowed (typically a second shadowed directional light —
        // the default atlas is exactly one default-resolution tile).
        // Double the size (capped) and ride the same recreate path a
        // `set_config` edit takes, applied just below.
        if self.pending_evsm_grow {
            self.pending_evsm_grow = false;
            let new_size =
                (self.config.evsm_atlas_size.saturating_mul(2)).min(SHADOW_ATLAS_MAX_SIZE);
            if new_size > self.config.evsm_atlas_size {
                tracing::info!(
                    "EVSM atlas overflow → growing from {} to {}",
                    self.config.evsm_atlas_size,
                    new_size
                );
                self.config.evsm_atlas_size = new_size;
                self.pending_resource_recreate.evsm_atlas = true;
                self.warned_evsm_overflow = false;
                // ShadowGlobals carries the EVSM atlas size — re-upload.
                self.dirty = true;
            }
        }

        // User-driven resource recreates land first so a fresh
        // `set_config` from the editor takes effect immediately. The
        // auto-grow path below operates on whatever size landed here.
        if self.pending_resource_recreate.any() {
            self.apply_pending_resource_recreate(gpu, bind_group_layouts, bind_groups)?;
        }

        // Dynamic atlas resize. If the previous frame's packer ran
        // out of room we grow the atlas to the next power of two
        // (capped at `SHADOW_ATLAS_MAX_SIZE`) before this frame's
        // pack. Recreates the texture + view and tells the bind-group
        // reconciler to rebind the opaque shadow group.
        if self.pending_atlas_grow {
            self.pending_atlas_grow = false;
            let new_size = (self.atlas_size.saturating_mul(2)).min(SHADOW_ATLAS_MAX_SIZE);
            if new_size > self.atlas_size {
                tracing::info!(
                    "shadow atlas overflow → growing from {} to {}",
                    self.atlas_size,
                    new_size
                );
                self.atlas_size = new_size;
                self.atlas_texture = gpu.create_texture(
                    &TextureDescriptor::new(
                        TextureFormat::Depth32float,
                        Extent3d::new(self.atlas_size, Some(self.atlas_size), Some(1)),
                        TextureUsage::new()
                            .with_render_attachment()
                            .with_texture_binding(),
                    )
                    .with_label("Shadow Atlas")
                    .into(),
                )?;
                self.atlas_view = self
                    .atlas_texture
                    .create_view()
                    .map_err(AwsmCoreError::create_texture_view)?;
                // The 2D atlas only carries spot-light depth now;
                // EVSM moment-write samples cascade depth from
                // `cascade_array_view` instead, so the auto-grow
                // doesn't need to rebuild that bind group.
                bind_groups
                    .mark_create(crate::bind_groups::BindGroupCreate::ShadowsResourcesChange);
                // Force the throttle to re-render every cascade at the
                // new atlas location.
                for entries in self.throttle.values_mut() {
                    for t in entries.iter_mut() {
                        t.last_rendered_frame = u64::MAX;
                    }
                }
                // `ShadowGlobals.atlas_sizes` is read by every PCF
                // tile clamp / PCSS kernel — re-upload so the shader
                // sees the new size on the next sample.
                self.dirty = true;
            } else {
                tracing::warn!(
                    "shadow atlas at max size {}, cannot grow further",
                    SHADOW_ATLAS_MAX_SIZE
                );
            }
        }

        if self.dirty {
            // Globals layout (matches WGSL `ShadowGlobals`).
            let mut data = [0u8; SHADOW_GLOBALS_BYTES];
            let atlas = self.atlas_size as f32;
            let evsm = self.config.evsm_atlas_size as f32;
            data[0..4].copy_from_slice(&atlas.to_ne_bytes());
            data[4..8].copy_from_slice(&atlas.to_ne_bytes());
            data[8..12].copy_from_slice(&evsm.to_ne_bytes());
            data[12..16].copy_from_slice(&evsm.to_ne_bytes());
            // Clamp `evsm_exponent` to the fp16-safe range — anything
            // above ~18 saturates the half-float moments and collapses
            // the Chebyshev visibility curve into a hard binary mask.
            let evsm_exponent = self
                .config
                .evsm_exponent
                .clamp(0.5, ShadowsConfig::EVSM_EXPONENT_MAX_FP16);
            data[16..20].copy_from_slice(&evsm_exponent.to_ne_bytes());
            data[20..24].copy_from_slice(&(self.config.evsm_blur_radius as f32).to_ne_bytes());
            data[24..28].copy_from_slice(&(self.config.sscs_step_count as f32).to_ne_bytes());
            data[28..32].copy_from_slice(&(self.config.sscs_enabled as u32 as f32).to_ne_bytes());
            data[32..36].copy_from_slice(&(self.config.debug_cascade_colors as u32).to_ne_bytes());
            data[36..40].copy_from_slice(&self.config.max_point_shadows.to_ne_bytes());
            // `flags.z` / `flags.w` are reserved padding (see the
            // `vec4<u32>` layout in `bind_groups.wgsl::ShadowGlobals`).
            // Left zeroed for std140 alignment; no live consumer.
            // cascade-array vec4: (layer.w, layer.h, max_layers, _).
            let cascade_size = self.cascade_resolution as f32;
            data[48..52].copy_from_slice(&cascade_size.to_ne_bytes());
            data[52..56].copy_from_slice(&cascade_size.to_ne_bytes());
            data[56..60].copy_from_slice(&(self.cascade_max_layers as f32).to_ne_bytes());
            data[60..64].copy_from_slice(&0.0_f32.to_ne_bytes());
            let n = data.len();
            self.globals_uploader.write_dirty_ranges(
                gpu,
                &self.globals_buffer,
                n,
                data.as_slice(),
                &[(0, n)],
            )?;
            self.dirty = false;
        }

        // Refit cascades for every casting directional light against
        // the current camera. If the camera hasn't been updated yet
        // (very first frame, before `update_camera`) we skip — the
        // next frame picks up.
        let Some(camera_matrices) = camera.last_matrices.as_ref() else {
            self.frame_count = self.frame_count.wrapping_add(1);
            return Ok(());
        };
        let _camera_inv_view_proj = camera_matrices.inv_view_projection();

        self.records.clear();
        self.active_descriptor_count = 0;
        self.active_view_count = 0;

        // Early-out when no light is currently casting. Skips the
        // O(meshes) caster-AABB sweep, descriptor pack, and throttle
        // reconciliation — the entire shadow generation pass is
        // also gated by `any_active()` upstream, so leaving stale
        // descriptors here is fine. We still tick `frame_count` so
        // throttle counters stay in step when shadows resume.
        if !self.params.values().any(|p| p.cast) {
            self.frame_count = self.frame_count.wrapping_add(1);
            return Ok(());
        }

        // Reuse the scratch staging buffers across frames. Zero the
        // descriptor scratch in full (gaps between active descriptors
        // must read as zero in the uniform), and only zero the view
        // scratch up to `MAX_SHADOW_VIEWS` slots that will actually
        // be written below.
        let descriptor_bytes = &mut self.descriptor_bytes_scratch[..];
        descriptor_bytes.fill(0);
        let view_bytes = &mut self.view_bytes_scratch[..];

        // Approximate the camera's near/far in world-space depth.
        // The actual values live on the camera but aren't exposed
        // directly here; reconstruct from the projection's column.
        // For a standard RH perspective with `Mat4::perspective_rh`
        // (which glam uses): proj[2][3] is `-2*near*far/(far-near)`
        // and proj[2][2] is `-(far+near)/(far-near)`; solving gives
        // the planes. Falls back to (0.1, 100.0) for orthographic.
        let (camera_near, camera_far) = extract_near_far(&camera_matrices.projection);

        // Per-mesh shadow-caster AABBs. `fit_cascades` clips each one
        // to the cascade's world footprint per-cascade, so we hand it
        // the full list rather than a precomputed union — a single
        // pre-unioned AABB would over-include casters that lie
        // laterally outside the cascade, ballooning the cascade's Z
        // range and destroying depth precision (the canonical failure
        // mode: a 10 km × 10 km ground plane whose union AABB stretches
        // thousands of metres along the tilted light direction).
        // Pull casters straight from the spatial index. Each leaf already
        // mirrors `mesh.world_aabb`; the shadow-caster `NodeFilter` enforces
        // the `cast_shadows && !hidden && !hud` predicate that the linear
        // walk used to apply by hand. Casters that haven't yet acquired a
        // world AABB (procedural / mid-load) aren't in the index — they're
        // still rendered conservatively by `shadow_render_pass::record`'s
        // tail-walk, but skipped for the cascade fit (nothing to clip
        // against).
        self.caster_aabbs_scratch.clear();
        for node in scene_spatial.iter_filtered(crate::scene_spatial::NodeFilter::shadow_caster()) {
            self.caster_aabbs_scratch.push(node.aabb.clone());
        }
        let caster_world_aabbs = self.caster_aabbs_scratch.as_slice();

        // Cursor for the row-pack atlas allocator. A future replacement
        // can swap in a real packer; for now we walk left-to-right
        // and wrap to the next row when the current row fills.
        let mut atlas_x: u32 = 0;
        let mut atlas_y: u32 = 0;
        // Layer cursor for the cascade-array. Each directional
        // cascade consumes one layer in iteration order — the order
        // is stable across frames as long as the `params` set is
        // unchanged, which is what the throttle relies on.
        let mut cascade_layer_cursor: u32 = 0;
        let cascade_max_layers = self.cascade_max_layers;
        let cascade_layer_size = self.cascade_resolution;
        // EVSM atlas allocator cursors (separate from PCF). Local for
        // the duration of the cascade-placement loop; final state
        // doesn't need to persist on `self`.
        let mut evsm_x: u32 = 0;
        let mut evsm_y: u32 = 0;
        let mut evsm_row_h: u32 = 0;
        let evsm_atlas_size = self.evsm_atlas_size;
        self.evsm_dispatch_queue.clear();
        self.evsm_pass.active_cascade_count = 0;
        let mut row_height: u32 = 0;
        // Reset cube slot ownership for lights that no longer cast.
        // The match loop below re-claims slots for surviving casters.
        for slot in self.cube_slots.iter_mut() {
            if let Some(key) = *slot {
                if !self.params.get(key).map(|p| p.cast).unwrap_or(false) {
                    *slot = None;
                }
            }
        }
        let mut cube_overflow = false;
        let mut place = |w: u32, h: u32, atlas_size: u32| -> Option<[u32; 4]> {
            if atlas_x + w > atlas_size {
                atlas_x = 0;
                atlas_y += row_height;
                row_height = 0;
            }
            if atlas_y + h > atlas_size {
                return None;
            }
            let rect = [atlas_x, atlas_y, w, h];
            atlas_x += w;
            row_height = row_height.max(h);
            Some(rect)
        };

        for (light_key, params) in self.params.iter() {
            if !params.cast {
                continue;
            }
            let Some(light) = lights.get(light_key) else {
                continue;
            };

            match light {
                crate::lights::Light::Directional { direction, .. } => {
                    let cascade_count = params.cascade_count.clamp(1, 4) as u32;
                    // Reserve `cascade_count` descriptors + `cascade_count`
                    // views (one view per cascade). Atlas allocation
                    // can still fail mid-loop per cascade, in which
                    // case we commit only the landed prefix.
                    let Some(alloc) = ShadowAlloc::try_new(
                        self.active_descriptor_count,
                        self.active_view_count,
                        cascade_count,
                        cascade_count,
                        MAX_SHADOW_DESCRIPTORS,
                        MAX_SHADOW_VIEWS,
                    ) else {
                        tracing::warn!(
                            "shadow descriptor / view budget exhausted (directional needs {})",
                            cascade_count
                        );
                        continue;
                    };
                    let descriptor_base = alloc.descriptor_base;
                    let dir = glam::Vec3::from(*direction);
                    let cascades = cascade::fit_cascades(
                        camera_matrices.view_projection(),
                        camera_matrices.view,
                        if dir.length_squared() > 1e-8 {
                            dir.normalize()
                        } else {
                            glam::Vec3::new(0.3, -1.0, 0.3).normalize()
                        },
                        camera_near.max(0.01),
                        // `max_distance <= 0` = AUTO: follow the camera far
                        // plane. A fixed default (the old 100.0) silently
                        // dropped every shadow beyond 100 UNITS of the camera
                        // — sane for meter-scale worlds, but a cm-scale
                        // import (e.g. the glTF sample Fox) lost shadows a
                        // meter out.
                        if params.max_distance > 0.0 {
                            camera_far.min(params.max_distance).max(camera_near + 1.0)
                        } else {
                            camera_far.max(camera_near + 1.0)
                        },
                        cascade_count,
                        params.cascade_split_lambda.clamp(0.0, 1.0),
                        params.resolution.max(16),
                        16,
                        caster_world_aabbs,
                    );

                    let mut landed: u32 = 0;
                    let mut views = Vec::with_capacity(cascades.len());
                    let evsm_first = match params.evsm_cutoff {
                        EvsmCutoff::Off => u32::MAX,
                        EvsmCutoff::LastCascade => cascade_count.saturating_sub(1),
                        EvsmCutoff::LastTwoCascades => cascade_count.saturating_sub(2),
                    };
                    for (cascade_index, (cascade, res, split_far)) in cascades.iter().enumerate() {
                        if cascade_layer_cursor >= cascade_max_layers {
                            if !self.warned_evsm_overflow {
                                self.warned_evsm_overflow = true;
                                tracing::warn!(
                                    "cascade-array layers exhausted (capacity {}) — cascade {} dropped                                      (raise ShadowsConfig::cascade_array_max_layers)",
                                    cascade_max_layers,
                                    cascade_index,
                                );
                            }
                            break;
                        }
                        let cascade_layer = cascade_layer_cursor;
                        cascade_layer_cursor += 1;
                        // Per-cascade resolution is the layer size (the
                        // cascade always fills its layer top-left). The
                        // texture-array forces a uniform layer size, so
                        // a per-light `params.resolution` larger than
                        // the layer is silently clamped. A
                        // `params.resolution` smaller than the layer
                        // would waste the bottom-right of the layer —
                        // we keep the layer-size resolution for
                        // simplicity. The plan's "uniform per
                        // directional light" assumption already holds:
                        // `cascade::cascade_resolution` returns the
                        // same value for every cascade index within a
                        // single light.
                        let used_res = (*res).min(cascade_layer_size);

                        let descriptor_index = descriptor_base + landed;
                        let off = descriptor_index as usize * SHADOW_DESCRIPTOR_BYTES;
                        let is_evsm = (cascade_index as u32) >= evsm_first;
                        // EVSM cascade: the receiver samples post-blur
                        // moments from `evsm_atlas` (so the
                        // *descriptor* carries an EVSM atlas rect), but
                        // the depth pass still writes into the cascade
                        // layer. `EvsmDispatchEntry.cascade_layer`
                        // lets the moment-write compute sample the
                        // right layer.
                        //
                        // If EVSM atlas allocation overflows, the
                        // cascade silently degrades to cascade-array
                        // PCF: descriptor stays kind = 3, no compute
                        // dispatch is queued.
                        let evsm_rect_alloc = if is_evsm {
                            // Inline row-pack on the EVSM atlas, same
                            // shape as before. Returns None on
                            // overflow → cascade degrades to PCF.
                            let r = used_res;
                            if evsm_x + r > evsm_atlas_size {
                                evsm_x = 0;
                                evsm_y += evsm_row_h;
                                evsm_row_h = 0;
                            }
                            if evsm_y + r > evsm_atlas_size {
                                // Queue an atlas grow (handled at the top of
                                // the next write_gpu); warn ONCE per episode,
                                // and only when no further growth is possible
                                // — transient pre-grow frames degrade this
                                // cascade to PCF silently.
                                self.pending_evsm_grow = true;
                                if !self.warned_evsm_overflow
                                    && evsm_atlas_size >= SHADOW_ATLAS_MAX_SIZE
                                {
                                    self.warned_evsm_overflow = true;
                                    tracing::warn!(
                                        "EVSM atlas at max size {} cannot fit cascade res={} — falling back to PCF",
                                        evsm_atlas_size,
                                        r
                                    );
                                }
                                None
                            } else {
                                let rect = [evsm_x, evsm_y, r, r];
                                evsm_x += r;
                                evsm_row_h = evsm_row_h.max(r);
                                Some(rect)
                            }
                        } else {
                            None
                        };
                        if let Some(evsm_rect) = evsm_rect_alloc {
                            // EVSM descriptor: sample-side reads moments
                            // from `evsm_atlas`, so the descriptor's
                            // atlas_rect carries the EVSM tile UV.
                            write_shadow_descriptor(
                                &mut descriptor_bytes[off..off + SHADOW_DESCRIPTOR_BYTES],
                                &cascade.view_projection,
                                evsm_rect,
                                self.evsm_atlas_size,
                                params.depth_bias,
                                params.normal_bias,
                                params.hardness,
                                params.pcss_penumbra_scale,
                                cascade.world_per_texel,
                                cascade_count,
                                *split_far,
                            );
                            // cascade_info.w = 1.0 → EVSM 2D sample.
                            descriptor_bytes[off + 108..off + 112]
                                .copy_from_slice(&1.0_f32.to_ne_bytes());
                            let slot = self.evsm_pass.active_cascade_count as usize;
                            if slot < evsm::MAX_EVSM_CASCADES_PER_FRAME {
                                let evsm_exponent = self
                                    .config
                                    .evsm_exponent
                                    .clamp(0.5, ShadowsConfig::EVSM_EXPONENT_MAX_FP16);
                                // Source rect on the cascade-array
                                // layer: (0, 0, used_res, used_res) —
                                // the cascade always fills the top-
                                // left of its layer.
                                self.evsm_pass.write_params_slot(
                                    slot,
                                    [0, 0],
                                    [used_res, used_res],
                                    [evsm_rect[0], evsm_rect[1]],
                                    [evsm_rect[2], evsm_rect[3]],
                                    evsm_exponent,
                                    self.config.evsm_blur_radius,
                                    cascade_layer,
                                );
                                self.evsm_dispatch_queue.push(EvsmDispatchEntry {
                                    descriptor_index,
                                    pcf_rect: [0, 0, used_res, used_res],
                                    evsm_rect,
                                    params_slot: slot as u32,
                                    cascade_layer,
                                    // Set definitively by the throttle
                                    // reconciliation pass below — start
                                    // true so a queue without any
                                    // throttling still dispatches.
                                    should_render: true,
                                });
                                self.evsm_pass.active_cascade_count += 1;
                            }
                        } else {
                            // Cascade-array PCF descriptor (kind = 3).
                            write_shadow_cascade_array_descriptor(
                                &mut descriptor_bytes[off..off + SHADOW_DESCRIPTOR_BYTES],
                                &cascade.view_projection,
                                cascade_layer,
                                used_res,
                                cascade_layer_size,
                                params.depth_bias,
                                params.normal_bias,
                                params.hardness,
                                params.pcss_penumbra_scale,
                                cascade.world_per_texel,
                                cascade_count,
                                *split_far,
                            );
                        }

                        // Throttle only the FAR cascade. Closer
                        // cascades carry per-frame contact detail and
                        // must refresh every frame.
                        let update_period =
                            if (cascade_index as u32) == cascade_count.saturating_sub(1) {
                                params.far_cascade_update_rate.period()
                            } else {
                                1
                            };
                        let view_slot = alloc.view_base + landed;
                        write_shadow_view_slot(
                            &mut *view_bytes,
                            view_slot as usize,
                            &cascade.view_projection,
                            params.depth_bias,
                            params.normal_bias,
                        );
                        views.push(LightShadowView {
                            view_projection: cascade.view_projection,
                            // Render attachment is the per-layer view;
                            // the viewport is (0, 0, used_res, used_res).
                            atlas_rect: [0, 0, used_res, used_res],
                            cube_layer: None,
                            cascade_layer: Some(cascade_layer),
                            update_period,
                            should_render: true,
                            shadow_view_slot: view_slot,
                        });
                        landed += 1;
                    }

                    if landed > 0 {
                        // Atlas overflow can cut the cascade loop
                        // short — `write_shadow_descriptor` was called
                        // per-cascade with the *requested* count, so
                        // each landed descriptor advertises
                        // `cascade_count`. Patch the
                        // cascade-count-in-light field (byte offset
                        // 104..108 in each 112-byte descriptor) to
                        // the actual landed prefix so the receiver's
                        // `sample_shadow_directional` walk doesn't
                        // stride past the published end into unwritten
                        // descriptor slots.
                        if landed != cascade_count {
                            tracing::warn!(
                                "directional shadow truncated: requested {} cascades, landed {}",
                                cascade_count,
                                landed
                            );
                            let landed_f = (landed as f32).to_ne_bytes();
                            for i in 0..landed {
                                let off = (descriptor_base + i) as usize * SHADOW_DESCRIPTOR_BYTES;
                                descriptor_bytes[off + 104..off + 108].copy_from_slice(&landed_f);
                            }
                        }
                        // Inline `commit_shadow_alloc` — `descriptor_bytes`
                        // / `view_bytes` hold an outstanding mut-borrow
                        // of `self.*_scratch`, so we can't call a
                        // `&mut self` method here. The two writes
                        // below are exactly what `commit_shadow_alloc`
                        // does; split-borrow lets them through.
                        debug_assert!(landed <= alloc.reserved_descriptors);
                        debug_assert!(landed <= alloc.reserved_views);
                        self.active_descriptor_count = alloc.descriptor_base + landed;
                        self.active_view_count = alloc.view_base + landed;
                        self.records.insert(
                            light_key,
                            LightShadowRecord {
                                views,
                                descriptor_base,
                            },
                        );
                    }
                    // else: alloc dropped without commit — counters
                    // didn't advance, the next light's `try_alloc_shadow`
                    // returns the same `descriptor_base` / `view_base`
                    // and overwrites any orphan bytes.
                }
                crate::lights::Light::Spot {
                    position,
                    direction,
                    range,
                    intensity,
                    outer_angle,
                    ..
                } => {
                    let Some(alloc) = ShadowAlloc::try_new(
                        self.active_descriptor_count,
                        self.active_view_count,
                        1,
                        1,
                        MAX_SHADOW_DESCRIPTORS,
                        MAX_SHADOW_VIEWS,
                    ) else {
                        tracing::warn!("shadow descriptor / view budget exhausted (spot)");
                        continue;
                    };
                    let res = params.resolution.max(16);
                    let Some(rect) = place(res, res, self.atlas_size) else {
                        tracing::warn!(
                            "shadow atlas overflow on spot light — will grow next frame"
                        );
                        self.pending_atlas_grow = true;
                        continue;
                    };
                    let pos = glam::Vec3::from(*position);
                    let dir_v = glam::Vec3::from(*direction);
                    let dir = if dir_v.length_squared() > 1e-8 {
                        dir_v.normalize()
                    } else {
                        glam::Vec3::new(0.0, -1.0, 0.0)
                    };
                    let up = if dir.x.abs() < 0.9 {
                        glam::Vec3::X
                    } else {
                        glam::Vec3::Z
                    };
                    let view = glam::Mat4::look_at_rh(pos, pos + dir, up);
                    let fov = (*outer_angle * 2.0).clamp(0.01, std::f32::consts::PI - 0.01);
                    // Same fix as the point/cube path: an unlimited-range
                    // (`range <= 0`) spot derives its reach from intensity
                    // so the projection far plane actually covers the lit
                    // cone. Raw `range` collapsed `far` to ~0.1 m, clipping
                    // every receiver out of the shadow frustum → no shadow.
                    let eff_range = crate::lights::Light::influence_radius(*intensity, *range);
                    let near = 0.05_f32.min(eff_range * 0.01).max(0.005);
                    let far = eff_range.max(near + 0.1);
                    let projection = glam::Mat4::perspective_rh(fov, 1.0, near, far);
                    let view_projection = projection * view;
                    // Approximate world-per-texel for the spot cone at
                    // its far plane: the perspective frustum's footprint
                    // there is `2 * far * tan(fov/2)`. Used by the PCF
                    // path to keep penumbra width consistent with
                    // directional cascades.
                    let spot_world_per_texel = 2.0 * far * (fov * 0.5).tan() / res as f32;

                    let descriptor_index = alloc.descriptor_base;
                    let off = descriptor_index as usize * SHADOW_DESCRIPTOR_BYTES;
                    // Scale the authored `pcss_penumbra_scale` by
                    // `tan(outer_angle * 0.5)` before baking it into
                    // the descriptor. Without this, a wider spot cone
                    // with the same authored scale gives a *narrower*
                    // perceived penumbra (the PCSS disc radius is
                    // measured in shadow-space NDC and the wider cone
                    // spreads the disc across more world). Multiplying
                    // by `tan(half_cone)` keeps the world-
                    // space penumbra width invariant to the cone angle.
                    let spot_pcss_penumbra_scale =
                        params.pcss_penumbra_scale * (outer_angle * 0.5).tan();
                    write_shadow_descriptor(
                        &mut descriptor_bytes[off..off + SHADOW_DESCRIPTOR_BYTES],
                        &view_projection,
                        rect,
                        self.atlas_size,
                        params.depth_bias,
                        params.normal_bias,
                        params.hardness,
                        spot_pcss_penumbra_scale,
                        spot_world_per_texel,
                        1,
                        // Spot lights don't use cascade selection; setting
                        // `split_far` to +infinity-ish makes the shader's
                        // walk pick this descriptor unconditionally.
                        f32::MAX,
                    );

                    let view_slot = alloc.view_base;
                    write_shadow_view_slot(
                        &mut *view_bytes,
                        view_slot as usize,
                        &view_projection,
                        params.depth_bias,
                        params.normal_bias,
                    );
                    // See directional branch — inlined commit because
                    // `descriptor_bytes` / `view_bytes` hold mut-borrows
                    // of self.*_scratch.
                    self.active_descriptor_count = alloc.descriptor_base + 1;
                    self.active_view_count = alloc.view_base + 1;
                    self.records.insert(
                        light_key,
                        LightShadowRecord {
                            views: vec![{
                                LightShadowView {
                                    view_projection,
                                    atlas_rect: rect,
                                    cube_layer: None,
                                    cascade_layer: None,
                                    update_period: 1,
                                    should_render: true,
                                    shadow_view_slot: view_slot,
                                }
                            }],
                            descriptor_base: descriptor_index,
                        },
                    );
                }
                crate::lights::Light::Point {
                    position,
                    range,
                    intensity,
                    ..
                } => {
                    // Point lights need 1 descriptor + 6 view slots
                    // (cube faces). All-or-nothing: partial publish
                    // would leave the receiver sampling a stale cube
                    // layer for the missing face's major axis.
                    let Some(alloc) = ShadowAlloc::try_new(
                        self.active_descriptor_count,
                        self.active_view_count,
                        1,
                        6,
                        MAX_SHADOW_DESCRIPTORS,
                        MAX_SHADOW_VIEWS,
                    ) else {
                        tracing::warn!(
                            "shadow descriptor / view budget exhausted (point needs 1 + 6)"
                        );
                        continue;
                    };
                    // O(1) ownership lookup via `cube_slot_for_light`,
                    // validated against `cube_slots` (a stale entry from
                    // a previous-pool reassignment falls back to the
                    // linear free-slot search).
                    let cached = self.cube_slot_for_light.get(light_key).copied();
                    let owned = cached.and_then(|idx| {
                        let i = idx as usize;
                        if self.cube_slots.get(i).and_then(|s| *s) == Some(light_key) {
                            Some(i)
                        } else {
                            None
                        }
                    });
                    let slot = owned.or_else(|| self.cube_slots.iter().position(|s| s.is_none()));
                    let Some(slot_index) = slot else {
                        cube_overflow = true;
                        continue;
                    };
                    self.cube_slots[slot_index] = Some(light_key);
                    self.cube_slot_for_light
                        .insert(light_key, slot_index as u32);

                    let pos = glam::Vec3::from(*position);
                    // Match the lighting/culling reach: an unlimited-range
                    // (`range <= 0`) point light derives its radius from
                    // intensity via `influence_radius` — the same value the
                    // GPU light buffer and culling AABB use. Using the raw
                    // `range` here collapsed the cube far plane + descriptor
                    // range to 0.05 m for infinite lights, so the cube
                    // sampler short-circuited every receiver >5 cm away to
                    // "fully lit" → no shadow at all.
                    let r = crate::lights::Light::influence_radius(*intensity, *range).max(0.05);
                    // 90° per face — adjacent faces meet exactly at the
                    // cube edge and the seamless-cubemap filter handles
                    // bilinear comparison across the seam.
                    let cube_fov = std::f32::consts::FRAC_PI_2;
                    // WebGPU cube sampling (D3D convention): on the +X
                    // face, texel t=0 maps to direction +Y, etc. A
                    // plain `look_at_rh(... up=-Y) * perspective_rh` —
                    // the OpenGL-style cube convention — writes world
                    // +Y to the *bottom* of the rendered face because
                    // WebGPU's framebuffer is top-left-origin while
                    // NDC.y is bottom-up. The mismatch shows up at
                    // sample time as a V-flipped read, which on a
                    // sphere of receivers manifests as a "double" or
                    // "phantom" shadow across the seam between
                    // adjacent faces. Post-multiplying the projection
                    // by a Y-flip negates NDC.y so world +Y lands at
                    // texel t=0; the matching `front_face = Cw` in the
                    // cube shadow pipeline restores winding (and
                    // therefore front-face culling).
                    let y_flip = glam::Mat4::from_scale(glam::Vec3::new(1.0, -1.0, 1.0));
                    let projection =
                        y_flip * glam::Mat4::perspective_rh(cube_fov, 1.0, POINT_SHADOW_NEAR, r);
                    // glTF cube-map face conventions, in the order
                    // WebGPU lays out cube layers: +X, -X, +Y, -Y, +Z, -Z.
                    let face_dirs = [
                        (glam::Vec3::X, -glam::Vec3::Y),
                        (-glam::Vec3::X, -glam::Vec3::Y),
                        (glam::Vec3::Y, glam::Vec3::Z),
                        (-glam::Vec3::Y, -glam::Vec3::Z),
                        (glam::Vec3::Z, -glam::Vec3::Y),
                        (-glam::Vec3::Z, -glam::Vec3::Y),
                    ];

                    let descriptor_base = alloc.descriptor_base;
                    let mut views: Vec<LightShadowView> = Vec::with_capacity(6);
                    // Per-face throttle period. Default `EveryFrame`
                    // (period = 1) preserves the previous behaviour;
                    // higher periods are a mobile / many-light perf
                    // lever — the throttle in this same `write_gpu`
                    // call already handles per-face cadence and forces
                    // a redraw whenever the light or its descriptor
                    // moves enough to invalidate the cache.
                    let cube_update_period = params.cube_face_update_rate.period();
                    // `try_alloc_shadow(1, 6)` above guaranteed the
                    // 6 view slots are available, so no per-face
                    // budget check is needed inside the loop.
                    for (face_idx, (dir, up)) in face_dirs.iter().enumerate() {
                        let view = glam::Mat4::look_at_rh(pos, pos + *dir, *up);
                        let vp = projection * view;
                        let view_slot = alloc.view_base + face_idx as u32;
                        write_shadow_view_slot(
                            &mut *view_bytes,
                            view_slot as usize,
                            &vp,
                            params.depth_bias,
                            params.normal_bias,
                        );
                        views.push(LightShadowView {
                            view_projection: vp,
                            // For cube faces the attachment is already the
                            // per-face 2D view at the cube's native
                            // resolution, so this rect doubles as the
                            // render-pass viewport — it must match
                            // `self.cube_resolution`, not the
                            // initialization-time `POINT_SHADOW_RESOLUTION`
                            // default, or a config change would render
                            // into a sub-rect of the new texture.
                            atlas_rect: [0, 0, self.cube_resolution, self.cube_resolution],
                            cube_layer: Some(slot_index as u32 * 6 + face_idx as u32),
                            cascade_layer: None,
                            update_period: cube_update_period,
                            should_render: true,
                            shadow_view_slot: view_slot,
                        });
                    }

                    // Only one descriptor per point light. Sample-site
                    // uses world-space direction to pick the face.
                    let descriptor_index = alloc.descriptor_base;
                    let off = descriptor_index as usize * SHADOW_DESCRIPTOR_BYTES;
                    write_shadow_descriptor(
                        &mut descriptor_bytes[off..off + SHADOW_DESCRIPTOR_BYTES],
                        // view_projection unused for cube; zero is fine.
                        &glam::Mat4::ZERO,
                        // Repurpose atlas_rect for (light_pos.xyz, range)
                        // — packed at the same byte offsets so the
                        // shader can pull them straight from the same
                        // vec4 it'd otherwise use for UV math.
                        [0, 0, 0, 0],
                        self.atlas_size,
                        params.depth_bias,
                        params.normal_bias,
                        params.hardness,
                        params.pcss_penumbra_scale,
                        // Caller patches cascade_info.y with the slot
                        // index after this returns — see below.
                        0.0,
                        1,
                        f32::MAX,
                    );
                    // Patch in the cube-specific atlas_rect (light_pos +
                    // range) and the "kind = cube + slice index" in
                    // `cascade_info.w / .y`.
                    descriptor_bytes[off + 64..off + 68].copy_from_slice(&pos.x.to_ne_bytes());
                    descriptor_bytes[off + 68..off + 72].copy_from_slice(&pos.y.to_ne_bytes());
                    descriptor_bytes[off + 72..off + 76].copy_from_slice(&pos.z.to_ne_bytes());
                    descriptor_bytes[off + 76..off + 80].copy_from_slice(&r.to_ne_bytes());
                    // cascade_info.x = kernel slack (point-only; this slot
                    // otherwise carries split_far, which the cube sampler
                    // never reads). Soft/PCSS taps fold it into the
                    // receiver-plane comparison bias to kill acne rings.
                    descriptor_bytes[off + 96..off + 100]
                        .copy_from_slice(&params.kernel_slack.to_ne_bytes());
                    // cascade_info.y = slot index (as f32)
                    descriptor_bytes[off + 100..off + 104]
                        .copy_from_slice(&(slot_index as f32).to_ne_bytes());
                    // cascade_info.w = 2.0 → cube
                    descriptor_bytes[off + 108..off + 112].copy_from_slice(&2.0_f32.to_ne_bytes());

                    // Inlined commit (see directional branch).
                    self.active_descriptor_count = alloc.descriptor_base + 1;
                    self.active_view_count = alloc.view_base + 6;
                    self.records.insert(
                        light_key,
                        LightShadowRecord {
                            views,
                            descriptor_base,
                        },
                    );
                }
            }
        }

        if self.active_descriptor_count > 0 {
            // Upload only the active prefix. The shader iterates
            // `descriptor_base..base+count` so trailing slots never
            // get read; the uniform buffer's tail keeps whatever it
            // held last frame (harmless — those slots are not bound
            // as descriptor indices anywhere).
            let used = self.active_descriptor_count as usize * SHADOW_DESCRIPTOR_BYTES;
            // Dest buffer is fixed-size at MAX_SHADOW_DESCRIPTORS;
            // pass the full descriptor-uniform byte size as dest_size
            // so the ring slot matches.
            self.descriptors_uploader.write_dirty_ranges(
                gpu,
                &self.descriptors_uniform,
                *SHADOW_DESCRIPTOR_UNIFORM_BYTES,
                &descriptor_bytes[..used],
                &[(0, used)],
            )?;
        }
        if self.active_view_count > 0 {
            // Upload the per-view matrices once. The render pass uses
            // dynamic offsets into this buffer to select per-pass
            // matrices — a single `writeBuffer` call here is critical:
            // queue.writeBuffer flushes all queued writes BEFORE any
            // command buffer executes, so if we wrote per-pass we'd
            // see only the last matrix in every pass.
            let used = self.active_view_count as usize * SHADOW_VIEW_STRIDE;
            let buffer_size = SHADOW_VIEW_STRIDE * MAX_SHADOW_VIEWS as usize;
            self.views_uploader.write_dirty_ranges(
                gpu,
                &self.shadow_view_buffer,
                buffer_size,
                &view_bytes[..used],
                &[(0, used)],
            )?;
        }

        // Reconcile throttle state with the freshly-built records.
        // Lights that vanished from the caster set drop their state;
        // views whose atlas rect moved get invalidated (the cached
        // depth is at the wrong location); the view-projection drift
        // check forces a redraw when the camera or light moved enough
        // to make the cached cascade visibly stale.
        // Drop throttle entries for lights that no longer have a
        // record this frame. `retain` is allocation-free; the
        // earlier `Vec<LightKey>` sweep + `contains()` was O(n²).
        self.throttle.retain(|k, _| self.records.contains_key(k));
        let frame = self.frame_count;
        for (light_key, record) in self.records.iter_mut() {
            if !self.throttle.contains_key(light_key) {
                self.throttle.insert(light_key, Vec::new());
            }
            let entry = &mut self.throttle[light_key];
            entry.resize(
                record.views.len(),
                ShadowViewThrottle {
                    last_rendered_frame: u64::MAX,
                    last_view_projection: Mat4::ZERO,
                    last_atlas_rect: [0; 4],
                    last_cascade_layer: None,
                },
            );
            for (i, view) in record.views.iter_mut().enumerate() {
                let t = &mut entry[i];
                if t.last_atlas_rect != view.atlas_rect {
                    t.last_rendered_frame = u64::MAX;
                }
                if t.last_cascade_layer != view.cascade_layer {
                    t.last_rendered_frame = u64::MAX;
                }
                let drift = view_projection_drift(&t.last_view_projection, &view.view_projection);
                if drift > 0.001 {
                    t.last_rendered_frame = u64::MAX;
                }
                // §B: split forced vs periodic. `forced` (rect / layer / drift /
                // config invalidation → `u64::MAX`) ALWAYS fires — a moved camera or
                // light drifts the view-projection, so those are covered here for
                // free. `periodic` is the throttle cadence (near cascades + cube
                // faces have `update_period == 1` → due every frame today). We
                // suppress ONLY the periodic component when the frame is provably
                // static (`shadow_static`): no caster moved, camera still, caster
                // set unchanged, no time-driven shadow material. The cached
                // per-attachment depth stays valid because each view clears
                // independently.
                let forced = t.last_rendered_frame == u64::MAX;
                let periodic = frame >= t.last_rendered_frame.saturating_add(view.update_period);
                let due = forced || (periodic && !shadow_static);
                // Per-attachment views — cube faces and cascade-array
                // layers — clear independently, so throttling them is
                // safe (the previous frame's depth is still intact).
                // Spot lights still share the 2D `shadow_atlas`, where
                // any cleared pass wipes the whole attachment; so spot
                // views are forced to render every frame until a
                // per-tile clear lands. `update_period` for spot views
                // is hard-coded to 1 above, so this just reasserts.
                let has_own_attachment = view.cube_layer.is_some() || view.cascade_layer.is_some();
                view.should_render = due || !has_own_attachment;
                if view.should_render {
                    t.last_rendered_frame = frame;
                    t.last_view_projection = view.view_projection;
                    t.last_atlas_rect = view.atlas_rect;
                    t.last_cascade_layer = view.cascade_layer;
                }
            }
        }

        if cube_overflow {
            tracing::warn!(
                "point-light shadow cube pool exhausted (capacity {})",
                self.cube_slots.len()
            );
        }

        // Propagate per-cascade throttle decisions into the EVSM
        // queue: a cascade that didn't re-render this frame keeps its
        // depth (and therefore its moments) from the previous frame,
        // so the moment-write + blur dispatches are wasted work.
        // Match queue entries to views by `cascade_layer` — the
        // layer cursor is monotonic per frame, so the mapping is
        // unique.
        for entry in self.evsm_dispatch_queue.iter_mut() {
            let mut should_render = true;
            'outer: for record in self.records.values() {
                for view in &record.views {
                    if view.cascade_layer == Some(entry.cascade_layer) {
                        should_render = view.should_render;
                        break 'outer;
                    }
                }
            }
            entry.should_render = should_render;
        }

        // Flush EVSM per-cascade params to the GPU. One write covers
        // every active cascade; the compute-pass loop in
        // `render_pass::record` binds slot N via dynamic offset.
        self.evsm_pass.upload_params(gpu)?;

        self.frame_count = self.frame_count.wrapping_add(1);

        // Descriptor / view bookkeeping invariants. The
        // per-frame `active_*_count` fields drive the uniform buffer
        // slices the shading passes bind via dynamic offset; if they
        // disagree with the per-light record list, the binding picks up
        // garbage data and the resulting visual artifact is impossible
        // to diagnose from the shader side. Catch the off-by-one here
        // so future allocator edits surface the regression immediately.
        //
        // Descriptors-per-record is *not* uniform across light kinds:
        //   - Directional: one descriptor per cascade ⇒ `views.len()`
        //   - Spot:        one descriptor, one view
        //   - Point:       one descriptor, six views (cube sampling
        //                  uses the same descriptor for all 6 faces)
        //
        // We tell point apart by `views[*].cube_layer.is_some()`.
        #[cfg(debug_assertions)]
        {
            let view_sum: usize = self.records.values().map(|r| r.views.len()).sum();
            debug_assert_eq!(
                view_sum, self.active_view_count as usize,
                "shadow view bookkeeping diverged: records sum to {view_sum} views, \
                 active_view_count = {}",
                self.active_view_count,
            );
            let descriptor_sum: usize = self
                .records
                .values()
                .map(|r| {
                    if r.views.iter().any(|v| v.cube_layer.is_some()) {
                        1 // cube/point: one descriptor for all faces
                    } else {
                        r.views.len() // directional cascades / spot
                    }
                })
                .sum();
            debug_assert_eq!(
                descriptor_sum,
                self.active_descriptor_count as usize,
                "shadow descriptor bookkeeping diverged: records sum to {descriptor_sum} descriptors, \
                 active_descriptor_count = {}",
                self.active_descriptor_count,
            );
        }

        Ok(())
    }

    /// Dynamic-offset argument for the shadow_view bind group at
    /// `view_global_index`. The buffer is laid out with
    /// `SHADOW_VIEW_STRIDE`-byte slots so offsets are
    /// `min-uniform-buffer-offset-alignment` compatible.
    pub fn shadow_view_dynamic_offset(view_global_index: u32) -> u32 {
        view_global_index * SHADOW_VIEW_STRIDE as u32
    }

    /// Iterates all per-frame caster records — used by the render
    /// pass loop to know which views to draw.
    pub fn records(&self) -> impl Iterator<Item = (LightKey, &LightShadowRecord)> + '_ {
        self.records.iter()
    }

    /// Returns the per-light authored shadow params, if registered.
    pub fn light_params(&self, key: LightKey) -> Option<&LightShadowParams> {
        self.params.get(key)
    }

    /// Drop every per-light shadow row in one shot. Paired with the
    /// `AwsmRenderer::clear_lights` mass-removal entry point so the
    /// shadow side stays in lockstep when callers blow away the
    /// lights set.
    pub(super) fn clear_all_lights(&mut self) {
        self.params.clear();
        self.throttle.clear();
        self.records.clear();
        self.cube_slot_for_light.clear();
        for slot in self.cube_slots.iter_mut() {
            *slot = None;
        }
    }

    /// Cleans every piece of shadow state keyed on `key`. Call this
    /// from `AwsmRenderer::remove_light` (the public entry point) —
    /// never call `Lights::remove` directly, or the shadow side
    /// leaks: the cube-pool slot stays "owned" by a dead key (
    /// `cube_slots[i] = Some(key)`), the throttle entry persists, and
    /// `params` keeps a `cast = true` row that makes
    /// `caster_count`/`any_active` lie and forces a per-frame
    /// caster-AABB sweep for a nonexistent caster.
    pub(super) fn on_light_removed(&mut self, key: LightKey) {
        self.params.remove(key);
        self.throttle.remove(key);
        self.records.remove(key);
        if let Some(idx) = self.cube_slot_for_light.remove(key) {
            if let Some(slot) = self.cube_slots.get_mut(idx as usize) {
                if *slot == Some(key) {
                    *slot = None;
                }
            }
        }
    }
}

/// Cheap config-time sanity check on the per-frame view-slot budget.
/// Logs a warning when `max_point_shadows * 6` leaves no headroom for
/// directional cascades or spot lights — point lights consume 6 view
/// slots each, and the runtime gracefully degrades on overflow (drops
/// the offending light's shadow for the frame), but a startup warning
/// beats discovering "why don't my N point lights cast shadows?" via
/// the dev console. Conservative threshold: warn if point allocation
/// alone leaves fewer than 8 slots free (room for ~2 directional
/// lights at 4 cascades each, or 8 spots).
fn warn_view_budget(config: &ShadowsConfig) {
    let point_views = config.max_point_shadows.saturating_mul(6);
    if point_views >= MAX_SHADOW_VIEWS {
        tracing::warn!(
            "ShadowsConfig.max_point_shadows = {} burns {} view slots — \
             the entire MAX_SHADOW_VIEWS = {} budget. Directional cascades \
             and spot lights will be silently dropped this frame. Lower \
             `max_point_shadows`, or raise `MAX_SHADOW_VIEWS` if you need this many.",
            config.max_point_shadows,
            point_views,
            MAX_SHADOW_VIEWS,
        );
    } else if point_views + 8 > MAX_SHADOW_VIEWS {
        tracing::warn!(
            "ShadowsConfig.max_point_shadows = {} reserves {} of {} view slots — \
             leaves {} for directional/spot. Mixing many point + several \
             directional cascades may exhaust the budget; the runtime degrades \
             safely, but consider lowering `max_point_shadows`.",
            config.max_point_shadows,
            point_views,
            MAX_SHADOW_VIEWS,
            MAX_SHADOW_VIEWS - point_views,
        );
    }
}
