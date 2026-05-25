//! High-level renderer API and shared modules.

#![allow(clippy::type_complexity)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::match_like_matches_macro)]
#![allow(clippy::vec_init_then_push)]
pub mod anti_alias;
pub mod bind_group_layout;
pub mod bind_groups;
pub mod bounds;
pub mod buffer;
pub mod camera;
pub mod coverage;
pub mod debug;
pub mod decals;
pub mod environment;
pub mod error;
pub mod features;
pub mod frame_globals;
pub mod frustum;
pub mod instances;
pub mod light_buckets;
pub mod lights;
pub mod materials;
pub mod meshes;
pub mod opaque_mipgen;
pub mod optimization_policy;
pub mod picker;
pub mod pipeline_layouts;
pub mod pipelines;
pub mod post_process;
pub mod raw_mesh;
pub mod render;
pub mod render_passes;
pub mod render_textures;
pub mod renderable;
pub mod scene_spatial;
pub mod shaders;
pub mod shadows;
pub mod textures;
pub mod transforms;
pub mod update;
pub mod web_global;
pub mod workers;
// re-export
pub mod core {
    pub use awsm_renderer_core::*;
}
#[cfg(feature = "animation")]
pub mod animation;

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
use light_buckets::{LightMeshBuckets, MeshLightIndicesGpu};
use lights::Lights;
use materials::Materials;
use meshes::Meshes;
use pipelines::Pipelines;
use scene_spatial::SceneSpatial;
use shaders::{ShaderCacheKey, Shaders};
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
    /// GPU storage buffers backing `light_buckets` for the shader path.
    /// Uploaded per-frame from the transposed buckets.
    pub mesh_light_indices_gpu: MeshLightIndicesGpu,
    /// Per-frame classify-pass output. Holds the per-`shader_id` tile
    /// buckets + indirect-dispatch args the opaque material pipelines
    /// consume.
    pub material_classify_buffers: render_passes::material_classify::buffers::ClassifyBuffers,
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
    /// [`MeshCoverage::ingest`]. `None` when
    /// `features.coverage_lod == false`.
    pub coverage_buffers: Option<render_passes::coverage::buffers::CoverageBuffers>,
    /// State for the coverage readback loop. `Arc<Mutex<…>>` so the
    /// `spawn_local`-detached `mapAsync` future can write back into
    /// it without re-borrowing the renderer — and so it stays
    /// future-proof for the day the renderer moves across threads
    /// (single-threaded today, so the lock is uncontested).
    pub coverage_readback_state: std::sync::Arc<std::sync::Mutex<CoverageReadbackState>>,
    /// Monotonic frame index. Wraps every ~272 years at 60 Hz — safe to
    /// treat as unbounded for any practical session. Drives the
    /// `skin_update_period` gate and other "every Nth frame" cadences.
    pub frame_index: u64,
    pub shaders: Shaders,
    pub materials: Materials,
    pub pipeline_layouts: PipelineLayouts,
    pub pipelines: Pipelines,
    pub lights: Lights,
    pub textures: Textures,
    pub logging: AwsmRendererLogging,
    pub render_textures: RenderTextures,
    pub render_passes: RenderPasses,
    pub environment: Environment,
    pub anti_aliasing: AntiAliasing,
    pub post_processing: PostProcessing,
    pub picker: Picker,
    pub lines: LineRenderer,
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
    // we pick between these on the fly
    _clear_color_perceptual_to_linear: Color,
    _clear_color: Color,

    #[cfg(feature = "animation")]
    pub animations: animation::Animations,
}

/// Compatibility requirements for this renderer.
///
/// `storage_buffers` is the worst-case `maxStorageBuffersPerShaderStage`
/// the opaque-material pass needs. Opaque currently binds:
///   * 8 storage buffers in `@group(0)`: visibility_data,
///     material_mesh_metas, materials, attribute_indices,
///     attribute_data, transforms (packed model + normal — Option E),
///     texture_transforms, instance_attrs.
///   * 1 storage buffer in `@group(1)`: mesh_light_indices.
///
/// Total = 9, leaving 1 spare under a 10-buffer limit. lights +
/// lights_info are uniforms in group(1) (Option F). The per-mesh
/// slice (`light_slice_offset` + `light_slice_count`) is packed into
/// MaterialMeshMeta itself, so no separate slices storage buffer is
/// needed. The transparent pass peaks at 9. Bumping this lower than
/// the binding count will pass adapter compatibility on a device that
/// exactly meets the declared limit, then fail pipeline validation
/// when the shader is compiled.
pub static COMPATIBITLIY_REQUIREMENTS: LazyLock<CompatibilityRequirements> =
    LazyLock::new(|| CompatibilityRequirements {
        storage_buffers: Some(9),
    });

