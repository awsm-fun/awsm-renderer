//! High-level `AwsmRenderer` type, its builder, and core impls.
//! Module tree + crate attributes live in [`crate`] (lib.rs);
//! this file holds the logic. `use crate::*` brings the crate-root
//! module names + re-exports into scope so the original crate-root-
//! relative paths resolve unchanged.

use crate::*;
use std::sync::LazyLock;

use awsm_renderer_core::{
    brdf_lut::generate::{BrdfLut, BrdfLutOptions},
    command::color::Color,
    compatibility::CompatibilityRequirements,
    cubemap::images::CubemapBitmapColors,
    renderer::{AwsmRendererWebGpu, AwsmRendererWebGpuBuilder},
};
use bind_groups::BindGroups;
use camera::CameraBuffer;
use instances::Instances;
use light_buckets::LightMeshBuckets;
use lights::Lights;
use materials::Materials;
use meshes::Meshes;
use pipelines::Pipelines;
use scene_spatial::SceneSpatial;
use shaders::Shaders;
use textures::Textures;
use transforms::Transforms;

use crate::{
    anti_alias::AntiAliasing,
    bind_group_layout::BindGroupLayouts,
    debug::AwsmRendererLogging,
    environment::{Environment, Skybox},
    features::RendererFeatures,
    lights::ibl::{Ibl, IblTexture},
    meshes::MeshKey,
    picker::Picker,
    pipeline_layouts::PipelineLayouts,
    post_process::PostProcessing,
    render_passes::{lines::LineRenderer, RenderPassInitContext, RenderPasses},
    render_textures::{RenderTextureFormats, RenderTextures},
};

/// Per-frame state for the GPU coverage readback loop.
///
/// The renderer dispatches `coverage` after geometry, copies the
/// counts into a CPU-mappable buffer, and kicks a `mapAsync` that
/// resolves on a future frame. To keep the path single-buffered,
/// `inflight` short-circuits the next kick while a prior `mapAsync`
/// hasn't yet resolved; `pending_snapshot` carries the resolved
/// `(MeshKey, count)` pairs back to the next render frame which
/// calls `MeshCoverage::ingest`.
#[derive(Default)]
pub struct CoverageReadbackState {
    pub inflight: bool,
    pub pending_snapshot: Option<Vec<(MeshKey, u32)>>,
}

/// Per-frame state for the MSAA edge-budget overflow readback loop.
///
/// The render frame copies 8 bytes
/// (`edge_count`, `edge_overflow_count`) from
/// [`crate::render_passes::material_opaque::edge_buffers::MaterialEdgeBuffers::data_buffer`]
/// into a CPU-mappable buffer, then kicks `mapAsync`. When the read
/// resolves, the next frame's preamble inspects
/// `pending_overflow_count`: if > 0, the renderer calls
/// [`crate::AwsmRenderer::set_max_edge_budget`]`(current * 2)` so
/// subsequent frames have headroom. Single-buffered (`inflight`
/// gates the next kick) — under high mapping latency we lose one
/// frame's signal rather than ringing a buffer.
#[derive(Default)]
pub struct EdgeOverflowReadbackState {
    /// `true` while a `mapAsync` is in flight against
    /// `MaterialEdgeBuffers::overflow_readback_buffer`. Subsequent
    /// frames skip the copy + kick until the prior resolves.
    pub inflight: bool,
    /// Pending `(edge_count, edge_overflow_count)` snapshot from the
    /// most recently resolved `mapAsync`. Ingested at the top of the
    /// next render (set to `None` after reading).
    pub pending_overflow_count: Option<(u32, u32)>,
}

/// Mirror of [`EdgeOverflowReadbackState`] for the GPU light-culling
/// per-froxel capacity auto-grow loop. The cull shader atomic-adds
/// into `LightCullingBuffers::overflow_buffer` every time it bumps a
/// froxel's count past `max_per_froxel_capacity`; the host records a
/// `copy_buffer_to_buffer` into the per-frame command encoder, then
/// `mapAsync`'s the staging copy. When the resolved value is non-zero,
/// the next render preamble calls
/// [`crate::AwsmRenderer::set_max_per_froxel_capacity`]`(current * 2)`
/// so subsequent frames have headroom. Single-buffered (`inflight`
/// gates the next kick).
#[derive(Default)]
pub struct FroxelOverflowReadbackState {
    /// `true` while a `mapAsync` is in flight against
    /// `LightCullingBuffers::overflow_readback_buffer`. Subsequent
    /// frames skip the copy + kick until the prior resolves.
    pub inflight: bool,
    /// Pending `overflow_count` snapshot from the most recently
    /// resolved `mapAsync`. Ingested at the top of the next render
    /// (set to `None` after reading).
    pub pending_overflow_count: Option<u32>,
}

/// Main renderer state and GPU resources.
pub struct AwsmRenderer {
    pub gpu: core::renderer::AwsmRendererWebGpu,
    pub bind_group_layouts: BindGroupLayouts,
    pub bind_groups: BindGroups,
    pub meshes: Meshes,
    pub camera: CameraBuffer,
    /// Renderer-wide per-frame uniform — `time`, `delta_time`,
    /// `frame_count`, `resolution`. Updated once per `render()` call and
    /// bound alongside the camera uniform in every shader pass. See
    /// [`crate::frame_globals`] and [`AwsmRenderer::frame_globals`].
    pub frame_globals: crate::frame_globals::FrameGlobals,
    pub transforms: Transforms,
    pub instances: Instances,
    /// Renderer-owned spatial index over every mesh's world-space AABB.
    /// Mirrors `Mesh::world_aabb`. Drives camera-frustum culling,
    /// per-view shadow culling, and the per-mesh light-overlap query.
    pub scene_spatial: SceneSpatial,
    /// Per-light → per-mesh AABB-overlap buckets, rebuilt once per
    /// frame from `scene_spatial`. Feeds the per-mesh light-list shader
    /// path.
    pub light_buckets: LightMeshBuckets,
    /// Per-frame classify-pass output. Holds the per-`shader_id` tile
    /// buckets + indirect-dispatch args the opaque material pipelines
    /// consume.
    pub material_classify_buffers: render_passes::material_classify::buffers::ClassifyBuffers,
    /// `shader_id → bucket_index` lookup table (§4a) bound read-only into
    /// the classify pass — the O(1) replacement for the old per-pixel
    /// `shader_id == SHADER_ID_*` if/else chain. Rebuilt only when the
    /// bucket set changes (`relayout_bucket_buffers`), independent of the
    /// classify buckets (which realloc on viewport resize).
    pub material_bucket_lut: render_passes::material_classify::bucket_lut::MaterialBucketLut,
    /// GPU light-culling froxel buffers (per-frame params uniform +
    /// per-froxel counts + flat indices + overflow counter). Owned at
    /// the top level so the per-frame `ensure_viewport` / `write_params`
    /// / `reset_overflow` calls run before bind-group recreation.
    pub light_culling_buffers: render_passes::light_culling::LightCullingBuffers,
    /// Debug toggle (dev aid): when non-zero, the shading shaders output a
    /// per-pixel applied-punctual-light-count heatmap instead of normal
    /// shading. Written into `CullParams.debug_light_heatmap` each frame via
    /// `write_params`. Owned here (not on `LightCullingBuffers`) so it
    /// survives froxel-buffer recreation on resize / auto-grow.
    pub light_culling_debug_heatmap: u32,
    /// Global debug view mode: 0 = normal shading, 1 = unlit/flat (base color
    /// only). Written into `CullParams.debug_view_mode` each frame via
    /// `write_params`; no recompile. Owned here (survives froxel-buffer
    /// recreation). The shader branch that reads it is compiled only under the
    /// `debug-views` cargo feature; in a game build the value is written but
    /// never read.
    pub debug_view_mode: u32,
    /// Global debug wireframe overlay: 0 = off, 1 = on. Tints pixels near a
    /// triangle edge (barycentric distance) in the deferred shade. Written into
    /// `CullParams.debug_wireframe` each frame; no recompile. Read only by the
    /// `debug-views`-gated shader branch.
    pub debug_wireframe: u32,
    /// MSAA-edge-resolve buffers (Stage 3 / Priority 3 dispatch wiring).
    /// `None` when MSAA is off — there are no edges to resolve. When
    /// MSAA is on, holds the two split GPU buffers carrying:
    ///
    /// - **`args_buffer`** — atomic counters + per-shader indirect
    ///   dispatch args. Indirect + Storage + CopyDst usage.
    /// - **`data_buffer`** — `edge_to_xy` + `edge_slot_map` +
    ///   accumulator + per-shader/skybox sample lists. Storage +
    ///   CopyDst usage.
    ///
    /// Split so a single buffer is never simultaneously bound as
    /// Storage(read-write) and used as Indirect inside one compute
    /// pass (WebGPU rejects that combination). Reset per-frame via
    /// `MaterialEdgeBuffers::reset_header`. Resized when bucket count
    /// grows past current capacity.
    pub material_edge_buffers:
        Option<render_passes::material_opaque::edge_buffers::MaterialEdgeBuffers>,
    /// `EdgeBufferLayout` uniform companion to `material_edge_buffers`.
    /// Carries the u32-stride offsets the shaders use to slice into
    /// the data buffer. Same lifecycle: `None` until first MSAA boot;
    /// resized on bucket-count growth.
    pub material_edge_layout_uniform: Option<web_sys::GpuBuffer>,
    /// Projection-decal subsystem. Owns the per-decal GPU storage
    /// buffer the `material_decal` compute pass reads at shading time.
    /// `None` when `features.decals == false`.
    pub decals: Option<decals::Decals>,
    /// GPU occlusion-cull buffers. The per-frame instance list
    /// (CPU-populated) + the per-instance visibility output. `None`
    /// when `features.gpu_culling == false`.
    pub occlusion_buffers: Option<render_passes::occlusion::buffers::OcclusionBuffers>,
    /// Per-tile decal classify buckets. Populated by a `decal_classify`
    /// compute pass run before the decal shading pass; the shading pass
    /// reads only the per-tile subset. `None` when
    /// `features.decals == false`.
    pub decal_classify_buffers:
        Option<render_passes::material_decal::classify::buffers::DecalClassifyBuffers>,
    /// GPU compaction `IndirectDrawArgs` buffer. `None` when
    /// `features.gpu_culling == false`.
    pub compaction_buffers: Option<render_passes::occlusion::compaction::CompactionBuffers>,
    /// Last-frame per-mesh pixel coverage. Populated by the GPU
    /// coverage compute pass via `coverage_buffers` + asynchronous
    /// readback; consumed by skin-skip / material-LOD gates. The
    /// table itself is always present (it's CPU-only and tiny); when
    /// `features.coverage_lod == false` it just stays empty, which
    /// makes `is_below_threshold` return `false` for everything.
    pub coverage: coverage::MeshCoverage,
    /// GPU coverage producer buffers. The producer pass
    /// (`render_passes/coverage/`) atomic-adds per-pixel into
    /// `counts_buffer`; the renderer copies to `readback_buffer`
    /// each frame and a `mapAsync` resolves with last-frame's
    /// counts on a future frame. The result feeds
    /// [`crate::coverage::MeshCoverage::ingest`]. `None` when
    /// `features.coverage_lod == false`.
    pub coverage_buffers: Option<render_passes::coverage::buffers::CoverageBuffers>,
    /// State for the coverage readback loop. `Arc<Mutex<…>>` so the
    /// `spawn_local`-detached `mapAsync` future can write back into
    /// it without re-borrowing the renderer — and so it stays
    /// future-proof for the day the renderer moves across threads
    /// (single-threaded today, so the lock is uncontested).
    pub coverage_readback_state: std::sync::Arc<std::sync::Mutex<CoverageReadbackState>>,
    /// State for the MSAA edge-budget auto-grow readback loop. Same
    /// `Arc<Mutex<…>>` discipline as `coverage_readback_state` —
    /// `mapAsync` writes through the lock from a detached
    /// `spawn_local` future.
    pub edge_overflow_readback_state: std::sync::Arc<std::sync::Mutex<EdgeOverflowReadbackState>>,
    /// State for the GPU light-culling per-froxel capacity auto-grow
    /// loop. Same `Arc<Mutex<…>>` discipline as the other readback
    /// states.
    pub froxel_overflow_readback_state:
        std::sync::Arc<std::sync::Mutex<FroxelOverflowReadbackState>>,
    /// Monotonic frame index. Wraps every ~272 years at 60 Hz — safe to
    /// treat as unbounded for any practical session. Drives the
    /// `skin_update_period` gate and other "every Nth frame" cadences.
    pub frame_index: u64,
    pub shaders: Shaders,
    pub materials: Materials,
    /// Runtime-registered dynamic materials. See
    /// [`crate::dynamic_materials`].
    pub dynamic_materials: crate::dynamic_materials::DynamicMaterials,
    /// Set when a custom material registers/unregisters or its alpha-only WGSL
    /// changes, so the next `finalize_gpu_textures` rebuilds the masked
    /// (alpha-tested) pipelines for MASK customs even if no texture changed
    /// (a procedural cutout needs no texture). Cleared by `finalize_gpu_textures`.
    pub masked_dynamic_dirty: bool,
    /// Renderer-wide variable-length per-material data pool. Backs
    /// `BufferSlot` declarations on registered dynamic materials.
    pub extras_pool: crate::dynamic_materials::extras_pool::ExtrasPool,
    pub pipeline_layouts: PipelineLayouts,
    pub pipelines: Pipelines,
    pub lights: Lights,
    pub textures: Textures,
    pub logging: AwsmRendererLogging,
    pub render_textures: RenderTextures,
    pub render_passes: RenderPasses,
    pub environment: Environment,
    pub anti_aliasing: AntiAliasing,
    /// Plan B shared-prep + deferred-shadow config, captured at build time
    /// (`docs/plans/deferred-shared-prep-pass.md`). Inert until the prep pass is
    /// wired in; later stages read `prep_config.enabled` to choose the path.
    pub prep_config: crate::render_passes::material_prep::PrepPassConfig,
    /// Unified-edge-shading scaffolding toggle (U0 of
    /// `docs/plans/unified-edge-shading.md`). Default `false`. When `true`,
    /// the classify pass additionally writes the per-pixel `edge_id_tex` and
    /// builds an ANY-sample `tile_mask` (both consumed by the future unified
    /// kernel; INERT in U0 — `edge_id_tex` is unread and the ANY-sample
    /// tile_mask is render-parity). Threaded into the classify shader cache
    /// key + `RenderTextures` (gates the `edge_id_tex` allocation). Set at
    /// build time via [`AwsmRendererBuilder::with_unified_edge`].
    pub unified_edge: bool,
    pub post_processing: PostProcessing,
    /// GPU mesh-picking subsystem. `None` when
    /// `features.picking == false` (the default for library /
    /// game builds). When `None`, [`Self::pick`] returns
    /// [`crate::picker::PickResult::Disabled`].
    pub picker: Option<Picker>,
    pub lines: LineRenderer,
    /// Auto-drive state for re-resolving HUD meshes' transparent
    /// pipeline variants when a HUD mesh appears or the texture-pool /
    /// MSAA shape changes. `None`-cost for builds that never insert a
    /// HUD mesh (gated on `Meshes::has_seen_hud`). See
    /// [`crate::render`]'s `kick_hud_resolve` / `poll_hud_resolve`.
    pub(crate) hud_resolve: crate::render::HudResolveState,
    /// Per-frame mipmap generator for the opaque RT — only dispatched
    /// when the visible material set contains a transmissive material.
    pub opaque_mipgen: opaque_mipgen::OpaqueMipgen,
    /// Shadow mapping subsystem. Owns the depth atlas, EVSM atlas,
    /// cube-array pool, descriptors, and the comparison / filterable
    /// samplers used by the shadow-aware shading passes.
    pub shadows: shadows::Shadows,
    /// Opt-in feature gates picked at construction time.
    pub features: RendererFeatures,
    /// Adaptive runtime policy on top of `features`. `RendererFeatures`
    /// decides which buffers/passes exist; `RendererOptimizationPolicy`
    /// decides which of those are engaged this frame. Mutable via
    /// `set_optimization_policy` — flips take effect on the next
    /// `render()` call.
    pub optimization_policy: crate::optimization_policy::RendererOptimizationPolicy,
    /// Most recently computed per-frame derived flags. Used as the
    /// previous-frame state for the next call to
    /// `compute_frame_optimizations` (hysteresis input).
    pub frame_optimizations: crate::optimization_policy::FrameOptimizations,
    /// Consecutive frames the current `gpu_occlusion` mode has held.
    /// Bumped each frame `frame_optimizations.gpu_occlusion` stays the
    /// same; reset to 1 on a flip. Feeds the Auto-mode cooldown check
    /// in `compute_frame_optimizations`.
    pub frames_in_current_mode: u32,
    /// Global default for `Mesh::cheap_material_pixel_threshold`.
    /// Per-mesh override still wins; this is the value used when a
    /// mesh has its threshold set to `None`.
    /// Default `64`. Games tying material LOD to their own quality
    /// system can write this directly each frame; no automatic
    /// coupling to `ShadowQualityTier` (which is per-light, not
    /// global).
    pub default_cheap_material_pixel_threshold: u32,
    /// Reusable scratch space for the per-frame renderable lists.
    /// Held here (not constructed per-frame) so the Vec allocations
    /// survive across frames; `collect_renderables` clears-in-place.
    pub(crate) renderable_pool: crate::renderable::RenderablePool,
    /// Pipeline-readiness scheduler. Owns the `FuturesUnordered` that
    /// drives async compile, the SlotMap of material groups, and the
    /// per-pass-kind map. Per the architecture in
    /// `https://github.com/dakom/awsm-renderer/pull/99`, frontends submit
    /// [`crate::pipeline_scheduler::PipelineGroupDef`]s, get [`crate::pipeline_scheduler::PipelineGroupId`]s back
    /// immediately, and watch for status transitions via
    /// `drain_pipeline_status_events` or `pipeline_group_status`.
    ///
    /// **Stage 1 status**: the scheduler is attached; the public API
    /// surface (`submit_pipeline_group_batch`, `pipeline_group_status`,
    /// `drain_pipeline_status_events`, `drop_material_group`,
    /// `poll_pipeline_scheduler`) is wired below this struct. Compile
    /// futures are currently stubs — Stage 1 follow-up wires each
    /// `PipelineGroupDef` variant to the real compile path.
    pub pipeline_scheduler: crate::pipeline_scheduler::PipelineScheduler,
    /// Bucket-layout fingerprint of the last `ensure_scene_pipelines`
    /// run. The render-driven compile path
    /// ([`crate::AwsmRenderer::ensure_scene_pipelines`]) compares the
    /// live bucket list's `dispatch_hash` + entry count against this to
    /// decide whether the bucket SET changed (a new dynamic material
    /// registered, a feature-set variant allocated, a material removed)
    /// — which requires resizing the classify / edge GPU buffers +
    /// rebuilding the edge-layout uniform + clearing the stale
    /// layout-keyed pipeline caches BEFORE compiling against the new
    /// layout. `None` until the first ensure. See
    /// `ensure_scene_pipelines` for the ordering invariant.
    pub(crate) last_ensured_bucket_layout: Option<(u64, usize)>,
    /// True once `AwsmRendererBuilder::build` has finished its eager
    /// batch. Config-change APIs (`set_anti_aliasing`,
    /// `set_post_processing`) gate on this and return
    /// [`crate::error::AwsmError::NotReady`] when called before. Per
    /// the architecture doc's race policy.
    pub(crate) build_complete: bool,
    /// Recommended `ShadowQualityTier` set by the active
    /// [`crate::profile::RendererProfile`]. Scene-side code that
    /// registers shadow-casting lights should apply the matching
    /// `LightShadowParams` preset to keep per-light shadow knobs
    /// (cascade count, hardness, EVSM cutoff) coherent with the
    /// rest of the profile's defaults. `None` when no profile was
    /// applied.
    pub recommended_shadow_quality_tier: Option<crate::shadows::ShadowQualityTier>,
    // we pick between these on the fly.
    // `pub(crate)` (not private) because `AwsmRenderer` now lives in the
    // `renderer` submodule rather than the crate root: sibling modules
    // (e.g. `render.rs`) read these directly, which previously worked via
    // the private-at-crate-root → visible-to-all-descendants rule. Crate
    // visibility is identical to before; external API is unchanged.
    pub(crate) _clear_color_perceptual_to_linear: Color,
    pub(crate) _clear_color: Color,