impl AwsmRenderer {
    /// Removes all scene data by rebuilding the renderer state.
    pub async fn remove_all(&mut self) -> crate::error::Result<()> {
        // meh, just recreate the renderer, it's fine
        let renderer = AwsmRendererBuilder::new(self.gpu.clone())
            .with_logging(self.logging.clone())
            .with_clear_color(self._clear_color.clone())
            .with_render_texture_formats(self.render_textures.formats.clone())
            .with_features(self.features.clone())
            .with_optimization_policy(self.optimization_policy.clone())
            .build()
            .await?;

        *self = renderer;
        Ok(())
    }

    /// Returns the active feature gates picked at construction time.
    pub fn features(&self) -> &RendererFeatures {
        &self.features
    }

    /// Force-compile the routinely-used WebGPU pipelines ahead of the
    /// first user-interactive frame, so the first draw doesn't stall
    /// on shader compilation. See [`PERFORMANCE.md §5g`][perf-5g] for
    /// the underlying browser-PSO-cache mechanics and the rationale.
    ///
    /// [perf-5g]: https://github.com/dakom/awsm-renderer/blob/main/docs/PERFORMANCE.md
    ///
    /// ## What's already prewarmed at construction time
    ///
    /// `AwsmRendererBuilder::build()` already compiles, in parallel:
    ///
    /// - **Opaque-compute** material kernels — 12 variants (3
    ///   `MaterialShaderId` × {MSAA on, off} × {mipmaps on, off}) plus
    ///   two empty kernels for the no-meshes case. See
    ///   `MaterialOpaquePipelines::new`.
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
    /// - **Builder-time prewarm** has already compiled the first-party
    ///   opaque material pipelines, the geometry passes, hzb,
    ///   material_classify, effects, decal, shadows, and the picker /
    ///   line variants. Calling this at the end of `build()` finds all
    ///   those keys already cached and returns immediately (single
    ///   tracing span; no GPU work).
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
    /// ## Future work
    ///
    /// - **Dynamic materials** (see `docs/plans/dynamic-materials.md`
    ///   Phase 3): runtime-registered custom shaders. After
    ///   registration, this method should walk `enabled_materials()`
    ///   and warm every (shader_id × attribute-set) combo it returns.
    pub async fn prewarm_pipelines(&mut self) -> crate::error::Result<()> {
        let _maybe_span = if self.logging.render_timings {
            Some(tracing::span!(tracing::Level::INFO, "Prewarm Pipelines").entered())
        } else {
            None
        };

        // Build one request per mesh. `ensure_keys` on both caches
        // dedupes internally by cache key, so we don't need to
        // dedupe at the request level — and dedup'ing here by
        // `(buffer_info, material)` OR-style (the previous
        // pre-existing pattern) misses pairs like (A,M1)(B,M2)(A,M2)
        // when M1 and M2 differ in `has_transmission`, which would
        // leave some meshes with stale pipeline-key map entries.
        let mut requests: Vec<
            crate::render_passes::material_transparent::pipeline::TransparentMeshPipelineRequest,
        > = Vec::new();
        for (mesh_key, mesh) in self.meshes.iter() {
            let buffer_info_key = self.meshes.buffer_info_key(mesh_key)?;
            let has_transmission = self.materials.has_transmission(mesh.material_key);
            requests.push(
                crate::render_passes::material_transparent::pipeline::TransparentMeshPipelineRequest {
                    mesh,
                    mesh_key,
                    buffer_info_key,
                    has_transmission,
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
            (
                "mesh_light_indices",
                self.mesh_light_indices_gpu.upload_stats(),
            ),
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
    /// supporting resource generation (IBL default cubemaps, BRDF
    /// LUT compute, opaque-mipgen pipeline). Quick on every load.
    Init,
    /// The main render-pass pipeline build — `RenderPasses::new`
    /// concurrently with `RenderTextures::new`. On a cold PSO disk
    /// cache this is the dominant cost (Dawn lowers every WGSL
    /// variant to MSL and then compiles every pipeline against the
    /// new module); on a warm cache it's effectively instant.
    /// Shader compilation and pipeline assembly happen interleaved
    /// inside this phase — they're not cleanly separable into two
    /// distinct stages, which is why there's only one phase variant
    /// for both. Includes shadow shaders, geometry, material,
    /// classify, HZB, effects, etc.
    CompilingShaders,
    /// Remaining renderer-side pipeline construction *after* the
    /// main `RenderPasses::new` batch: picker / line-renderer /
    /// material-classify buffers / occlusion / coverage / decal
    /// allocation. Each is light by itself, but they happen serially
    /// so the cumulative wall-clock is visible to the user on cold
    /// loads.
    BuildingPipelines,
    /// Shadow subsystem build (shadow caster + EVSM pipelines) +
    /// anti-aliasing / post-processing setter pipelines. Roughly the
    /// last shader-compile work before the renderer is interactive.
    FinalizingScene,
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
            optimization_policy: crate::optimization_policy::RendererOptimizationPolicy::default(),
            phase_handler: None,
        }
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
    /// `awsm_scene_schema::EditorProject` so the cube-pool size, EVSM
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
    pub fn with_render_texture_formats(mut self, formats: RenderTextureFormats) -> Self {
        self.render_texture_formats = Some(formats);
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
            optimization_policy,
            phase_handler,
        } = self;

        let mut phase_handler = phase_handler;
        let mut emit_phase = |phase: RendererLoadingPhase| {
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

        // Three default-cubemap creations (prefiltered IBL / irradiance
        // IBL / skybox), the BRDF LUT generation, and the opaque
        // mipgen pipeline are independent `&gpu`-only GPU operations.
        // Run their async halves concurrently; the browser interleaves
        // the underlying ImageBitmap decodes + GPU shader/pipeline
        // compiles, knocking several creations worth of wall-clock off
        // `AwsmRendererBuilder::build`. The post-await registration
        // touches the shared `textures` table and must stay sync, so
        // the helpers are split into `prepare_resources` (async,
        // standalone) + `register` (sync, takes `&mut Textures`).
        // OpaqueMipgen joins this block because it's needed later in
        // the builder but has no shared mutable state — keeping it in
        // its prior sequential slot served no purpose.
        let (
            ibl_filtered_resources,
            ibl_irradiance_resources,
            skybox_resources,
            brdf_lut,
            opaque_mipgen,
        ) = futures::future::try_join5(
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
        )
        .await?;

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

        // Parallelise `RenderPasses::new` with `RenderTextures::new`.
        // Both subsystems compile pipelines + shaders against the same
        // shared `&gpu` handle (no contention; WebGPU's device queue
        // serialises internally) but touch disjoint owned state —
        // RenderPasses mutates the shader / pipeline / bind-group-layout
        // caches; RenderTextures::new is self-contained. The
        // `RenderPassInitContext.gpu: &AwsmRendererWebGpu` shape (was
        // `&mut`) is what unblocks the join — see the doc on
        // `RenderPassInitContext`.
        let formats_for_textures = render_texture_formats.clone();
        let bind_groups = BindGroups::new(&features);
        let mut render_pass_init = RenderPassInitContext {
            gpu: &gpu,
            bind_group_layouts: &mut bind_group_layouts,
            pipeline_layouts: &mut pipeline_layouts,
            pipelines: &mut pipelines,
            shaders: &mut shaders,
            render_texture_formats: &mut render_texture_formats,
            textures: &mut textures,
            features: &features,
        };

        // Bulk shader + pipeline compilation begins here. On a cold
        // PSO disk cache (fresh Chrome profile, post-redeploy first
        // load) this is the window where the user actually waits —
        // tens of seconds while Dawn lowers every WGSL variant to
        // MSL and assembles every pipeline. Shader compilation and
        // pipeline assembly are interleaved (each pass batches its
        // own shaders then its own pipelines) so there's no clean
        // boundary between "shaders done" and "pipelines start";
        // the `CompilingShaders` phase covers the whole block.
        emit_phase(RendererLoadingPhase::CompilingShaders);

        let (render_passes, render_textures) =
            futures::future::try_join(RenderPasses::new(&mut render_pass_init, &features), async {
                RenderTextures::new(&gpu, formats_for_textures, &features)
                    .await
                    .map_err(crate::error::AwsmError::from)
            })
            .await?;

        // Main render-pass compile done. Picker / line-renderer /
        // material-classify / occlusion / coverage / decal still
        // need their pipelines built before the first frame;
        // signal the transition so the UI message stops claiming
        // shader compile is in progress.
        emit_phase(RendererLoadingPhase::BuildingPipelines);

        // Pre-warm the shader cache with everything Picker + LineRenderer
        // need before constructing either. Picker has two compute shader
        // variants (multisampled true/false); LineRenderer has one
        // (parameter-free). Issuing all three through `ensure_keys` lets
        // the browser kick off all `compile_shader` calls together and
        // await every `validate_shader` in parallel — see the doc on
        // `Shaders::ensure_keys`. The per-pass constructors then proceed
        // sequentially through `&mut pipelines / &mut shaders`, but the
        // slow part (shader compile) is already done.
        shaders
            .ensure_keys(
                &gpu,
                [
                    ShaderCacheKey::Picker(picker::ShaderCacheKeyPicker {
                        multisampled_geometry: false,
                    }),
                    ShaderCacheKey::Picker(picker::ShaderCacheKeyPicker {
                        multisampled_geometry: true,
                    }),
                    ShaderCacheKey::Line(render_passes::lines::ShaderCacheKeyLine),
                ],
            )
            .await?;

        let picker = Picker::new(
            &gpu,
            &mut bind_group_layouts,
            &mut pipeline_layouts,
            &mut shaders,
            &mut pipelines,
        )
        .await?;

        let lines = LineRenderer::load(
            &gpu,
            &mut bind_group_layouts,
            &mut pipeline_layouts,
            &mut pipelines,
            &mut shaders,
            &render_textures.formats,
        )
        .await?;

        let mesh_light_indices_gpu = MeshLightIndicesGpu::new(&gpu)?;

        // Sized for a small initial viewport; recreated by
        // `ClassifyBuffers::ensure_capacity` on first frame once the
        // real render-texture size is known.
        let material_classify_buffers =
            render_passes::material_classify::buffers::ClassifyBuffers::new(&gpu, 1024)?;

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

        // Shadows + the final anti-aliasing / post-processing
        // pipeline configuration below are the renderer-init tail:
        // pipelines created here are needed for the first frame but
        // they don't dominate the cold-load cost the way the
        // RenderPasses block does. Signal the transition so frontends
        // can swap to a less alarming "Finalising scene…" message.
        emit_phase(RendererLoadingPhase::FinalizingScene);

        let shadows = shadows::Shadows::new(
            &gpu,
            &mut bind_group_layouts,
            &mut pipeline_layouts,
            &mut pipelines,
            &mut shaders,
            &render_passes.geometry.bind_groups,
            &render_textures.formats,
            shadows_config.unwrap_or_default(),
        )
        .await?;

        #[cfg(feature = "animation")]
        let animations = animation::Animations::default();

        let mut _self = AwsmRenderer {
            gpu,
            meshes,
            camera,
            frame_globals,
            transforms,
            instances,
            scene_spatial: SceneSpatial::default(),
            light_buckets: LightMeshBuckets::default(),
            mesh_light_indices_gpu,
            material_classify_buffers,
            decals,
            occlusion_buffers,
            decal_classify_buffers,
            compaction_buffers,
            coverage: coverage::MeshCoverage::default(),
            coverage_buffers,
            coverage_readback_state: std::sync::Arc::new(std::sync::Mutex::new(
                CoverageReadbackState::default(),
            )),
            frame_index: 0,
            shaders,
            bind_group_layouts,
            bind_groups,
            materials,
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
            #[cfg(feature = "animation")]
            animations,
        };

        // need to call these to create pipelines
        _self.set_anti_aliasing(_self.anti_aliasing.clone()).await?;
        _self
            .set_post_processing(_self.post_processing.clone())
            .await?;

        emit_phase(RendererLoadingPhase::Ready);

        Ok(_self)
    }
}