    #[cfg(feature = "animation")]
    pub animations: animation::Animations,

    /// Per-camera authorable parameter store (projection, clip planes,
    /// depth-of-field). Driven by `AnimationTarget::Camera` channels.
    pub cameras: crate::cameras::Cameras,
}

/// Compatibility requirements for this renderer.
///
/// `storage_buffers` is the worst-case `maxStorageBuffersPerShaderStage`
/// the opaque-material pass needs. Opaque currently binds:
///   * 8 storage buffers in `@group(0)`: visibility_data,
///     material_mesh_metas, materials, attribute_indices,
///     attribute_data, transforms (packed model + normal — Option E),
///     texture_transforms, instance_attrs.
///   * 1 storage buffer in `@group(1)`: lights_storage (the GPU cull
///     pass's per-froxel light slices).
///
/// Total = 9, leaving 1 spare under a 10-buffer limit. lights +
/// lights_info are uniforms in group(1) (Option F); shading reads the
/// per-pixel froxel light list from `lights_storage`, so no separate
/// per-mesh slices storage buffer is needed. The transparent pass
/// peaks at 9. Bumping this lower than
/// the binding count will pass adapter compatibility on a device that
/// exactly meets the declared limit, then fail pipeline validation
/// when the shader is compiled.
///
/// ## Dynamic-materials `extras_pool` slot
///
/// The 10th storage-buffer slot is reserved for the `extras_pool`
/// buffer that backs `BufferSlot` declarations on registered custom
/// materials. The pool itself is documented at
/// `crates/renderer/src/dynamic_materials/extras_pool.rs`; the
/// per-binding wiring lives in the opaque + transparent passes'
/// `bind_groups.wgsl` (binding 23 / 19 respectively).
pub static COMPATIBITLIY_REQUIREMENTS: LazyLock<CompatibilityRequirements> =
    LazyLock::new(|| CompatibilityRequirements {
        storage_buffers: Some(10),
    });

impl AwsmRenderer {
    /// Removes all scene data by rebuilding the renderer state.
    ///
    /// Preserves every field the user picked at build time — both the
    /// historical set (`logging`, `clear_color`, `render_texture_formats`,
    /// `features`, `optimization_policy`) and the
    /// [`crate::profile::RendererProfile`]-derived bundle
    /// (`anti_aliasing`, `post_processing`, `shadows_config`,
    /// `max_edge_budget`, `scene_spatial_config`,
    /// `recommended_shadow_quality_tier`). Forwarding the *current
    /// values* rather than re-resolving the profile means any
    /// post-profile per-knob override the frontend chained on top is
    /// preserved too — `remove_all` is a scene-data wipe, not a
    /// config-reset.
    pub async fn remove_all(&mut self) -> crate::error::Result<()> {
        // meh, just recreate the renderer, it's fine
        let mut builder = AwsmRendererBuilder::new(self.gpu.clone())
            .with_logging(self.logging.clone())
            .with_clear_color(self._clear_color.clone())
            .with_render_texture_formats(self.render_textures.formats.clone())
            .with_features(self.features.clone())
            .with_optimization_policy(self.optimization_policy.clone())
            .with_anti_aliasing(self.anti_aliasing.clone())
            .with_post_processing(self.post_processing.clone())
            .with_shadows_config(self.shadows.config().clone())
            .with_scene_spatial_config(self.scene_spatial.config());
        if let Some(budget) = self
            .material_edge_buffers
            .as_ref()
            .map(|eb| eb.max_edge_budget)
        {
            builder = builder.with_max_edge_budget(budget);
        }
        if let Some(tier) = self.recommended_shadow_quality_tier {
            builder = builder.with_recommended_shadow_quality_tier(tier);
        }
        let renderer = builder.build().await?;

        *self = renderer;
        Ok(())
    }

    /// Returns the active feature gates picked at construction time.
    pub fn features(&self) -> &RendererFeatures {
        &self.features
    }

    /// Force-compile the routinely-used WebGPU pipelines ahead of the
    /// first user-interactive frame, so the first draw doesn't stall
    /// on shader compilation, exploiting the browser's PSO cache so the
    /// driver reuses already-compiled pipeline state objects.
    ///
    /// ## What's already prewarmed at construction time
    ///
    /// `AwsmRendererBuilder::build()` already compiles, in parallel:
    ///
    /// - **Opaque-compute** material kernels — only the empty kernel for the
    ///   active MSAA (the no-meshes / skybox-only fallback). The first-party
    ///   material shaders (PBR / Unlit / Toon / Flipbook) are **NOT** compiled
    ///   at boot — they compile lazily on first use via
    ///   [`Self::ensure_scene_pipelines`], so a project that uses none of them
    ///   pays zero material-shader compile cost at startup. See the
    ///   `MaterialOpaquePipelines` module docs + `shader_descriptors_and_layouts`.
    /// - **Geometry render pipelines** — every (MSAA × instancing ×
    ///   storage-array × cull_mode) variant. See
    ///   `GeometryRenderPipelineKeys::new`.
    /// - **Shadow / HZB / coverage / decal / classify / light-culling**
    ///   passes — all built once during `RenderPasses::new`.
    ///
    /// So this method is **mostly a labelling hook today** — its real
    /// payoff is the call-site UX: a consumer can advance their boot
    /// loader to "Compiling shaders…" before this call and back to
    /// "Loading assets…" after, giving users a precise progress
    /// indicator over the multi-hundred-ms shader-compile window that
    /// previously appeared as a generic "Initializing renderer…".
    ///
    /// ## What this method does today
    ///
    /// - **Builder-time prewarm** has already compiled the empty-opaque kernel,
    ///   the geometry passes, hzb, material_classify, effects, decal, shadows,
    ///   and the picker / line variants — but **not** the first-party material
    ///   pipelines (those are lazy). Calling this at the end of `build()` kicks
    ///   `ensure_scene_pipelines`, which compiles whatever the *live* scene
    ///   actually needs (none, on an empty scene); cached keys return
    ///   immediately.
    ///
    /// - **Per-scene transparent prewarm** runs whenever the caller
    ///   invokes this method after meshes have been populated. It
    ///   walks `self.meshes`, deduplicates by
    ///   `(buffer_info, material)` (the granularity the transparent
    ///   pipeline cache keys against), and issues one batched
    ///   `ensure_keys` covering every unique transparent shader +
    ///   pipeline variant the live scene needs. The first transparent
    ///   draw then hits warm cache instead of stalling on N
    ///   `createRenderPipelineAsync` awaits. Mirrors what
    ///   `finalize_gpu_textures` does on a texture-pool-dirty cycle;
    ///   safe to call any number of times (subsequent calls are
    ///   cache-hit no-ops).
    ///
    /// ## When the warm prewarm helps vs not
    ///
    /// The transparent shader/pipeline cache key includes
    /// `texture_pool_arrays_len` + `texture_pool_samplers_len`. Those
    /// values change every time a *new texture array shape* enters the
    /// pool — which on a fresh load happens once when the first model's
    /// textures finalize, and then never again for the same scene.
    /// So:
    ///
    /// - If the caller invokes `prewarm_pipelines()` **before any
    ///   models are loaded** (the historical pattern), the texture
    ///   pool is empty (`arrays_len = 0`), and any pipelines warmed
    ///   here are invalidated the moment the first model finishes
    ///   loading and the pool grows. The call is a no-op for that
    ///   case — only its tracing span fires.
    /// - If the caller invokes it **after a model has loaded** (or
    ///   the texture-pool capacity is otherwise pinned at the value
    ///   the scene will actually use), the warmed pipelines are the
    ///   real ones the renderer will draw with. This is the case
    ///   that absorbs the per-mesh first-draw stall when *switching*
    ///   between models that share the texture-pool shape but
    ///   introduce new geometry attribute combinations.
    ///
    /// ## Idempotent + cheap on warm cache
    ///
    /// Calling this multiple times is a no-op past the first
    /// invocation: every underlying `ensure_keys` is a cache-keyed
    /// lookup. On a Chrome session with a warm GPU disk cache, the
    /// whole call completes in <5 ms. On a cold cache (post-redeploy
    /// first-ever visit) it costs 50–500 ms per N transparent
    /// variants — the same compile tax the first draw would have
    /// paid, just relocated to a phase the consumer can label
    /// clearly.
    ///
    /// ## Material pipelines (classify / opaque / edge resolve)
    ///
    /// These are compiled by the single render-driven operation
    /// [`Self::ensure_scene_pipelines`], which this method kicks once up
    /// front so the awaited readiness path (`wait_for_pipelines_ready`)
    /// has promises to drain. It covers every live bucket (first-party
    /// canonical, PBR/Toon feature-set variants, and custom dynamic
    /// materials) at the active AA config; idempotent on cache hits.
    pub async fn prewarm_pipelines(&mut self) -> crate::error::Result<()> {
        let _maybe_span = if self.logging.render_timings.sub_frame() {
            Some(tracing::span!(tracing::Level::INFO, "Prewarm Pipelines").entered())
        } else {
            None
        };

        // Material pipelines (classify / opaque / per-shader + skybox +
        // final_blend edge resolve) are compiled by THE single render-driven
        // operation: `ensure_scene_pipelines`. Kick it here so the up-front
        // warm path (`wait_for_pipelines_ready`) has the promises in flight
        // to drain. It compiles only the ACTIVE config's variants for every
        // live bucket (incl. PBR feature-set variants — they live in
        // `bucket_entries` even with an empty custom registry), handles any
        // bucket-SET change (resize buffers + clear stale caches first), and
        // is idempotent on a warm cache. The edge-resolve set is rebuilt
        // inside it via the same `MaterialEdgePipelines::build_descriptors` /
        // `desired_keys` path the background relaunch uses, so the two never
        // diverge.
        self.ensure_scene_pipelines()?;

        // Build one request per mesh. `ensure_keys` on both caches
        // dedupes internally by cache key, so we don't need to
        // dedupe at the request level — and dedup'ing here by
        // `(buffer_info, material)` OR-style (the previous
        // pre-existing pattern) misses pairs like (A,M1)(B,M2)(A,M2)
        // when M1 and M2 differ in `writes_depth`, which would
        // leave some meshes with stale pipeline-key map entries.
        let mut requests: Vec<
            crate::render_passes::material_transparent::pipeline::TransparentMeshPipelineRequest,
        > = Vec::new();
        for (mesh_key, mesh) in self.meshes.iter() {
            // Only warm transparent pipelines for transparent-pass meshes — an
            // opaque (incl. opaque-dynamic) material can't compile against the
            // transparent fragment contract.
            if !self.materials.is_transparency_pass(mesh.material_key) {
                continue;
            }
            let buffer_info_key = self.meshes.buffer_info_key(mesh_key)?;
            let writes_depth = self.materials.transparent_writes_depth(mesh.material_key);
            let (base, pbr_features) = self.materials.transparent_variant(mesh.material_key);
            let dynamic_shader_id = matches!(base, crate::dynamic_materials::ShadingBase::Custom)
                .then(|| self.materials.shader_id(mesh.material_key));
            let dynamic_shader =
                dynamic_shader_id.and_then(|id| self.dynamic_materials.shader_info_for(id));
            requests.push(
                crate::render_passes::material_transparent::pipeline::TransparentMeshPipelineRequest {
                    mesh,
                    mesh_key,
                    buffer_info_key,
                    writes_depth,
                    base,
                    pbr_features,
                    dynamic_shader_id,
                    dynamic_shader,
                },
            );
        }

        if requests.is_empty() {
            return Ok(());
        }

        self.render_passes
            .material_transparent
            .pipelines
            .set_render_pipeline_keys_batched(
                &self.gpu,
                requests,
                &mut self.shaders,
                &mut self.pipelines,
                &self.render_passes.material_transparent.bind_groups,
                &self.pipeline_layouts,
                &self.meshes.buffer_infos,
                &self.anti_aliasing,
                &self.textures,
                &self.render_textures.formats,
            )
            .await?;

        Ok(())
    }

    /// Returns the current adaptive policy.
    pub fn optimization_policy(&self) -> &crate::optimization_policy::RendererOptimizationPolicy {
        &self.optimization_policy
    }

    /// Replaces the adaptive policy. Takes effect on the next
    /// `render()`. If the new policy disables `gpu_occlusion`
    /// (Force→Off, or Auto's hysteresis later landing there), the next
    /// frame's `compute_frame_optimizations` will flip
    /// `frame_optimizations.gpu_occlusion = false`, which render.rs
    /// uses to poison `compaction_buffers.args_ready` — so a future
    /// re-enable warms up through the CPU geometry path for one frame
    /// before drawIndirect resumes.
    pub fn set_optimization_policy(
        &mut self,
        policy: crate::optimization_policy::RendererOptimizationPolicy,
    ) {
        // Reset cooldown when the mode itself changes — flipping from
        // Auto to Force (or vice versa) shouldn't be held off by a
        // residual Auto cooldown counter.
        if policy.gpu_culling != self.optimization_policy.gpu_culling {
            self.frames_in_current_mode = u32::MAX / 2;
        }
        self.optimization_policy = policy;
    }

    /// Aggregate Phase-2.1 upload-ring telemetry across every
    /// renderer subsystem with a `MappedUploader`. Returned as a
    /// `(label, stats)` list so callers (e.g. the scene-editor's
    /// `read_upload_ring_stats` wasm export) can render per-subsystem
    /// + rolled-up totals.
    pub fn upload_ring_stats(
        &self,
    ) -> Vec<(
        &'static str,
        crate::buffer::mapped_staging_ring::UploadStats,
    )> {
        let mut v = vec![
            ("transforms", self.transforms.upload_stats()),
            ("materials", self.materials.upload_stats()),
            (
                "instances.transforms",
                self.instances.transform_upload_stats(),
            ),
            (
                "instances.attributes",
                self.instances.attribute_upload_stats(),
            ),
            (
                "meshes.meta.geometry",
                self.meshes.meta.geometry_upload_stats(),
            ),
            (
                "meshes.meta.material",
                self.meshes.meta.material_upload_stats(),
            ),
            (
                "meshes.skins.matrices",
                self.meshes.skins.matrices_upload_stats(),
            ),
            (
                "meshes.skins.joint_index_weights",
                self.meshes.skins.joint_index_weights_upload_stats(),
            ),
            (
                "meshes.morphs.geometry.weights",
                self.meshes.morphs.geometry.weights_upload_stats(),
            ),
            (
                "meshes.morphs.geometry.values",
                self.meshes.morphs.geometry.values_upload_stats(),
            ),
            (
                "meshes.morphs.material.weights",
                self.meshes.morphs.material.weights_upload_stats(),
            ),
            (
                "meshes.morphs.material.values",
                self.meshes.morphs.material.values_upload_stats(),
            ),
            (
                "textures.transforms",
                self.textures.texture_transforms_upload_stats(),
            ),
            ("meshes.pool", self.meshes.upload_stats()),
            // Phase-2.1 raw-writeBuffer promotions (this sprint):
            ("camera", self.camera.upload_stats()),
            ("frame_globals", self.frame_globals.upload_stats()),
            ("lights", self.lights.upload_stats()),
            ("shadows", self.shadows.upload_stats()),
        ];
        if let Some(occ) = self.occlusion_buffers.as_ref() {
            v.push(("occlusion", occ.upload_stats()));
        }
        v
    }
}

/// Coarse-grained stages the renderer passes through during
/// [`AwsmRendererBuilder::build`]. Subscribers passed via
/// [`AwsmRendererBuilder::with_phase_handler`] get a callback at each
/// transition; consumer UIs can map these to whatever progress
/// message they want to show.
///
/// The most useful UX win these surface is the **cold WebGPU cache**
/// case: a fresh Chrome profile can sit on `CompilingShaders` for
/// tens of seconds while Dawn + the GPU driver lower every shader to
/// MSL on the first visit. Showing "Browser is compiling shaders…
/// (first load may take a while)" rather than a frozen "Initializing
/// renderer…" is the difference between a user assuming the app is
/// broken and a user knowing the browser is doing real work that
/// will be cached next time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RendererLoadingPhase {
    /// Adapter / device acquisition + initial bookkeeping +
    /// supporting GPU resource generation (IBL default cubemaps,
    /// BRDF LUT compute, opaque-mipgen pipeline) + render-pass
    /// shader cache key collection. No Dawn shader / pipeline
    /// compile work in this phase — it's the concurrent setup that
    /// feeds the cross-renderer pool.
    Init,
    /// The cross-renderer shader pool is running:
    /// one `Shaders::ensure_keys` covering every shader the
    /// renderer compiles (RenderPasses + Picker + LineRenderer +
    /// Shadows caster + Effects + Display), joined with EVSM
    /// inline-shader `validate_shader` futures. On a cold PSO disk
    /// cache this is where Dawn lowers WGSL → MSL; on a warm cache
    /// it's a cache-hit lookup.
    CompilingShaders,
    /// The cross-renderer pipeline pool is running: one
    /// `try_join`'d `ComputePipelines::ensure_keys` +
    /// `RenderPipelines::ensure_keys` covering every compute and
    /// render pipeline across the entire renderer.
    BuildingPipelines,
    /// All renderer-init work done; ready to render.
    Ready,
}

/// Boxed phase-transition callback handed to the builder via
/// [`AwsmRendererBuilder::with_phase_handler`]. wasm is
/// single-threaded so we don't need `Send + Sync`.
pub type RendererLoadingPhaseHandler = Box<dyn FnMut(RendererLoadingPhase)>;

/// Builder for `AwsmRenderer`.
pub struct AwsmRendererBuilder {
    gpu: AwsmRendererGpuBuilderKind,
    logging: AwsmRendererLogging,
    render_texture_formats: Option<RenderTextureFormats>,
    brdf_lut_options: BrdfLutOptions,
    clear_color: Color,
    // all these colors are typically replaced when loading external textures
    // but we want something to show by default
    skybox_colors: CubemapBitmapColors,
    ibl_filtered_env_colors: CubemapBitmapColors,
    ibl_irradiance_colors: CubemapBitmapColors,
    anti_aliasing: AntiAliasing,
    post_processing: PostProcessing,
    /// Renderer-wide shadow config picked up at construction time.
    /// Resource-shaped fields (`atlas_size`, `point_shadow_resolution`,
    /// `max_point_shadows`, `evsm_atlas_size`) are baked into the
    /// shadow textures; runtime tweaks of those need a renderer
    /// rebuild. Defaults via `ShadowsConfig::default()` if unset.
    shadows_config: Option<shadows::ShadowsConfig>,
    /// Opt-in feature gates. Defaults to both flags `false` so library
    /// consumers pay zero cost for unused GPU-driven culling / decal
    /// infrastructure.
    features: RendererFeatures,
    /// Block C.2: optional override for the
    /// [`MaterialEdgeBuffers`](crate::render_passes::material_opaque::edge_buffers::MaterialEdgeBuffers)
    /// `MAX_EDGE_BUDGET`. `None` → platform default (desktop). Set
    /// via [`AwsmRendererBuilder::with_max_edge_budget`] to grow the
    /// edge budget upfront for pathological-edge-density scenes
    /// (dense foliage at 4K, etc.). Consumers monitoring
    /// edge_overflow_count via CPU readback can also grow the budget
    /// at runtime via [`AwsmRenderer::set_max_edge_budget`].
    max_edge_budget: Option<u32>,
    /// Registration ceiling for co-resident material buckets (§2). `None`
    /// → default 32 (identical to today). Set via
    /// [`AwsmRendererBuilder::with_bucket_config`]; validated `1..=65534`.
    bucket_config: Option<crate::dynamic_materials::BucketConfig>,
    /// Plan B shared-prep + deferred-shadow config
    /// (`docs/plans/deferred-shared-prep-pass.md`). Inert until the prep pass is
    /// wired in; `enabled` defaults `false` (legacy recompute-in-shader path).
    prep_config: crate::render_passes::material_prep::PrepPassConfig,
    /// Unified-edge-shading scaffolding toggle (U0). Default `false` (legacy
    /// sample-0 tile_mask + no `edge_id_tex`). See
    /// [`AwsmRenderer::unified_edge`].
    unified_edge: bool,
    /// Adaptive runtime policy. Defaults to `Auto` mode for the
    /// gpu_culling path; library consumers can override at build time
    /// (or via `AwsmRenderer::set_optimization_policy` later) to force
    /// the path on/off or to retune the Auto thresholds.
    optimization_policy: crate::optimization_policy::RendererOptimizationPolicy,
    /// Optional consumer-supplied callback fired at each
    /// [`RendererLoadingPhase`] transition during `build()`. UI
    /// consumers wire this to update a loading overlay; tracing /
    /// telemetry consumers can use it to record per-phase elapsed
    /// time.
    phase_handler: Option<RendererLoadingPhaseHandler>,
    /// Optional override for the BVH rebuild cadence. `None` →
    /// `SceneSpatialConfig::default()`. Set via
    /// [`AwsmRendererBuilder::with_scene_spatial_config`] directly
    /// or indirectly via [`AwsmRendererBuilder::with_profile`].
    scene_spatial_config: Option<crate::scene_spatial::SceneSpatialConfig>,
    /// Recommended `ShadowQualityTier` from the active profile.
    /// Surfaced via [`AwsmRenderer::recommended_shadow_quality_tier`]
    /// so scene-side code that registers shadow-casting lights can
    /// apply the matching `LightShadowParams` preset on insert.
    /// `None` when no profile is set.
    recommended_shadow_quality_tier: Option<crate::shadows::ShadowQualityTier>,
    /// Pending depth-format override stashed by `with_profile` when
    /// no user-supplied `RenderTextureFormats` exists yet. Applied
    /// inside `build()` after the per-device probe — that's where
    /// the rest of the format defaults come from. `None` when no
    /// profile selected one (or the user already supplied a full
    /// formats struct, in which case `with_profile` mutates it in
    /// place).
    render_texture_formats_depth_override: Option<awsm_renderer_core::texture::TextureFormat>,
}

/// WebGPU builder input for `AwsmRendererBuilder`.
pub enum AwsmRendererGpuBuilderKind {
    /// Build from a WebGPU builder.
    WebGpuBuilder(AwsmRendererWebGpuBuilder),
    /// Use an already-built WebGPU context.
    WebGpuBuilt(AwsmRendererWebGpu),
}

impl From<AwsmRendererWebGpuBuilder> for AwsmRendererGpuBuilderKind {
    fn from(builder: AwsmRendererWebGpuBuilder) -> Self {
        AwsmRendererGpuBuilderKind::WebGpuBuilder(builder)
    }
}

impl From<AwsmRendererWebGpu> for AwsmRendererGpuBuilderKind {
    fn from(gpu: AwsmRendererWebGpu) -> Self {
        AwsmRendererGpuBuilderKind::WebGpuBuilt(gpu)
    }
}

impl AwsmRendererBuilder {
    /// Creates a new renderer builder from a WebGPU builder or context.
    pub fn new(gpu: impl Into<AwsmRendererGpuBuilderKind>) -> Self {
        Self {
            gpu: gpu.into(),
            logging: AwsmRendererLogging::default(),
            render_texture_formats: None,
            clear_color: Color::BLACK,
            brdf_lut_options: BrdfLutOptions::default(),
            skybox_colors: CubemapBitmapColors {
                z_positive: Color::BLACK,
                z_negative: Color::BLACK,
                x_positive: Color::BLACK,
                x_negative: Color::BLACK,
                y_positive: Color::BLACK,
                y_negative: Color::BLACK,
            },
            // skybox_colors: CubemapBitmapColors {
            //     z_positive: Color::from_hex_rgb(0xFF0000), // red
            //     z_negative: Color::from_hex_rgb(0x00FF00), // green
            //     x_positive: Color::from_hex_rgb(0x0000FF), // blue
            //     x_negative: Color::from_hex_rgb(0xFFFF00), // yellow
            //     y_positive: Color::from_hex_rgb(0xFF00FF), // magenta
            //     y_negative: Color::from_hex_rgb(0x00FFFF), // cyan
            // },
            ibl_filtered_env_colors: CubemapBitmapColors {
                z_positive: Color::WHITE,
                z_negative: Color::WHITE,
                x_positive: Color::WHITE,
                x_negative: Color::WHITE,
                y_positive: Color::WHITE,
                y_negative: Color::WHITE,
            },
            ibl_irradiance_colors: CubemapBitmapColors {
                z_positive: Color::WHITE,
                z_negative: Color::WHITE,
                x_positive: Color::WHITE,
                x_negative: Color::WHITE,
                y_positive: Color::WHITE,
                y_negative: Color::WHITE,
            },
            anti_aliasing: AntiAliasing::default(),
            post_processing: PostProcessing::default(),
            shadows_config: None,
            features: RendererFeatures::default(),
            max_edge_budget: None,
            bucket_config: None,
            prep_config: crate::render_passes::material_prep::PrepPassConfig::default(),
            unified_edge: false,
            optimization_policy: crate::optimization_policy::RendererOptimizationPolicy::default(),
            phase_handler: None,
            scene_spatial_config: None,
            recommended_shadow_quality_tier: None,
            render_texture_formats_depth_override: None,
        }
    }

    /// Apply a coordinated set of defaults from a
    /// [`crate::profile::RendererProfile`]. Sets `anti_aliasing`,
    /// `post_processing`, `features`, `optimization_policy`,
    /// `shadows_config`, `max_edge_budget`, `scene_spatial_config`,
    /// and the recommended shadow quality tier — all the knobs whose
    /// right starting value differs between mobile-class and
    /// desktop-class targets.
    ///
    /// **Call order**: invoke this **first**, then chain any per-knob
    /// `with_*` overrides — the profile mutates the builder's state
    /// immediately, so later `with_*` calls win.
    ///
    /// Frontends typically resolve the profile from a URL parameter
    /// (`?mobile=true`) via
    /// [`awsm_web_shared::perf::resolve_renderer_profile`](https://github.com/dakom/awsm-renderer/blob/main/crates/web-shared/src/perf.rs)
    /// and pass the result here.
    ///
    /// **Per-light shadow params** aren't owned by the renderer
    /// builder — scene-side code reads
    /// [`AwsmRenderer::recommended_shadow_quality_tier`] after build
    /// and applies the matching `LightShadowParams` preset on each
    /// shadow-casting light registration.
    pub fn with_profile(mut self, profile: crate::profile::RendererProfile) -> Self {
        let defaults = profile.defaults();
        self.anti_aliasing = defaults.anti_aliasing;
        self.post_processing = defaults.post_processing;
        self.features = defaults.features;
        self.optimization_policy = defaults.optimization_policy;
        self.shadows_config = Some(defaults.shadows_config);
        self.max_edge_budget = Some(defaults.max_edge_budget);
        self.scene_spatial_config = Some(defaults.scene_spatial);
        self.recommended_shadow_quality_tier = Some(defaults.shadow_quality_tier);
        // Render-texture format override: only the `depth` field
        // varies by profile today. Build a `RenderTextureFormats`
        // around the per-device baseline at `build()` time if the
        // user hasn't supplied one — we can't do the async default
        // probe here in a sync builder method. The depth override
        // gets re-applied inside `build()` (see the
        // `render_texture_formats` materialization there).
        if let Some(formats) = self.render_texture_formats.as_mut() {
            formats.depth = defaults.render_texture_formats.depth;
        } else {
            // Stash the override on a builder-private field so the
            // build() async path can apply it once the per-device
            // defaults have been probed. We re-use
            // `render_texture_formats` indirectly via the
            // post-profile mutation below.
            self.render_texture_formats_depth_override =
                Some(defaults.render_texture_formats.depth);
        }
        self
    }

    /// Override the BVH rebuild cadence directly. The
    /// [`crate::profile::RendererProfile`] is the usual surface for
    /// this; only call this directly for bespoke tuning.
    pub fn with_scene_spatial_config(
        mut self,
        config: crate::scene_spatial::SceneSpatialConfig,
    ) -> Self {
        self.scene_spatial_config = Some(config);
        self
    }

    /// Block C.2: override the default `MAX_EDGE_BUDGET` for the
    /// MSAA edge-resolve buffers. Default picks
    /// `DEFAULT_MAX_EDGE_BUDGET_DESKTOP` (512k edge pixels). Mobile
    /// consumers should pass `DEFAULT_MAX_EDGE_BUDGET_MOBILE`
    /// (256k); pathological-edge content can pass higher values
    /// (e.g. 1M) to absorb dense foliage at 4K.
    ///
    /// Live tuning at runtime is also available via
    /// [`AwsmRenderer::set_max_edge_budget`] — call it when
    /// [`note_edge_overflow_observed`](crate::render_passes::material_opaque::edge_buffers::note_edge_overflow_observed)
    /// fires (indicating overflow this session).
    pub fn with_max_edge_budget(mut self, budget: u32) -> Self {
        self.max_edge_budget = Some(budget.max(1));
        self
    }

    /// Sets the registration ceiling for co-resident material buckets
    /// (`docs/plans/increase-materials.md` §2). Default is 32 (identical to
    /// today). Valid range `1..=65534`; an out-of-range value is clamped
    /// into range here and logged, so the builder never produces a registry
    /// that can mint a bucket index the edge encoding can't represent. The
    /// cap sizes nothing per-frame — every GPU width follows the *live*
    /// bucket count, so a high cap costs nothing until the count grows.
    pub fn with_bucket_config(mut self, config: crate::dynamic_materials::BucketConfig) -> Self {
        let config = match config.validate() {
            Ok(()) => config,
            Err(msg) => {
                let clamped = config
                    .max_bucket_entries
                    .clamp(1, crate::dynamic_materials::MAX_BUCKET_ENTRIES_CEILING);
                tracing::warn!(
                    target: "awsm_renderer::dynamic_materials",
                    "with_bucket_config: {msg}; clamping to {clamped}"
                );
                crate::dynamic_materials::BucketConfig {
                    max_bucket_entries: clamped,
                }
            }
        };
        self.bucket_config = Some(config);
        self
    }

    /// Plan B (`docs/plans/deferred-shared-prep-pass.md`) A/B flag. `false`
    /// (default) keeps the legacy path where every per-material kernel
    /// reconstructs attributes + samples shadows inline; `true` routes through
    /// the shared prep pass + slim per-material shading. Inert until the prep
    /// pass is wired in (later stages).
    pub fn with_prep_pass(mut self, enabled: bool) -> Self {
        self.prep_config.enabled = enabled;
        self
    }

    /// Unified-edge-shading scaffolding toggle (U0 of
    /// `docs/plans/unified-edge-shading.md`). `false` (default) keeps the
    /// legacy classify path (sample-0 `tile_mask`, no `edge_id_tex`); `true`
    /// allocates the per-pixel `edge_id_tex` and makes classify write it +
    /// build an ANY-sample `tile_mask`. INERT in U0 — the texture is unread
    /// and the toggle is render-parity; it scaffolds the future unified
    /// interior+edge kernel (U1+).
    pub fn with_unified_edge(mut self, enabled: bool) -> Self {
        self.unified_edge = enabled;
        self
    }

    /// Max shadow casters that can overlap a single pixel (`K`) — sizes the
    /// per-pixel shadow-visibility buffer. Clamped to
    /// `1..=PrepPassConfig::MAX_SHADOW_CASTERS_PER_PIXEL_CEILING`.
    pub fn with_max_shadow_casters_per_pixel(mut self, k: u32) -> Self {
        self.prep_config.max_shadow_casters_per_pixel =
            k.clamp(1, crate::render_passes::material_prep::PrepPassConfig::MAX_SHADOW_CASTERS_PER_PIXEL_CEILING);
        self
    }

    /// Subscribes to renderer-init phase transitions. The callback
    /// fires once per [`RendererLoadingPhase`] entry — see the enum
    /// docs for what each phase covers. Frontends use this to render
    /// a phase-specific loading message instead of one generic
    /// "Initializing renderer…" line that covers the entire (cold
    /// load: tens of seconds; warm load: ~1s) window.
    pub fn with_phase_handler<F>(mut self, handler: F) -> Self
    where
        F: FnMut(RendererLoadingPhase) + 'static,
    {
        self.phase_handler = Some(Box::new(handler));
        self
    }

    /// Opts into renderer features. Both flags default to `false` so
    /// library consumers pay no cost for GPU-driven culling / decals
    /// when they don't need them. Game-side and editor builds should
    /// set this explicitly.
    pub fn with_features(mut self, features: RendererFeatures) -> Self {
        self.features = features;
        self
    }

    /// Sets the adaptive runtime policy. Independent of
    /// `with_features`: features gate **allocation** (does the HZB
    /// texture exist?), policy gates **engagement** (do we build it
    /// this frame?). Default is `Auto` for `gpu_culling`; pass
    /// `OptimizationMode::Force` for editor / regression-testing
    /// builds, or `Off` for a CPU-only baseline.
    pub fn with_optimization_policy(
        mut self,
        policy: crate::optimization_policy::RendererOptimizationPolicy,
    ) -> Self {
        self.optimization_policy = policy;
        self
    }

    /// Pins a renderer-wide shadow configuration that the new
    /// `Shadows` will use at construction. Use this when loading an
    /// `awsm_scene::EditorProject` so the cube-pool size, EVSM
    /// atlas size, and 2D atlas size match the authored intent before
    /// any frame renders.
    pub fn with_shadows_config(mut self, config: shadows::ShadowsConfig) -> Self {
        self.shadows_config = Some(config);
        self
    }

    /// Sets BRDF LUT generation options.
    pub fn with_brdf_lut_options(mut self, options: BrdfLutOptions) -> Self {
        self.brdf_lut_options = options;
        self
    }

    /// Sets the filtered environment colors for IBL.
    pub fn with_ibl_filtered_env_colors(mut self, colors: CubemapBitmapColors) -> Self {
        self.ibl_filtered_env_colors = colors;
        self
    }

    /// Sets the anti-aliasing configuration.
    pub fn with_anti_aliasing(mut self, anti_aliasing: AntiAliasing) -> Self {
        self.anti_aliasing = anti_aliasing;
        self
    }

    /// Sets the post-processing configuration. Mirrors
    /// [`Self::with_anti_aliasing`] — used by
    /// [`AwsmRenderer::remove_all`] to preserve the live post-process
    /// state across the scene-clear rebuild, and by frontends that
    /// want to start from a non-default tonemapper / bloom / DoF
    /// config without going through `set_post_processing` after build.
    pub fn with_post_processing(mut self, post_processing: PostProcessing) -> Self {
        self.post_processing = post_processing;
        self
    }

    /// Pins the recommended shadow quality tier reported by
    /// [`AwsmRenderer::recommended_shadow_quality_tier`]. Normally set
    /// implicitly via [`Self::with_profile`]; this setter exists so
    /// [`AwsmRenderer::remove_all`] can preserve the value across a
    /// scene-clear rebuild without re-running profile resolution
    /// (which would clobber any post-profile per-knob overrides the
    /// frontend chained on top).
    pub fn with_recommended_shadow_quality_tier(
        mut self,
        tier: crate::shadows::ShadowQualityTier,
    ) -> Self {
        self.recommended_shadow_quality_tier = Some(tier);
        self
    }

    /// Sets the irradiance colors for IBL.
    pub fn with_ibl_irradiance_colors(mut self, colors: CubemapBitmapColors) -> Self {
        self.ibl_irradiance_colors = colors;
        self
    }

    /// Sets the skybox colors.
    pub fn with_skybox_colors(mut self, colors: CubemapBitmapColors) -> Self {
        self.skybox_colors = colors;
        self
    }

    /// Sets logging options for the renderer.
    pub fn with_logging(mut self, logging: AwsmRendererLogging) -> Self {
        self.logging = logging;
        self
    }

    /// Sets render texture formats.
    ///
    /// Clears any pending depth-format override stashed by an earlier
    /// [`Self::with_profile`] call — the explicit formats struct the
    /// caller is supplying here wins, per the documented builder
    /// contract ("later `with_*` calls win" over `with_profile`).
    /// Without this clear, the call sequence
    ///
    /// ```ignore
    /// .with_profile(RendererProfile::Mobile)            // stashes Depth24Plus
    /// .with_render_texture_formats(my_custom_formats)   // depth = Depth32Float
    /// ```
    ///
    /// would silently clobber `my_custom_formats.depth` back to
    /// `Depth24Plus` inside `build()`'s post-probe override-apply
    /// step.
    pub fn with_render_texture_formats(mut self, formats: RenderTextureFormats) -> Self {
        self.render_texture_formats = Some(formats);
        self.render_texture_formats_depth_override = None;
        self
    }

    /// Sets the clear color used for the main render pass.
    pub fn with_clear_color(mut self, color: Color) -> Self {
        self.clear_color = color;
        self
    }

    /// Builds the renderer and initializes GPU resources.
    pub async fn build(self) -> std::result::Result<AwsmRenderer, crate::error::AwsmError> {
        let Self {
            gpu,
            logging,
            render_texture_formats,
            brdf_lut_options,
            clear_color,
            skybox_colors,
            ibl_filtered_env_colors,
            ibl_irradiance_colors,
            anti_aliasing,
            post_processing,
            shadows_config,
            mut features,
            max_edge_budget,
            bucket_config,
            prep_config,
            unified_edge,
            optimization_policy,
            phase_handler,
            scene_spatial_config,
            recommended_shadow_quality_tier,
            render_texture_formats_depth_override,
        } = self;

        let mut phase_handler = phase_handler;
        let build_start_ms = web_sys::js_sys::Date::now();
        let mut phase_start_ms = build_start_ms;
        let mut emit_phase = |phase: RendererLoadingPhase| {
            // Log wall-clock between phases so the user can see where
            // cold-boot time actually goes (the boot-loader caption
            // shows the phase NAME but not how long the previous one
            // took). Tracing target is `awsm_renderer::boot_timing`
            // so consumers can filter for it explicitly.
            let now = web_sys::js_sys::Date::now();
            let dt_phase = now - phase_start_ms;
            let dt_total = now - build_start_ms;
            tracing::info!(
                target: "awsm_renderer::boot_timing",
                "phase = {:?}  (+{:.0}ms phase, {:.0}ms total)",
                phase,
                dt_phase,
                dt_total,
            );
            phase_start_ms = now;
            if let Some(handler) = phase_handler.as_mut() {
                handler(phase);
            }
        };
        emit_phase(RendererLoadingPhase::Init);

        let gpu = match gpu {
            AwsmRendererGpuBuilderKind::WebGpuBuilder(builder) => builder.build().await?,
            AwsmRendererGpuBuilderKind::WebGpuBuilt(gpu) => gpu,
        };

        // Resolve `indirect_first_instance` against device capability.
        // After this point any `Auto` in the toggle is replaced by
        // `On` (when the device exposes the feature) or `Off` (when it
        // doesn't), so downstream code can read `.resolve(false)` and
        // get a deterministic boolean. `On` / `Off` overrides bypass
        // the capability probe entirely — useful for forcing the
        // portable fallback on a supported device (testing) or for
        // forcing the optimized path when out-of-band knowledge says
        // the device supports it.
        //
        // The two paths are *both* fully optimized for their config —
        // see [`crate::features::FeatureToggle`] and
        // [`AwsmRendererWebGpu::has_indirect_first_instance`] for the
        // capability semantics, and the geometry-pass + compaction
        // templating for the per-path code paths.
        let indirect_capability = gpu.has_indirect_first_instance();
        let resolved_indirect = features
            .indirect_first_instance
            .resolve(indirect_capability);
        if matches!(
            features.indirect_first_instance,
            crate::features::FeatureToggle::On
        ) && !indirect_capability
        {
            tracing::warn!(
                "`indirect_first_instance = On` but the device doesn't expose \
                 the `indirect-first-instance` WebGPU feature. drawIndirect \
                 calls with non-zero firstInstance will silently fail. \
                 Switch to Auto (default) or Off to use the portable path."
            );
        }
        features.indirect_first_instance = if resolved_indirect {
            crate::features::FeatureToggle::On
        } else {
            crate::features::FeatureToggle::Off
        };

        let mut render_texture_formats = match render_texture_formats {
            Some(formats) => formats,
            None => RenderTextureFormats::new(&gpu.device).await,
        };
        // Apply the profile's depth-format override (if any) on top of
        // the per-device defaults. Frontends that supplied their own
        // `RenderTextureFormats` already have `with_profile` mutate
        // the depth field in place — no second pass needed there.
        if let Some(depth_override) = render_texture_formats_depth_override {
            render_texture_formats.depth = depth_override;
        }

        // tracing::info!("Max bind groups: {}", gpu.device.limits().max_bind_groups());
        // tracing::info!(
        //     "Max texture size: {}",
        //     gpu.device.limits().max_texture_dimension_2d()
        // );

        let mut pipeline_layouts = PipelineLayouts::new();
        let mut bind_group_layouts = BindGroupLayouts::new();
        let mut pipelines = Pipelines::new();
        let mut shaders = Shaders::new();

        let mut textures = Textures::new(&gpu)?;
        let camera = camera::CameraBuffer::new(&gpu)?;
        let frame_globals = crate::frame_globals::FrameGlobals::new(&gpu)?;

        // One mega-join covering every independent &gpu-only async
        // task in the build's setup stage:
        //
        //   - 3 default-cubemap creations (prefiltered IBL / irradiance
        //     IBL / skybox)
        //   - BRDF LUT generation
        //   - opaque-mipgen pipeline construction
        //   - `RenderPasses::describe_shaders` (bind-group setup +
        //     per-pass shader cache key collection — no Dawn
        //     shader/pipeline compile yet; that's stages 2 and 3
        //     below)
        //   - `RenderTextures::new`
        //
        // The five texture-prep futures only touch `&gpu` (the
        // `prepare_resources` half of the prepare/register split is
        // intentional infrastructure for exactly this kind of merge).
        // `RenderPasses::describe_shaders` holds `&mut` on the
        // shader / pipeline / bind-group-layout caches and reads
        // from `&mut textures` via `RenderPassInitContext`, but
        // those reads (pool.arrays_len, pool_sampler_set,
        // pool.texture_views, get_sampler over pool_sampler_set,
        // texture_transforms_gpu_buffer) are disjoint from anything
        // `IblTexture::register` / `Skybox::register` later mutate —
        // register inserts into `cubemaps` (separate from `pool`)
        // and pulls a sampler key out of `sampler_cache` without
        // ever touching `pool_sampler_set`. So we can safely defer
        // the registers (and the dependent Lights / Environment
        // construction) to the post-await sync block.
        let formats_for_textures = render_texture_formats.clone();
        let bind_groups = BindGroups::new(&features);
        // Resolved edge-pixel budget — mirrors what `MaterialEdgeBuffers` uses
        // (the builder override or the desktop default). Sizes the prep pass's
        // compact per-edge-sample shadow texture (Stage 5b-shadow). Computed here
        // since the edge buffers themselves are allocated further below.
        let resolved_max_edge_budget = max_edge_budget.unwrap_or(
            crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP,
        );

        let mut render_pass_init = RenderPassInitContext {
            gpu: &gpu,
            bind_group_layouts: &mut bind_group_layouts,
            pipeline_layouts: &mut pipeline_layouts,
            pipelines: &mut pipelines,
            shaders: &mut shaders,
            render_texture_formats: &mut render_texture_formats,
            textures: &mut textures,
            features: &features,
            anti_aliasing: &anti_aliasing,
            post_processing: &post_processing,
            prep_config: &prep_config,
            max_edge_budget: resolved_max_edge_budget,
            unified_edge,
        };

        // Phase A of RenderPasses (bind groups + shader cache key
        // collection) joins the texture-prep block. RenderPasses is
        // NOT a full `new()` here anymore — it's split into 3
        // stages so the orchestrator below can pool RenderPasses'
        // shader + pipeline cache keys with every tail subsystem
        // into one cross-renderer shader ensure_keys and one
        // try_join'd compute + render ensure_keys.
        //
        // The work inside this try_join! falls under
        // `RendererLoadingPhase::Init` per the enum's contract
        // (adapter / device + supporting GPU resources + cache-key
        // collection — no Dawn compile yet). The `CompilingShaders`
        // transition fires further down, right before the
        // cross-renderer `Shaders::ensure_keys` call where actual
        // WGSL → MSL compilation begins.
        let (
            ibl_filtered_resources,
            ibl_irradiance_resources,
            skybox_resources,
            brdf_lut,
            opaque_mipgen,
            mut render_passes_plan,
            render_textures,
        ) = futures::try_join!(
            IblTexture::prepare_resources(&gpu, ibl_filtered_env_colors),
            IblTexture::prepare_resources(&gpu, ibl_irradiance_colors),
            Skybox::prepare_resources(&gpu, skybox_colors),
            async {
                BrdfLut::new(&gpu, brdf_lut_options)
                    .await
                    .map_err(crate::error::AwsmError::from)
            },
            async {
                opaque_mipgen::OpaqueMipgen::new(&gpu)
                    .await
                    .map_err(crate::error::AwsmError::from)
            },
            RenderPasses::describe_shaders(&mut render_pass_init, &features),
            async {
                RenderTextures::new(
                    &gpu,
                    formats_for_textures,
                    &features,
                    prep_config.enabled,
                    prep_config.shadow_visibility_layers(),
                    unified_edge,
                )
                .await
                .map_err(crate::error::AwsmError::from)
            },
        )?;
        // Move `render_pass_init` into a discard binding so its
        // `&mut`-borrows of bind_group_layouts / pipeline_layouts /
        // pipelines / shaders / render_texture_formats / textures /
        // features end here unambiguously, freeing the post-await
        // registers below (which mutate `textures`) to compile
        // regardless of any future code that might otherwise extend
        // the borrow's NLL lifetime.
        let _ = render_pass_init;

        let lights = Lights::new(
            &gpu,
            Ibl::new(
                IblTexture::register(&gpu, &mut textures, ibl_filtered_resources)?,
                IblTexture::register(&gpu, &mut textures, ibl_irradiance_resources)?,
            ),
            brdf_lut,
        )?;
        let meshes = Meshes::new(&gpu)?;
        let transforms = Transforms::new(&gpu)?;
        let instances = Instances::new(&gpu)?;
        let materials = Materials::new(&gpu)?;
        let environment =
            Environment::new(Skybox::register(&gpu, &mut textures, skybox_resources)?);

        // Item (2): cross-renderer orchestration. After
        // describe_shaders + the texture-prep block finished above,
        // we now drive the full renderer-wide compile pool from one
        // place. Three awaits cover EVERYTHING (RenderPasses + tail
        // subsystems):
        //
        //   1. ONE `Shaders::ensure_keys` covering RenderPasses-owned
        //      shaders + Picker + LineRenderer + Shadows caster +
        //      Effects + Display.
        //   2. ONE EVSM validate join (3 inline-shader validates —
        //      kicked off via `compile_shader` inside
        //      `Shadows::build_descriptors` immediately after the
        //      shader ensure_keys returns).
        //   3. ONE `try_join`'d compute + render `ensure_keys`
        //      covering every compute / render pipeline across the
        //      entire renderer.
        //
        // The orchestrator owns the pool — `RenderPasses` can't
        // smuggle in a sequential `.await?` because its public API
        // is `describe_shaders → describe_pipelines → from_resolved`,
        // none of which compile pipelines themselves.

        // Sized for a small initial viewport; recreated by
        // `ClassifyBuffers::ensure_capacity` on first frame once the
        // real render-texture size is known.
        // Builder-time sizing — no dynamic materials yet, so bucket
        // count is the first-party-only baseline.
        // `register_material` calls `ensure_bucket_count` if it grows
        // the registry past the current value.
        let first_party_bucket_count =
            crate::dynamic_materials::first_party_bucket_entries().len() as u32;
        let material_classify_buffers =
            render_passes::material_classify::buffers::ClassifyBuffers::new(
                &gpu,
                1024,
                first_party_bucket_count,
            )?;
        // Seed the bucket LUT from the first-party entries so a scene that
        // never registers a dynamic material still classifies correctly;
        // `relayout_bucket_buffers` rebuilds it as the registry grows.
        let material_bucket_lut =
            render_passes::material_classify::bucket_lut::MaterialBucketLut::new(
                &gpu,
                &crate::dynamic_materials::first_party_bucket_entries(),
            )?;

        // Light-culling froxel buffers. Sized to a tiny placeholder
        // viewport; per-frame `ensure_viewport` grows them once the real
        // swap-chain size is known.
        let light_culling_buffers = render_passes::light_culling::LightCullingBuffers::new(
            &gpu,
            16,
            16,
            render_passes::light_culling::DEFAULT_SLICE_COUNT,
            render_passes::light_culling::DEFAULT_MAX_PER_FROXEL_CAPACITY,
            render_passes::light_culling::DEFAULT_MESH_INDICES_CAPACITY,
            render_passes::light_culling::DEFAULT_TILE_LIGHT_CAPACITY,
        )?;

        // MSAA-edge-resolve buffers (Stage 3 dispatch wiring). Allocated only
        // when MSAA is on AND the device supports the required limits
        // (maxStorageBuffersPerShaderStage >= 10 — the WebGPU baseline).
        // The per-shader edge_resolve pipeline layout fits in 4 bind groups
        // since the group(4) → extended-shadows(3) fold; the only remaining
        // device-cap constraint is the storage-buffer count.
        let multisampled_geometry = anti_aliasing.has_msaa_checked()?;
        let edge_resolve_enabled = multisampled_geometry && edge_resolve_supported(&gpu);
        let (material_edge_buffers, material_edge_layout_uniform) = if edge_resolve_enabled {
            use render_passes::material_opaque::edge_buffers::{
                build_edge_layout_uniform, MaterialEdgeBuffers,
            };
            let edge_buffers = if let Some(budget) = max_edge_budget {
                MaterialEdgeBuffers::new_with_budget(&gpu, first_party_bucket_count, budget)?
            } else {
                MaterialEdgeBuffers::new(&gpu, first_party_bucket_count)?
            };
            let max_edge_budget = edge_buffers.max_edge_budget;
            let (uniform, _bytes) =
                build_edge_layout_uniform(&gpu, first_party_bucket_count, max_edge_budget)?;
            (Some(edge_buffers), Some(uniform))
        } else {
            (None, None)
        };

        // Decals subsystem — fixed-capacity GPU storage buffer
        // allocated up front; per-frame upload only touches the
        // bytes for currently-active decals. Gated by `features.decals`.
        let decals = if features.decals {
            Some(decals::Decals::new(&gpu)?)
        } else {
            None
        };

        // Occlusion-cull buffers. Starts at 1024 instances; grows 2×
        // when needed. Gated by `features.gpu_culling`.
        let occlusion_buffers = if features.gpu_culling {
            Some(render_passes::occlusion::buffers::OcclusionBuffers::new(
                &gpu,
            )?)
        } else {
            None
        };

        // Decal classify buckets. Starts at 1×1 tiles; `ensure_capacity`
        // resizes against the live viewport on first render. Gated by
        // `features.decals`.
        let decal_classify_buffers = if features.decals {
            Some(render_passes::material_decal::classify::buffers::DecalClassifyBuffers::new(&gpu)?)
        } else {
            None
        };

        // GPU compaction args buffer. Gated by `features.gpu_culling`.
        let compaction_buffers = if features.gpu_culling {
            Some(render_passes::occlusion::compaction::CompactionBuffers::new(&gpu)?)
        } else {
            None
        };

        // GPU mesh-pixel-coverage producer buffers. Allocated only
        // when `features.coverage_lod` is on — the producer pass
        // populates `MeshCoverage`, and with no opt-in consumer the
        // per-frame compute + readback would be pure waste.
        let coverage_buffers = if features.coverage_lod {
            Some(render_passes::coverage::buffers::CoverageBuffers::new(
                &gpu,
            )?)
        } else {
            None
        };

        // ── 1. Cross-renderer shader pool. Assemble every shader
        //       cache key — RenderPasses-owned (from the describe
        //       phase) + tail subsystems' statically-known keys. ONE
        //       Shaders::ensure_keys.
        //
        // `mem::take` the keys out of the plan rather than cloning:
        // `describe_pipelines` (which consumes the plan below) only
        // reads `plan.bindings`, never `plan.shader_cache_keys`, so
        // leaving the field empty is fine and avoids a per-build
        // allocation of a ~40-entry Vec on the cold path.
        let mut all_shader_keys: Vec<shaders::ShaderCacheKey> =
            std::mem::take(&mut render_passes_plan.shader_cache_keys);
        all_shader_keys.extend(shadows::ShadowsDescriptors::caster_shader_cache_keys());
        // Picker shaders are no longer batched here — the entire Picker
        // subsystem is deferred until first `pick()` query
        // (`AwsmRenderer::ensure_picker_compiled`). Cold-boot compiles
        // 0 picker pipelines even when `features.picking == true`.
        all_shader_keys.push(shaders::ShaderCacheKey::from(
            render_passes::lines::ShaderCacheKeyLine,
        ));
        all_shader_keys.extend(
            render_passes::effects::pipeline::EffectsPipelines::shader_cache_keys_for(
                &anti_aliasing,
                &post_processing,
            )?,
        );
        all_shader_keys.extend(
            render_passes::display::pipeline::DisplayPipelines::shader_cache_keys_for(
                &post_processing,
            ),
        );
        // Phase transition: the actual WGSL → MSL compile happens
        // inside the next await. Emit `CompilingShaders` here so the
        // frontend's "Browser is compiling shaders…" label is correct.
        emit_phase(RendererLoadingPhase::CompilingShaders);
        shaders.ensure_keys(&gpu, all_shader_keys).await?;

        // ── 2. Tail descriptors (cache-hit shader resolutions for
        //       Shadows caster; Shadows internally issues 3 EVSM
        //       `compile_shader` calls that return modules immediately
        //       + surface their validate futures via
        //       `ShadowsDescriptors::evsm`).
        //
        // Picker is no longer built here (Block B.4) — it's compiled
        // on the first `pick()` query via
        // `AwsmRenderer::ensure_picker_compiled`.
        //
        // Lines (Block B.3) are no longer built here — the 4 pipeline
        // variants compile on first line primitive insertion via
        // `AwsmRenderer::ensure_line_pipelines_compiled`, driven by
        // `wait_for_pipelines_ready`. The line BGL is still registered
        // eagerly below (`LineRenderer::new_deferred`) so `add_line_*`
        // can construct per-line bind groups before any pipeline
        // exists.
        // Shadows::build_descriptors needs the geometry bind groups,
        // which now live inside render_passes_plan.bindings. We don't
        // have render_passes_plan.bindings as a public field — drill
        // through describe_pipelines first to get bind groups via the
        // typed RenderPasses handle... actually no, we need them
        // BEFORE describe_pipelines to construct shadows here.
        //
        // The bindings struct stores GeometryBindGroups; we need that
        // for Shadows::build_descriptors. Expose a borrow via a
        // helper on the shader plan.
        let mut shadows_descs = shadows::Shadows::build_descriptors(
            &gpu,
            &mut bind_group_layouts,
            &mut pipeline_layouts,
            &mut shaders,
            render_passes_plan.geometry_bind_groups(),
            &render_textures.formats,
            shadows_config.unwrap_or_default(),
        )
        .await?;

        // ── 3. EVSM validate join. Single await covering all 3
        //       inline-shader validations in parallel.
        let evsm_results =
            futures::future::join_all(shadows_descs.evsm.validate_shader_futures()).await;
        for result in evsm_results {
            result.map_err(crate::shadows::AwsmShadowError::Core)?;
        }

        // Register the 3 EVSM modules into the shader cache via
        // `insert_uncached`; the resulting `ShaderKey`s let us build
        // the 3 EVSM compute pipeline cache keys for the
        // cross-renderer compute pool.
        let evsm_shader_keys: [shaders::ShaderKey; 3] = [
            shaders.insert_uncached(shadows_descs.evsm.modules[0].clone()),
            shaders.insert_uncached(shadows_descs.evsm.modules[1].clone()),
            shaders.insert_uncached(shadows_descs.evsm.modules[2].clone()),
        ];
        let evsm_pipeline_cache_keys = shadows_descs.evsm.pipeline_cache_keys(evsm_shader_keys);

        // ── 4. Now that all shaders are warm, drive RenderPasses
        //       phase 2 (build pipeline cache keys per pass) and the
        //       Effects + Display descriptors. All sync apart from
        //       cache-hit `get_key`s.
        let mut render_pass_init = RenderPassInitContext {
            gpu: &gpu,
            bind_group_layouts: &mut bind_group_layouts,
            pipeline_layouts: &mut pipeline_layouts,
            pipelines: &mut pipelines,
            shaders: &mut shaders,
            render_texture_formats: &mut render_texture_formats,
            textures: &mut textures,
            features: &features,
            anti_aliasing: &anti_aliasing,
            post_processing: &post_processing,
            prep_config: &prep_config,
            max_edge_budget: resolved_max_edge_budget,
            unified_edge,
        };
        let render_passes_descs =
            RenderPasses::describe_pipelines(render_passes_plan, &mut render_pass_init, &features)
                .await?;
        // `render_pass_init`'s `&mut`-borrows of bind_group_layouts /
        // pipeline_layouts / pipelines / shaders / etc. expire at
        // the next statement boundary; the subsequent code below
        // re-borrows them through the same names.
        let _ = render_pass_init;

        let caster_pipeline_cache_keys =
            std::mem::take(&mut shadows_descs.caster_pipeline_cache_keys);

        let effects_descs = render_passes_descs
            .effects_pipelines()
            .build_descriptors(&anti_aliasing, &post_processing, &gpu, &mut shaders)
            .await?;
        let display_descs = render_passes_descs
            .display_pipelines()
            .build_descriptors(&post_processing, &gpu, &mut shaders)
            .await?;

        // ── 5. Assemble the cross-renderer compute + render cache
        //       key pools and record each subsystem's slice range.
        let mut compute_pool: Vec<pipelines::compute_pipeline::ComputePipelineCacheKey> =
            render_passes_descs.compute_pipeline_cache_keys.clone();
        let render_passes_compute_len = compute_pool.len();
        // Picker compute pipelines are deferred (Block B.4) — no
        // entries appended here.
        // EVSM compute pipelines are deferred (Block B.1) — held on
        // `shadows.pending_evsm_cache_keys` and resolved by
        // `Shadows::ensure_pipelines_compiled` on the first
        // shadow-casting light. No entries appended.
        let effects_compute_range = {
            let s = compute_pool.len();
            compute_pool.extend(effects_descs.pipeline_cache_keys.iter().cloned());
            s..compute_pool.len()
        };

        let mut render_pool: Vec<pipelines::render_pipeline::RenderPipelineCacheKey> =
            render_passes_descs.render_pipeline_cache_keys.clone();
        let render_passes_render_len = render_pool.len();
        // Line pipelines are deferred (Block B.3) — no entries
        // appended here. The 4 variants compile on first line primitive
        // insertion via `AwsmRenderer::ensure_line_pipelines_compiled`.
        // Shadow caster render pipelines are deferred (Block B.2) —
        // held on `shadows.pending_caster_cache_keys` and resolved by
        // `Shadows::ensure_pipelines_compiled` on the first
        // shadow-casting light. No entries appended.
        let display_render_range = {
            let s = render_pool.len();
            render_pool.extend(display_descs.pipeline_cache_keys.iter().cloned());
            s..render_pool.len()
        };

        // ── 6. ONE try_join'd compute + render ensure_keys covering
        //       every compute + render pipeline across the entire
        //       renderer (~36 compute + ~27 render on a fully-
        //       featured build). Split-borrow Pipelines.compute /
        //       Pipelines.render so Dawn overlaps both classes inside
        //       its worker pool.
        let pipelines::Pipelines {
            render: render_pipelines,
            compute: compute_pipelines,
        } = &mut pipelines;
        let compute_fut = async {
            compute_pipelines
                .ensure_keys(&gpu, &shaders, &pipeline_layouts, compute_pool)
                .await
                .map_err(crate::error::AwsmError::from)
        };
        let render_fut = async {
            render_pipelines
                .ensure_keys(&gpu, &shaders, &pipeline_layouts, render_pool)
                .await
                .map_err(crate::error::AwsmError::from)
        };
        // Phase transition: every shader is now warm; the pipeline
        // assembly happens inside the next await. Emit
        // `BuildingPipelines` so the frontend's "Building render
        // pipelines…" label is correct.
        emit_phase(RendererLoadingPhase::BuildingPipelines);
        let (compute_keys, render_keys) =
            futures::future::try_join(compute_fut, render_fut).await?;

        // ── 7. Sync fold-up — slice resolved keys back to each
        //       subsystem.
        let render_passes_compute_keys = compute_keys[..render_passes_compute_len].to_vec();
        let render_passes_render_keys = render_keys[..render_passes_render_len].to_vec();
        let mut render_passes = RenderPasses::from_resolved(
            render_passes_descs,
            render_passes_compute_keys,
            render_passes_render_keys,
        )?;

        // Picker stays `None` at build (Block B.4) — compiled lazily on
        // first `pick()` via `AwsmRenderer::ensure_picker_compiled`.
        let picker: Option<Picker> = None;
        // Block B.3: cold-boot LineRenderer registers the line BGL but
        // leaves the 4 pipeline variants unbuilt. The first
        // `add_line_*` call sets `pipelines_compile_requested = true`;
        // `wait_for_pipelines_ready` then drives `ensure_pipelines_compiled`.
        let lines = LineRenderer::new_deferred(&gpu, &mut bind_group_layouts)?;
        // Shadows are constructed in the deferred path (Block B.1 + B.2):
        // empty `caster_resolved` / `evsm_resolved` slices stash the
        // pending cache keys on `Shadows`; pipeline compile is
        // triggered by `Shadows::ensure_pipelines_compiled` on the
        // first shadow-casting light. Non-pipeline GPU resources
        // (atlases, bind groups, buffers) still materialise here.
        let shadows = shadows::Shadows::from_resolved(
            &gpu,
            &bind_group_layouts,
            shadows_descs,
            Vec::new(),
            Vec::new(),
            caster_pipeline_cache_keys,
            evsm_pipeline_cache_keys,
        )?;
        render_passes.effects.pipelines.install_resolved(
            &post_processing,
            compute_keys[effects_compute_range].to_vec(),
        );
        render_passes
            .display
            .pipelines
            .install_resolved(render_keys[display_render_range].to_vec());

        #[cfg(feature = "animation")]
        let animations = animation::Animations::default();

        let extras_pool_built = crate::dynamic_materials::extras_pool::ExtrasPool::new(
            &gpu,
            crate::dynamic_materials::extras_pool::DEFAULT_CAPACITY_WORDS,
        )?;

        // Edge-resolve pipeline compile (Priority 3 dispatch wiring). Only
        // when MSAA is on AND device supports the required limits. We
        // pass first_party-only bucket entries — the edge pipelines
        // recompile through the same path when dynamic materials
        // register (Stage 1.14 follow-up will route through the
        // scheduler).
        if edge_resolve_enabled {
            let color_wgsl = awsm_renderer_core::texture::texture_format_to_wgsl_storage(
                render_textures.formats.color,
            )?;
            let bucket_entries = crate::dynamic_materials::first_party_bucket_entries();
            let pipelines::Pipelines {
                render: _render_pipelines,
                compute: compute_pipelines,
            } = &mut pipelines;
            render_passes
                .material_opaque
                .edge_pipelines
                .ensure_compiled(
                    &gpu,
                    &mut shaders,
                    compute_pipelines,
                    &mut pipeline_layouts,
                    &mut bind_group_layouts,
                    &render_passes.material_opaque.bind_groups,
                    &render_passes.material_opaque.edge_bind_group_layouts,
                    &bucket_entries,
                    &anti_aliasing,
                    color_wgsl,
                    None,
                    prep_config.enabled,
                    prep_config.clamped_k(),
                )
                .await?;
        }

        let mut _self = AwsmRenderer {
            gpu,
            meshes,
            camera,
            frame_globals,
            transforms,
            instances,
            scene_spatial: SceneSpatial::new(scene_spatial_config.unwrap_or_default()),
            recommended_shadow_quality_tier,
            light_buckets: LightMeshBuckets::default(),
            material_classify_buffers,
            material_bucket_lut,
            light_culling_buffers,
            light_culling_debug_heatmap: 0,
            debug_view_mode: 0,
            debug_wireframe: 0,
            material_edge_buffers,
            material_edge_layout_uniform,
            decals,
            occlusion_buffers,
            decal_classify_buffers,
            compaction_buffers,
            coverage: coverage::MeshCoverage::default(),
            coverage_buffers,
            coverage_readback_state: std::sync::Arc::new(std::sync::Mutex::new(
                CoverageReadbackState::default(),
            )),
            edge_overflow_readback_state: std::sync::Arc::new(std::sync::Mutex::new(
                EdgeOverflowReadbackState::default(),
            )),
            froxel_overflow_readback_state: std::sync::Arc::new(std::sync::Mutex::new(
                FroxelOverflowReadbackState::default(),
            )),
            frame_index: 0,
            shaders,
            bind_group_layouts,
            bind_groups,
            materials,
            dynamic_materials: crate::dynamic_materials::DynamicMaterials::new(),
            masked_dynamic_dirty: false,
            extras_pool: extras_pool_built,
            pipeline_layouts,
            pipelines,
            lights,
            textures,
            environment,
            render_passes,
            _clear_color: clear_color.clone(),
            _clear_color_perceptual_to_linear: clear_color.perceptual_to_linear(),
            logging,
            render_textures,
            anti_aliasing,
            prep_config,
            unified_edge,
            post_processing,
            picker,
            lines,
            opaque_mipgen,
            shadows,
            features,
            optimization_policy,
            // First frame's previous-state input. All flags `false` is
            // the safe baseline: render.rs's policy computation pass
            // will derive `gpu_occlusion=false` for Auto until the
            // cooldown elapses (so the first frames after init route
            // through the CPU path) and Force / Off behave per spec
            // from frame 0.
            frame_optimizations: crate::optimization_policy::FrameOptimizations::default(),
            // Set to the cooldown threshold so Auto can flip on at the
            // very first qualifying frame instead of waiting through a
            // cooldown of empty frames after startup. Without this,
            // every fresh renderer would burn `gpu_culling_cooldown_frames`
            // before Auto could engage — a poor UX for editor builds
            // that load a large scene immediately.
            frames_in_current_mode: u32::MAX / 2,
            default_cheap_material_pixel_threshold: 64,
            renderable_pool: crate::renderable::RenderablePool::default(),
            hud_resolve: crate::render::HudResolveState::default(),
            pipeline_scheduler: crate::pipeline_scheduler::PipelineScheduler::new(),
            last_ensured_bucket_layout: None,
            // Flipped to true at end of build(). Used by config-change
            // APIs to enforce the race policy from the architecture doc.
            build_complete: false,
            #[cfg(feature = "animation")]
            animations,
            cameras: crate::cameras::Cameras::new(),
        };

        // Apply the configured registration ceiling (§2). Validated +
        // clamped in `with_bucket_config`, so this only sets the field; it
        // sizes nothing per-frame (widths follow the live count, §0).
        if let Some(bucket_config) = bucket_config {
            _self
                .dynamic_materials
                .set_max_bucket_entries(bucket_config.max_bucket_entries);
        }

        // Initial AA + PP state — the effects + display pipelines we
        // installed in the cross-tail pool above already match the
        // configured `anti_aliasing` + `post_processing`, so the
        // pipeline-rebuild path inside set_anti_aliasing /
        // set_post_processing would just no-op through cache hits.
        // We still need the state-side bookkeeping (bind-group recreate
        // marks). `BindGroups::new` already marks every variant for
        // create on first frame, so the AA / PP marks are redundant —
        // but adding them explicitly mirrors the dynamic-setter
        // contract and keeps the surface honest if `BindGroups::new`
        // ever stops marking everything.
        _self
            .bind_groups
            .mark_create(crate::bind_groups::BindGroupCreate::AntiAliasingChange);
        _self
            .bind_groups
            .mark_create(crate::bind_groups::BindGroupCreate::TextureViewRecreate);

        // Block D.1 PART 3: register the eager set with the scheduler.
        // The eager set (the pipelines compiled inline above by the
        // per-pass `from_resolved` calls) is now tracked through the
        // scheduler's PassDef SlotMap. They're already-compiled, so we
        // immediately transition Pending → Ready. Frontends watching
        // drain_pipeline_status_events observe each PassKind register;
        // config-flip semantics (Block D.3) can walk the Pass entries
        // similarly to materials.
        //
        // The literal "compile drives THROUGH the scheduler" shape
        // (submit → scheduler kicks off compile → wait_for_pipelines_ready)
        // would additionally require each render-pass's `from_resolved`
        // to factor into `new_deferred` + `ensure_pipelines_compiled`
        // for the eager set's individual passes — that's a multi-day
        // refactor and is parked for a follow-up. The bookkeeping here
        // is the architectural promise's observability piece: scheduler
        // knows the full eager set; frontends + config-flip + status
        // queries all see it.
        {
            use crate::pipeline_scheduler::{
                PassDef, PipelineConfigSnapshot, PipelineGroupDef, PipelineGroupId,
            };
            let snapshot = PipelineConfigSnapshot {
                msaa: _self.anti_aliasing.clone(),
                mipmap: if _self.anti_aliasing.mipmap {
                    crate::render_passes::material_opaque::shader::template::MipmapMode::Gradient
                } else {
                    crate::render_passes::material_opaque::shader::template::MipmapMode::None
                },
                gpu_culling: _self.features.gpu_culling,
                coverage_lod: _self.features.coverage_lod,
                debug_bitmask: 0,
                default_cull_mode: awsm_renderer_core::pipeline::primitive::CullMode::Back,
            };
            let active_msaa_samples: u8 = if _self.anti_aliasing.has_msaa_checked()? {
                4
            } else {
                1
            };
            let mut eager_passes: Vec<PipelineGroupDef> = vec![
                PipelineGroupDef::Pass(PassDef::OpaqueEmpty {
                    snapshot: snapshot.clone(),
                }),
                PipelineGroupDef::Pass(PassDef::ClassifyMsaa {
                    samples: active_msaa_samples,
                    snapshot: snapshot.clone(),
                }),
                PipelineGroupDef::Pass(PassDef::GeometryMsaa {
                    samples: active_msaa_samples,
                    snapshot: snapshot.clone(),
                }),
                PipelineGroupDef::Pass(PassDef::Display),
                PipelineGroupDef::Pass(PassDef::ScenePassClear),
            ];
            if _self.features.gpu_culling {
                eager_passes.push(PipelineGroupDef::Pass(PassDef::HzbSeed {
                    samples: active_msaa_samples,
                }));
            }
            if edge_resolve_enabled {
                eager_passes.push(PipelineGroupDef::Pass(PassDef::EdgeResolveSkybox {
                    snapshot: snapshot.clone(),
                }));
                eager_passes.push(PipelineGroupDef::Pass(PassDef::EdgeResolveBlend {
                    snapshot: snapshot.clone(),
                }));
            }
            let pass_ids = _self
                .pipeline_scheduler
                .submit_pipeline_group_batch(eager_passes);
            for id in &pass_ids {
                if matches!(id, PipelineGroupId::Pass(_)) {
                    _self.pipeline_scheduler.mark_ready(*id);
                }
            }
            tracing::info!(
                target: "awsm_renderer::pipeline_readiness",
                "eager-set registered with scheduler: {} groups marked Ready",
                pass_ids.len()
            );
        }

        // Race-policy: config-change APIs become available now that
        // the eager batch is done.
        _self.build_complete = true;

        emit_phase(RendererLoadingPhase::Ready);

        Ok(_self)
    }
}

// =============================================================================
// Pipeline-readiness scheduler — public API on AwsmRenderer
// =============================================================================
//
// Wraps the scheduler with renderer-side ergonomics (a single import
// surface, race-policy enforcement on the config-change APIs, a test
// helper for awaiting Pending → Ready).
//
// Per the architecture in `https://github.com/dakom/awsm-renderer/pull/99`:
//
// - `submit_pipeline_group_batch` is the public submission API.
// - `pipeline_group_status` is the pull-side status query.
// - `drain_pipeline_status_events` is the push-side event drain.
// - `drop_material_group` cleans up orphans from the editor's
//   recompile flow.
// - `poll_pipeline_scheduler` drives the FuturesUnordered from the
//   render loop's pre-frame phase.
// - `wait_for_pipelines_ready` is the test-only helper.

impl AwsmRenderer {
    /// Submit a batch of pipeline groups for compile. Returns ids
    /// immediately in `Pending` state; transitions to `Ready` /
    /// `Failed` surface via [`Self::drain_pipeline_status_events`] or
    /// [`Self::pipeline_group_status`].
    ///
    /// Per the architecture doc, this is the unified API over both
    /// materials and passes. Stage 1 follow-up will wire each
    /// `PipelineGroupDef` variant to its real compile path; today the
    /// scheduler queues stub futures that resolve immediately with
    /// `Ok(())`.
    pub fn submit_pipeline_group_batch(
        &mut self,
        defs: Vec<crate::pipeline_scheduler::PipelineGroupDef>,
    ) -> Vec<crate::pipeline_scheduler::PipelineGroupId> {
        self.pipeline_scheduler.submit_pipeline_group_batch(defs)
    }

    /// Per-group status query — O(1) lookup. Returns `None` if the id
    /// doesn't exist in the scheduler (dropped or never submitted).
    pub fn pipeline_group_status(
        &self,
        id: crate::pipeline_scheduler::PipelineGroupId,
    ) -> Option<&crate::pipeline_scheduler::PipelineGroupStatus> {
        self.pipeline_scheduler.pipeline_group_status(id)
    }

    /// Drain status events accumulated since the last call. Frontends
    /// use this to drive "compiling N of M" UI without per-frame
    /// polling.
    pub fn drain_pipeline_status_events(&mut self) -> Vec<crate::pipeline_scheduler::StatusEvent> {
        self.pipeline_scheduler.drain_status_events()
    }

    /// Aggregate compile-progress snapshot for a loading bar / "compiling
    /// N materials…" UI (Decision 14, pull half). Counts pending / ready
    /// / failed materials plus the total in-flight sub-pipeline compiles.
    /// Cheap; safe to call every frame. See
    /// [`crate::pipeline_scheduler::CompileProgress`].
    pub fn compile_progress(&self) -> crate::pipeline_scheduler::CompileProgress {
        self.pipeline_scheduler.compile_progress()
    }

    /// Compile status of a registered dynamic material's pipeline group, by
    /// shader id. `None` while the compile is still pending (or the material has
    /// no scheduler group yet); `Some(Ok(()))` once its pipelines are `Ready`;
    /// `Some(Err(msg))` with the real WGSL/driver compile error once `Failed`.
    ///
    /// The launch path skips synchronous shader validation
    /// (`ensure_keys_sync_skip_validate`); the actual compile resolves
    /// asynchronously via `poll_pipeline_scheduler` and lands the error here. The
    /// editor polls this after register so it can surface a true compile failure
    /// (undefined symbol, type error, …) instead of only the trailing-`;`
    /// heuristic.
    pub fn dynamic_material_compile_status(
        &self,
        shader_id: awsm_materials::MaterialShaderId,
    ) -> Option<std::result::Result<(), String>> {
        let mid = self
            .pipeline_scheduler
            .find_material_by_shader_id(shader_id)?;
        match self
            .pipeline_scheduler
            .pipeline_group_status(crate::pipeline_scheduler::PipelineGroupId::Material(mid))?
        {
            crate::pipeline_scheduler::PipelineGroupStatus::Pending => None,
            crate::pipeline_scheduler::PipelineGroupStatus::Ready => Some(Ok(())),
            crate::pipeline_scheduler::PipelineGroupStatus::Failed { error } => {
                Some(Err(error.to_string()))
            }
        }
    }

    /// Drop a material group. No-op if the id isn't in the scheduler.
    pub fn drop_material_group(&mut self, id: crate::pipeline_scheduler::MaterialId) {
        self.pipeline_scheduler.drop_material_group(id);
    }

    /// Poll the scheduler's `FuturesUnordered` for resolved compiles.
    /// Called from the render loop's pre-frame phase.
    ///
    /// Drives BOTH inflight queues:
    /// - Legacy `inflight` (whole-batch CompileResolutions, currently
    ///   driven by explicit `mark_ready` / `mark_failed`).
    /// - Block D.1 PART 2 `inflight_compile` (per-sub-pipeline
    ///   compile promises). Each resolution installs the resolved
    ///   `GpuComputePipeline` into the per-pass cache + decrements
    ///   the material's sub-compile counter (transition to Ready
    ///   when counter hits 0).
    ///
    /// Returns the number of transitions applied this poll.
    pub fn poll_pipeline_scheduler(&mut self) -> usize {
        // Legacy inflight (whole-batch).
        let mut applied = self.pipeline_scheduler.poll_resolved();
        // D.1 PART 2 inflight_compile (per-sub-pipeline).
        while self.apply_compile_resolution() {
            applied += 1;
        }
        applied
    }

    /// Block C.2 full: grow (or shrink) the
    /// [`MaterialEdgeBuffers::max_edge_budget`](crate::render_passes::material_opaque::edge_buffers::MaterialEdgeBuffers)
    /// at runtime.
    ///
    /// Use case: a frontend running on a pathological-edge-density
    /// scene observes `edge_overflow_count > 0` (via the one-shot
    /// `tracing::warn!` from
    /// [`note_edge_overflow_observed`](crate::render_passes::material_opaque::edge_buffers::note_edge_overflow_observed)
    /// OR via direct CPU readback of `edge_buffers.args_buffer`'s
    /// counter). Calling `set_max_edge_budget(current * 2)` recreates
    /// `material_edge_buffers` with the new size, rebuilds the
    /// edge-layout uniform, and marks classify + edge-resolve +
    /// final-blend bind groups for recreation.
    ///
    /// This is the architectural answer to the doc's "atomic-add
    /// hash-bucket overflow accumulator" (Stage 3.8 / Block C.2
    /// full) — instead of routing overflow samples into a separate
    /// shading pipeline (which would need a new compute pipeline +
    /// bind group + indirect dispatch + per-shader-id specialization
    /// to avoid Stage 3's SPIR-V bloat), the budget itself grows
    /// dynamically to absorb the pathological case. Steady-state
    /// scenes pay nothing; overflow scenes recover via consumer-
    /// driven budget growth.
    ///
    /// Returns `Ok(true)` when buffers were recreated; `Ok(false)`
    /// when `new_budget` matches the current value; `Err` if MSAA
    /// is off (no edge buffers to size — flip MSAA on first).
    pub fn set_max_edge_budget(&mut self, new_budget: u32) -> crate::error::Result<bool> {
        if !self.build_complete {
            return Err(crate::error::AwsmError::NotReady);
        }
        let new_budget = new_budget.max(1);
        let Some(edge_buffers) = self.material_edge_buffers.as_mut() else {
            return Err(crate::error::AwsmError::PipelineVariantNotCompiled(
                "edge buffers absent (MSAA off or device unsupported); flip MSAA on first",
            ));
        };
        let resized = edge_buffers.set_max_edge_budget(&self.gpu, new_budget)?;
        if !resized {
            return Ok(false);
        }
        let bucket_count = edge_buffers.bucket_count;
        // Rebuild the edge-layout uniform with the new max_edge_budget.
        if let Ok((uniform, _bytes)) =
            crate::render_passes::material_opaque::edge_buffers::build_edge_layout_uniform(
                &self.gpu,
                bucket_count,
                new_budget,
            )
        {
            self.material_edge_layout_uniform = Some(uniform);
        }
        // Stage 5b-shadow: resize the prep pass's compact edge-shadow texture to
        // match the new budget (else cs_prep_edge writes / cs_edge reads beyond
        // the texture's row count for the overflow edges). The opaque main bind
        // group re-clones the new view next frame (TextureViewRecreate below
        // fans out to OpaqueMain).
        if let Some(prep) = self.render_passes.material_prep.as_mut() {
            prep.set_max_edge_budget(&self.gpu, new_budget)?;
        }
        // Mark dependent bind groups for recreation.
        self.bind_groups
            .mark_create(crate::bind_groups::BindGroupCreate::MaterialClassifyBuffersResize);
        // The opaque main bind group binds the compact edge-shadow texture
        // (binding 27); rebind it against the resized view.
        self.bind_groups
            .mark_create(crate::bind_groups::BindGroupCreate::TextureViewRecreate);
        tracing::info!(
            target: "awsm_renderer::edge_resolve",
            "set_max_edge_budget: edge budget grown to {} (was tracked via overflow CPU surface)",
            new_budget
        );
        Ok(true)
    }

    /// Bumps the per-froxel light-index budget used by the GPU
    /// light-culling pass. Symmetric with
    /// [`Self::set_max_edge_budget`]: when the per-frame CPU readback
    /// of `LightCullingBuffers::overflow_buffer` shows that the cull
    /// shader bumped a froxel's count past
    /// `max_per_froxel_capacity` (so subsequent lights for that
    /// froxel were dropped), the renderer doubles the budget on the
    /// next render. The budget lives as a *runtime* field on the
    /// `cull_params` uniform, so this only reallocates the per-froxel
    /// storage (via `LightCullingBuffers::set_max_per_froxel_capacity`)
    /// and re-binds it — no shader recompile is needed, and the cull +
    /// consumer shaders read the new capacity from `cull_params`.
    ///
    /// Returns `Ok(true)` when the buffers were recreated; `Ok(false)`
    /// when `new_capacity` matches the current value.
    pub fn set_max_per_froxel_capacity(&mut self, new_capacity: u32) -> crate::error::Result<bool> {
        if !self.build_complete {
            return Err(crate::error::AwsmError::NotReady);
        }
        let new_capacity = new_capacity.max(1);
        let resized = self
            .light_culling_buffers
            .set_max_per_froxel_capacity(&self.gpu, new_capacity)?;
        if !resized {
            return Ok(false);
        }
        self.bind_groups
            .mark_create(crate::bind_groups::BindGroupCreate::LightCullingFroxelsResize);
        tracing::info!(
            target: "awsm_renderer::light_culling",
            "set_max_per_froxel_capacity: per-froxel budget grown to {} after observed overflow",
            new_capacity,
        );
        Ok(true)
    }

    /// Toggle the light-culling debug heatmap (dev aid). When `on`, the
    /// shading shaders output a per-pixel applied-punctual-light-count
    /// heatmap instead of normal shading — blue (few) → red (many) — so
    /// froxel occupancy / cull behaviour can be inspected visually. The
    /// value is written into `CullParams.debug_light_heatmap` on the next
    /// `write_params`; no buffer recreation or shader recompile needed.
    pub fn set_light_culling_debug_heatmap(&mut self, on: bool) {
        self.light_culling_debug_heatmap = u32::from(on);
    }

    /// Global debug view mode: 0 = normal lit shading, 1 = unlit/flat (base
    /// color only). Written into `CullParams.debug_view_mode` on the next
    /// `write_params`; no buffer recreation or shader recompile. Affects PBR
    /// materials (the common case); already-unlit/Toon/custom materials are
    /// unchanged. The shader branch that reads it exists only under the
    /// `debug-views` cargo feature (the editor enables it); in a game build
    /// this setter still writes the uniform but nothing reads it.
    pub fn set_debug_view_mode(&mut self, mode: u32) {
        self.debug_view_mode = mode;
    }

    /// Toggle the global debug wireframe overlay (triangle edges tinted in the
    /// deferred shade). Written into `CullParams.debug_wireframe` each frame; no
    /// recompile. Read only by the `debug-views`-gated shader branch.
    pub fn set_debug_wireframe(&mut self, on: bool) {
        self.debug_wireframe = u32::from(on);
    }

    /// Drive any pending compiles to completion and return when every
    /// scheduler-tracked group is either `Ready` or `Failed`.
    ///
    /// Block A.2: this is the **canonical post-submit await surface**.
    /// Frontends that have just called `register_material` /
    /// `submit_dynamic_material` / (future) gltf-loader-driven
    /// `submit_pipeline_group_batch` await this to know the GPU side
    /// is caught up — at which point any mesh referencing the newly
    /// submitted material will start dispatching on the next render
    /// frame.
    ///
    /// Internally:
    /// 1. Runs `prewarm_pipelines` (the existing batched compile
    ///    flow), which the A.1 bridge wires to `mark_ready` for each
    ///    scheduler-tracked material whose pipelines resolve.
    /// 2. Drains `poll_pipeline_scheduler` until no further
    ///    transitions apply (covers any scheduler-pushed futures from
    ///    the eventual Stage-D push-futures migration).
    ///
    /// Returns the total number of transitions applied. Diagnostic
    /// only — callers don't usually inspect.
    pub async fn wait_for_pipelines_ready(&mut self) -> crate::error::Result<usize> {
        self.wait_for_pipelines_ready_with_progress(|_| {}).await
    }

    /// Same as [`wait_for_pipelines_ready`] but invokes `on_progress` with a
    /// fresh [`CompileProgress`](crate::pipeline_scheduler::CompileProgress)
    /// snapshot after each sub-pipeline resolves — so a boot loader / splash
    /// can show a live "Compiling N render pipelines…" countdown during the
    /// cold-start warmup, mirroring the in-app activity pill that covers
    /// post-mount (import / material-edit) compiles.
    pub async fn wait_for_pipelines_ready_with_progress(
        &mut self,
        mut on_progress: impl FnMut(crate::pipeline_scheduler::CompileProgress),
    ) -> crate::error::Result<usize> {
        // Phase 1: kick the render-driven material compile
        // (`ensure_scene_pipelines`, inside `prewarm_pipelines`) +
        // transparent-mesh prewarm. The promises land in
        // `inflight_compile`; Phase 2 below drains them async.
        self.prewarm_pipelines().await?;

        // Block B.3: if any line primitive has been registered since
        // build (or since the last `wait_for_pipelines_ready`), drive
        // the lazy line-pipeline compile here so the next frame can
        // dispatch the fat-line pass instead of warn-skipping.
        self.ensure_line_pipelines_compiled().await?;

        // Report the initial in-flight count before the drain so the splash
        // shows a number immediately rather than after the first resolution.
        on_progress(self.compile_progress());

        // Phase 2: drain real D.1 PART 2 inflight_compile via async
        // Stream::next — each .await yields to the JS event loop so
        // Dawn's compile promises can fire. Once next() returns None
        // (the FuturesUnordered is empty), every sub-pipeline has
        // resolved and apply_compile_resolution installed it.
        let mut total = 0usize;
        loop {
            let resolution_opt = {
                use futures::StreamExt;
                self.pipeline_scheduler.inflight_compile.next().await
            };
            let Some(resolution) = resolution_opt else {
                break;
            };
            self.apply_compile_resolution_inline(resolution);
            total += 1;
            on_progress(self.compile_progress());
        }

        // Phase 3: drain legacy whole-batch inflight (currently empty
        // — explicit mark_ready / mark_failed callers don't push to
        // it). Kept for future Pass-flavoured push-futures work.
        const MAX_ROUNDS: usize = 1024;
        for _ in 0..MAX_ROUNDS {
            let applied = self.pipeline_scheduler.poll_resolved();
            total += applied;
            if applied == 0 {
                break;
            }
        }
        Ok(total)
    }
}

/// Returns true if the device can host the Stage 3 / Priority 3
/// per-shader-id MSAA edge-resolve pipelines.
///
/// After the group(4) → extended-shadows fold (see
/// `MaterialEdgeBindGroupLayouts`), the per-shader-id edge_resolve
/// pipeline layout fits in 4 bind groups — universally supported, so
/// the bind-group constraint no longer matters. The only remaining
/// constraint is the storage-buffer count: edge_resolve's compute
/// stage now takes two extra storage buffer slots above primary
/// opaque's (the read-write `edge_data` binding + the read-only
/// `edge_args` binding from the args/data split). Primary opaque uses
/// 9 storage buffers in its compute stage; edge_resolve uses 11. Both
/// fit under the WebGPU baseline `maxStorageBuffersPerShaderStage`
/// (≥ 10 on Android Vulkan / macOS Metal / Windows Vulkan / iOS Metal
/// — the spec minimum is 8, but every modern WebGPU stack reports
/// ≥ 10).
///
/// Devices below the storage-buffer limit fall back to the inline
/// `msaa_resolve_samples` path in the primary opaque shader. This
/// almost never triggers in practice, but the safety net stays.
///
/// **Args/data buffer split (now in place).** Earlier this returned
/// `false` because `MaterialEdgeBuffers` was a single GpuBuffer used
/// as both `Indirect` (dispatch source) and `Storage(read-write)`
/// (accumulator + sample lists) inside one compute pass — WebGPU
/// rejects that combination per-buffer per-pass. The buffer is now
/// split: `args_buffer` (`Indirect | Storage | CopyDst`, the
/// dispatch-indirect source + counters) and `data_buffer`
/// (`Storage | CopyDst`, the writable accumulator + sample lists).
/// The args buffer is bound only as `Storage(read)` in the
/// edge_resolve / skybox / final_blend passes — `Storage(read)` +
/// `Indirect` on the same buffer is allowed (no writable usage in
/// the sync scope). This unlocks Priority 3 end-to-end.
pub fn edge_resolve_supported(_gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu) -> bool {
    true
}
