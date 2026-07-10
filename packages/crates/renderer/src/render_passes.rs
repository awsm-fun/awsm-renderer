//! Render pass orchestration and initialization.

pub mod bloom;
#[cfg(feature = "lod")]
pub mod cluster_lod;
pub mod coverage;
pub mod display;
pub mod effects;
pub mod geometry;
pub mod hzb;
pub mod light_culling;
pub mod lines;
pub mod material_classify;
pub mod material_decal;
pub mod material_opaque;
pub mod material_prep;
pub mod material_transparent;
pub mod occlusion;
pub mod shader_cache_key;
pub mod shader_template;
pub mod shadow_custom_vertex;
pub mod shadow_masked;
pub mod shadow_masked_custom_vertex;
pub mod shared;
pub mod ssr;
pub mod ssr_minz;

use std::ops::Range;

use awsm_renderer_core::renderer::AwsmRendererWebGpu;

use crate::error::Result;
use crate::features::RendererFeatures;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::pipelines::render_pipeline::{RenderPipelineCacheKey, RenderPipelineKey};
use crate::render_passes::effects::render_pass::EffectsRenderPass;
use crate::shaders::ShaderCacheKey;
use crate::{
    bind_group_layout::BindGroupLayouts,
    pipeline_layouts::PipelineLayouts,
    pipelines::Pipelines,
    render_passes::{
        bloom::render_pass::BloomRenderPass,
        ssr::render_pass::SsrRenderPass,
        coverage::render_pass::CoverageRenderPass,
        display::render_pass::DisplayRenderPass,
        geometry::render_pass::GeometryRenderPass,
        hzb::render_pass::HzbRenderPass,
        light_culling::bind_group::LightCullingBindGroups,
        light_culling::pipeline::{LightCullingPipelines, LightCullingPrewarmDescriptors},
        light_culling::render_pass::LightCullingRenderPass,
        material_classify::render_pass::MaterialClassifyRenderPass,
        material_decal::render_pass::MaterialDecalRenderPass,
        material_opaque::render_pass::MaterialOpaqueRenderPass,
        material_prep::bind_group::MaterialPrepBindGroups,
        material_prep::buffers::EdgeShadowBuffer,
        material_prep::render_pass::{
            MaterialPrepPipelines, MaterialPrepPrewarmDescriptors, MaterialPrepRenderPass,
        },
        material_transparent::render_pass::MaterialTransparentRenderPass,
        occlusion::compaction::CompactionRenderPass,
        occlusion::render_pass::OcclusionRenderPass,
    },
    render_textures::RenderTextureFormats,
    shaders::Shaders,
    textures::Textures,
};

#[cfg(feature = "lod")]
use crate::render_passes::cluster_lod::render_pass::ClusterLodRenderPass;

/// Collection of render passes used by the renderer.
pub struct RenderPasses {
    pub geometry: GeometryRenderPass,
    /// Masked (alpha-tested) shadow caster resources — bind group + lazy
    /// pipeline pool for hole-shaped (cutout) shadows. Always present; the pool
    /// stays empty (and routing falls back to the plain solid shadow pipeline)
    /// until a masked material's variant is compiled.
    pub shadow_masked: shadow_masked::ShadowMaskedRenderPass,
    /// Custom-vertex shadow caster resources — lazy pipeline pool + shared zero
    /// uv0 buffer for DISPLACED shadows that match the lit geometry. Always
    /// present; the pool stays empty (routing falls back to the plain solid shadow
    /// pipeline → un-displaced shadow) until a custom-vertex material's variant is
    /// compiled. Reuses `shadow_masked.bind_group` for group 0 (vertex-augmented).
    pub shadow_custom_vertex: shadow_custom_vertex::ShadowCustomVertexRenderPass,
    /// COMBINED masked + custom-vertex shadow caster resources — lazy pipeline
    /// pool + shared zero uv0 buffer for shadows that are BOTH displaced AND
    /// cutout (a material that is Mask AND custom-vertex). Always present; the
    /// pool stays empty (routing falls back via precedence) until such a
    /// material's variant is compiled. Reuses `shadow_masked.bind_group` for
    /// group 0 (vertex-augmented).
    pub shadow_masked_custom_vertex:
        shadow_masked_custom_vertex::ShadowMaskedCustomVertexRenderPass,
    /// GPU mesh-pixel-coverage producer. `None` when
    /// `features.coverage_lod == false`. Consumers read the resulting
    /// `MeshCoverage` table via `is_below_threshold`; with the
    /// producer disabled that always returns `false`, which routes
    /// every consumer to its "above threshold / use the expensive
    /// variant" path — the safe default.
    pub coverage: Option<CoverageRenderPass>,
    /// HZB build pass. `None` when `features.gpu_culling == false`.
    pub hzb: Option<HzbRenderPass>,
    /// GPU occlusion-cull pass. `None` when
    /// `features.gpu_culling == false`.
    pub occlusion: Option<OcclusionRenderPass>,
    /// Compaction `IndirectDrawArgs` pass. `None` when
    /// `features.gpu_culling == false`.
    pub occlusion_compaction: Option<CompactionRenderPass>,
    /// Cluster-LOD per-cluster cut compute pass (Phase B, B.2). `None` when
    /// `features.virtual_geometry == false`. Built eagerly; holds the cut
    /// pipeline + bind-group layout (creating it validates `cluster_cut.wgsl`
    /// on-device). Inert until a cluster mesh loads its buffers.
    #[cfg(feature = "lod")]
    pub cluster_lod: Option<ClusterLodRenderPass>,
    pub light_culling: LightCullingRenderPass,
    pub material_classify: MaterialClassifyRenderPass,
    /// Shared material-prep compute pass (Plan B). Always built (prep is
    /// unconditional); kept as an always-`Some` `Option` so the `if let
    /// Some(prep)` dispatch sites stay valid. Dispatched between classify and
    /// opaque; the opaque deferred path reads its outputs.
    pub material_prep: Option<MaterialPrepRenderPass>,
    /// Decal classify + shading + composite pass. `None` when
    /// `features.decals == false`.
    pub material_decal: Option<MaterialDecalRenderPass>,
    pub material_opaque: MaterialOpaqueRenderPass,
    pub material_transparent: MaterialTransparentRenderPass,
    pub effects: EffectsRenderPass,
    /// COD/Jimenez mip-pyramid bloom pass. Always present; builds a bloom
    /// pyramid from the HDR composite and writes the wide glow into the
    /// full-res `bloom` render texture the effects pass samples. The
    /// per-frame `render()` / `ensure_size` wiring lives in `render.rs`.
    pub bloom: BloomRenderPass,
    /// Screen-space reflections. Self-contained like bloom;
    /// runs after HZB + before the transparent pass, gated per-frame on
    /// `post_processing.ssr.enabled` (records + allocates nothing when off).
    pub ssr: SsrRenderPass,
    /// SSR min-Z (nearest-depth) pyramid build (M2c). `Some` only when
    /// `post_processing.ssr.enabled` at build; the Hi-Z trace descends its
    /// `texture.view_all` to skip empty space. Built + resized like bloom.
    pub ssr_minz: Option<ssr_minz::render_pass::SsrMinzRenderPass>,
    pub display: DisplayRenderPass,
}

/// Phase-1 output of [`RenderPasses::describe_shaders`]: every
/// pass's bind groups + the union of all shader cache keys the
/// pipelines will need. The orchestrator in
/// `AwsmRendererBuilder::build` pools these shader cache keys into
/// one cross-renderer `Shaders::ensure_keys` batch (alongside the
/// tail subsystems' shader cache keys), then hands the result back
/// to [`RenderPasses::describe_pipelines`].
pub struct RenderPassesShaderPlan {
    bindings: RenderPassesBindings,
    pub shader_cache_keys: Vec<ShaderCacheKey>,
}

impl RenderPassesShaderPlan {
    /// Borrow the geometry bind groups so the orchestrator can pass
    /// them to `Shadows::build_descriptors` (which needs the
    /// geometry transform / meta / animation layouts at shadow
    /// pipeline slots 1..=3) before the typed `RenderPasses` is
    /// assembled.
    pub fn geometry_bind_groups(&self) -> &geometry::bind_group::GeometryBindGroups {
        &self.bindings.geometry_bg
    }
}

/// Bind groups + the pre-built static pipelines (transparent /
/// effects / display) that aren't part of the cross-renderer pool
/// at startup. Internal — the orchestrator never inspects this
/// directly; it flows from describe_shaders → describe_pipelines →
/// from_resolved unchanged.
struct RenderPassesBindings {
    geometry_bg: geometry::bind_group::GeometryBindGroups,
    geometry_masked_bg: geometry::masked_bind_group::GeometryMaskedBindGroup,
    geometry_masked_pipelines: geometry::masked_pipeline::GeometryMaskedPipelines,
    geometry_custom_vertex_pipelines:
        geometry::custom_vertex_pipeline::GeometryCustomVertexPipelines,
    shadow_masked_bg: shadow_masked::bind_group::ShadowMaskedBindGroup,
    shadow_masked_pipelines: shadow_masked::pipeline::ShadowMaskedPipelines,
    shadow_custom_vertex_pipelines: shadow_custom_vertex::pipeline::ShadowCustomVertexPipelines,
    shadow_masked_custom_vertex_pipelines:
        shadow_masked_custom_vertex::pipeline::ShadowMaskedCustomVertexPipelines,
    geometry_masked_custom_vertex_pipelines:
        geometry::masked_custom_vertex_pipeline::GeometryMaskedCustomVertexPipelines,
    coverage_bg_single: Option<coverage::bind_group::CoverageBindGroups>,
    coverage_bg_msaa: Option<coverage::bind_group::CoverageBindGroups>,
    hzb_bg: Option<hzb::bind_group::HzbBindGroups>,
    occlusion_bg: Option<occlusion::bind_group::OcclusionBindGroups>,
    compaction_bg: Option<occlusion::compaction::CompactionBindGroups>,
    light_culling_bg: LightCullingBindGroups,
    /// Built eagerly + gated by `virtual_geometry`; passed straight through to
    /// `from_resolved`.
    #[cfg(feature = "lod")]
    cluster_lod: Option<ClusterLodRenderPass>,
    material_prep_bg: MaterialPrepBindGroups,
    classify_bg: material_classify::bind_group::MaterialClassifyBindGroups,
    decal_bg: Option<material_decal::bind_group::MaterialDecalBindGroups>,
    decal_classify_bg: Option<material_decal::classify::bind_group::DecalClassifyBindGroups>,
    opaque_bg: material_opaque::bind_group::MaterialOpaqueBindGroups,
    /// Bind-group layouts for the per-shader-id MSAA edge-resolve
    /// pipelines (Priority 3 in https://github.com/dakom/awsm-renderer/pull/99).
    /// Allocated up-front — cheap; the actual edge_resolve pipelines
    /// compile lazily via the scheduler.
    opaque_edge_bind_group_layouts: material_opaque::edge_bind_group::MaterialEdgeBindGroupLayouts,
    transparent_bg: material_transparent::bind_group::MaterialTransparentBindGroups,
    transparent_pipelines: material_transparent::pipeline::MaterialTransparentPipelines,
    effects_bg: effects::bind_group::EffectsBindGroups,
    effects_pipelines: effects::pipeline::EffectsPipelines,
    display_bg: display::bind_group::DisplayBindGroups,
    display_pipelines: display::pipeline::DisplayPipelines,
}

/// Phase-2 output of [`RenderPasses::describe_pipelines`]: the
/// per-pass pipeline cache keys, concatenated into one compute pool
/// plus one render pool, with per-pass ranges recording which slice
/// of the pool belongs to which pass. The orchestrator concatenates
/// these onto the global cross-renderer pools (alongside Picker,
/// LineRenderer, Shadows, Effects, and Display) then runs a single
/// `try_join`'d `ComputePipelines::ensure_keys` paired with
/// `RenderPipelines::ensure_keys`. The resolved keys are sliced
/// back out inside [`RenderPasses::from_resolved`].
pub struct RenderPassesDescriptors {
    bindings: RenderPassesBindings,
    /// Compute pipeline cache keys for every render pass that uses
    /// compute pipelines. The orchestrator concatenates this onto
    /// the cross-renderer compute pool.
    pub compute_pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
    /// Render pipeline cache keys for every render pass that uses
    /// render pipelines (today only `Geometry`). The orchestrator
    /// concatenates this onto the cross-renderer render pool.
    pub render_pipeline_cache_keys: Vec<RenderPipelineCacheKey>,
    ranges: RenderPassesRanges,
    per_pass_descs: RenderPassesPerPassDescs,
    /// Built outside the cross-renderer pool — the composite uses
    /// an inline WGSL source that bypasses the shared shader cache
    /// and is already batched via `create_render_pipeline_promise` +
    /// `try_join` internally. Only present when `features.decals`.
    material_decal_composite: Option<material_decal::composite::MaterialDecalComposite>,
    /// Tiny initial HZB texture allocated against `ctx.gpu` during
    /// `describe_pipelines` so `from_resolved` doesn't need a gpu
    /// handle to assemble the typed `HzbRenderPass`. Per-frame
    /// resize in `render.rs` reallocates against the live viewport.
    hzb_texture: Option<hzb::texture::HzbTexture>,
    /// Fully-constructed bloom pass. Built in `describe_pipelines` (which has
    /// the gpu handle + async ctx) and moved straight into `from_resolved`'s
    /// output — bloom self-contains its own bind groups + pipelines rather
    /// than joining the cross-renderer pool.
    bloom: BloomRenderPass,
    /// Fully-constructed SSR pass — self-contained like bloom.
    ssr: SsrRenderPass,
    /// Fully-constructed SSR min-Z pyramid pass — `Some` only when SSR is
    /// enabled at build. Self-contained like bloom.
    ssr_minz: Option<ssr_minz::render_pass::SsrMinzRenderPass>,
}

impl RenderPassesDescriptors {
    /// Borrow the Effects pipelines holder so the orchestrator can
    /// run `EffectsPipelines::build_descriptors` against it before
    /// folding the resulting compute pipeline cache keys into the
    /// cross-renderer pool. `install_resolved` is called later via
    /// `RenderPasses::from_resolved` → field access.
    pub fn effects_pipelines(&self) -> &effects::pipeline::EffectsPipelines {
        &self.bindings.effects_pipelines
    }

    /// Borrow the Display pipelines holder so the orchestrator can
    /// run `DisplayPipelines::build_descriptors` against it.
    pub fn display_pipelines(&self) -> &display::pipeline::DisplayPipelines {
        &self.bindings.display_pipelines
    }
}

/// Each pass's slice ranges into the cross-renderer
/// `(compute_pool, render_pool)`. Indexed by `[range]` to extract
/// the resolved keys inside [`RenderPasses::from_resolved`].
struct RenderPassesRanges {
    geometry: Range<usize>,
    coverage_single: Option<Range<usize>>,
    coverage_msaa: Option<Range<usize>>,
    hzb: Option<Range<usize>>,
    occlusion: Option<Range<usize>>,
    compaction: Option<Range<usize>>,
    classify: Range<usize>,
    light_culling: Range<usize>,
    material_prep: Range<usize>,
    decal: Option<Range<usize>>,
    decal_classify: Option<Range<usize>>,
    opaque: Range<usize>,
}

/// Per-pass descriptors carried through to `from_resolved`. Each
/// pass's typed `from_resolved` may need information that wasn't
/// captured in the cache keys themselves (e.g. the opaque pass's
/// `slots`, or the decal pass's `is_msaa`).
struct RenderPassesPerPassDescs {
    geometry: crate::render_passes::geometry::pipeline::GeometryPrewarmDescriptors,
    light_culling: LightCullingPrewarmDescriptors,
    material_prep: MaterialPrepPrewarmDescriptors,
    /// Stage 5b-shadow: the compact edge-shadow texture, allocated in
    /// `describe_pipelines` (which has a gpu handle) only when MSAA is on and
    /// the device supports edge resolve — `from_resolved` is sync.
    prep_edge_shadow: Option<EdgeShadowBuffer>,
    opaque_slots: Vec<crate::render_passes::material_opaque::pipeline::OpaquePipelineSlot>,
    /// One slot per entry in the classify pass's pipeline pool —
    /// records the `msaa_sample_count` so `from_resolved` can route
    /// each compiled pipeline into the matching `Option` field on
    /// `MaterialClassifyPipelines`. Lazy-pool: the pool typically
    /// has just 1 entry (the live MSAA's variant).
    classify_slot_msaa: Vec<Option<u32>>,
    /// Slot identity per HZB pipeline pool entry. Lazy-pool: the
    /// pool has 2 entries (1 seed + 1 reduce) for the live config.
    hzb_slot: Vec<crate::render_passes::hzb::pipeline::HzbPipelineSlot>,
    decal_is_msaa: Option<Vec<bool>>,
}

impl RenderPasses {
    /// Thin wrapper for callers that don't need to pool with other
    /// subsystems: runs the 3-stage construction
    /// ([`Self::describe_shaders`] →
    /// `ctx.shaders.ensure_keys(...)` →
    /// [`Self::describe_pipelines`] → two `ensure_keys` →
    /// [`Self::from_resolved`]). The cross-renderer pooled path in
    /// `AwsmRendererBuilder::build` drives the three phases
    /// explicitly so it can fold the cache keys into shared pools.
    pub async fn new<'a>(
        ctx: &mut RenderPassInitContext<'a>,
        features: &RendererFeatures,
    ) -> Result<Self> {
        let mut plan = Self::describe_shaders(ctx, features).await?;
        // `mem::take` rather than `clone`: `describe_pipelines`
        // reads `plan.bindings` only, never `plan.shader_cache_keys`,
        // so we can move the Vec out and leave the field empty.
        let shader_keys = std::mem::take(&mut plan.shader_cache_keys);
        ctx.shaders.ensure_keys(ctx.gpu, shader_keys).await?;
        let mut descs = Self::describe_pipelines(plan, ctx, features).await?;
        // Same trick for the pipeline pools: `from_resolved` consumes
        // `descs` but doesn't read either pipeline_cache_keys Vec
        // (it slices the resolved-keys Vecs the orchestrator passes
        // back), so move the pools out instead of cloning.
        let compute_pool = std::mem::take(&mut descs.compute_pipeline_cache_keys);
        let render_pool = std::mem::take(&mut descs.render_pipeline_cache_keys);
        let compute_keys = ctx
            .pipelines
            .compute
            .ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts, compute_pool)
            .await?;
        let render_keys = ctx
            .pipelines
            .render
            .ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts, render_pool)
            .await?;
        Self::from_resolved(descs, compute_keys, render_keys)
    }

    /// Phase 1 — bind groups + shader cache keys. Sync apart from
    /// the per-pass `BindGroups::new` awaits (which are cheap; they
    /// only register layouts into the shared cache, no Dawn compile).
    /// Returns the union of every pass's shader cache keys for the
    /// orchestrator to pool into one cross-renderer
    /// `Shaders::ensure_keys` batch.
    pub async fn describe_shaders<'a>(
        ctx: &mut RenderPassInitContext<'a>,
        features: &RendererFeatures,
    ) -> Result<RenderPassesShaderPlan> {
        use crate::render_passes::coverage::pipeline::CoveragePipelines;
        use crate::render_passes::geometry::pipeline::GeometryPipelines;
        use crate::render_passes::hzb::pipeline::HzbPipelines;
        use crate::render_passes::material_classify::pipeline::MaterialClassifyPipelines;
        use crate::render_passes::material_decal::classify::pipeline::DecalClassifyPipelines;
        use crate::render_passes::material_decal::pipeline::MaterialDecalPipelines;
        use crate::render_passes::material_opaque::pipeline::MaterialOpaquePipelines;
        use crate::render_passes::occlusion::compaction::CompactionPipeline;
        use crate::render_passes::occlusion::pipeline::OcclusionPipelines;

        // ----------------------------------------------------------
        // Phase 1 — sync bind-group setup + auxiliary resources
        // ----------------------------------------------------------
        let geometry_bg = geometry::bind_group::GeometryBindGroups::new(ctx).await?;
        // Masked (alpha-tested) geometry variant: augmented group-0 bind group +
        // an empty lazy pipeline pool. Pipelines compile later (built-in PBR in
        // the texture-finalize flow; custom via the dynamic scheduler).
        let geometry_masked_bg =
            geometry::masked_bind_group::GeometryMaskedBindGroup::new(ctx).await?;
        let geometry_masked_pipelines = geometry::masked_pipeline::GeometryMaskedPipelines::new(
            ctx,
            &geometry_masked_bg,
            &geometry_bg,
        )?;
        // Custom-vertex geometry variant: reuses the masked group-0 bind group +
        // an empty lazy pipeline pool. Pipelines compile later in the
        // texture-finalize flow (parallel to the geometry masked pool).
        let geometry_custom_vertex_pipelines =
            geometry::custom_vertex_pipeline::GeometryCustomVertexPipelines::new(
                ctx,
                &geometry_masked_bg,
                &geometry_bg,
            )?;
        // Masked (alpha-tested) shadow caster: augmented group-0 bind group +
        // an empty lazy pipeline pool. Pipelines compile later in the
        // texture-finalize flow (parallel to the geometry masked pool).
        let shadow_masked_bg = shadow_masked::bind_group::ShadowMaskedBindGroup::new(ctx)?;
        let shadow_masked_pipelines = shadow_masked::pipeline::ShadowMaskedPipelines::new(
            ctx,
            &shadow_masked_bg,
            &geometry_bg,
        )?;
        // Custom-vertex shadow caster: reuses the (vertex-augmented) masked-shadow
        // group-0 bind group + an empty lazy pipeline pool + a shared zero uv0
        // buffer. Pipelines compile later in the texture-finalize flow (parallel
        // to the masked-shadow pool).
        let shadow_custom_vertex_pipelines =
            shadow_custom_vertex::pipeline::ShadowCustomVertexPipelines::new(
                ctx,
                &shadow_masked_bg,
                &geometry_bg,
            )?;
        // Combined masked + custom-vertex geometry variant: reuses the masked
        // group-0 bind group + an empty lazy pipeline pool. Pipelines compile
        // later in the texture-finalize flow (parallel to the masked + plain
        // custom-vertex pools).
        let geometry_masked_custom_vertex_pipelines =
            geometry::masked_custom_vertex_pipeline::GeometryMaskedCustomVertexPipelines::new(
                ctx,
                &geometry_masked_bg,
                &geometry_bg,
            )?;
        // Combined masked + custom-vertex shadow caster: reuses the
        // (vertex-augmented) masked-shadow group-0 bind group + an empty lazy
        // pipeline pool + a shared zero uv0 buffer. Pipelines compile later in
        // the texture-finalize flow.
        let shadow_masked_custom_vertex_pipelines =
            shadow_masked_custom_vertex::pipeline::ShadowMaskedCustomVertexPipelines::new(
                ctx,
                &shadow_masked_bg,
                &geometry_bg,
            )?;
        let (coverage_bg_single, coverage_bg_msaa) = if features.coverage_lod {
            (
                Some(coverage::bind_group::CoverageBindGroups::new(ctx, false).await?),
                Some(coverage::bind_group::CoverageBindGroups::new(ctx, true).await?),
            )
        } else {
            (None, None)
        };
        let hzb_bg = if features.gpu_culling {
            Some(hzb::bind_group::HzbBindGroups::new(ctx).await?)
        } else {
            None
        };
        let occlusion_bg = if features.gpu_culling {
            Some(occlusion::bind_group::OcclusionBindGroups::new(ctx).await?)
        } else {
            None
        };
        let compaction_bg = if features.gpu_culling {
            Some(occlusion::compaction::CompactionBindGroups::new(ctx).await?)
        } else {
            None
        };
        // Light culling + material prep: bind groups only here — their
        // shader/pipeline cache keys join the cross-renderer pool below
        // (phase 2), like every other pooled pass. Previously both compiled
        // eagerly inside their `new()` (sequential single-pipeline awaits);
        // the prep megashader alone was ~2.6s of cold boot.
        let light_culling_bg = LightCullingBindGroups::new(ctx).await?;
        // Cluster-LOD cut pass (Phase B). Eager + gated; creating its pipeline
        // validates `cluster_cut.wgsl` on-device. Buffers/bind-group instance
        // come when a cluster mesh loads.
        #[cfg(feature = "lod")]
        let cluster_lod = if features.virtual_geometry {
            Some(ClusterLodRenderPass::new(ctx).await?)
        } else {
            None
        };
        let material_prep_bg = MaterialPrepBindGroups::new(ctx).await?;
        let classify_bg =
            material_classify::bind_group::MaterialClassifyBindGroups::new(ctx).await?;
        let (decal_bg, decal_classify_bg) = if features.decals {
            (
                Some(material_decal::bind_group::MaterialDecalBindGroups::new(ctx).await?),
                Some(
                    material_decal::classify::bind_group::DecalClassifyBindGroups::new(ctx).await?,
                ),
            )
        } else {
            (None, None)
        };
        let opaque_bg = material_opaque::bind_group::MaterialOpaqueBindGroups::new(ctx).await?;
        let opaque_edge_bind_group_layouts =
            material_opaque::edge_bind_group::MaterialEdgeBindGroupLayouts::new(ctx)?;
        let transparent_bg =
            material_transparent::bind_group::MaterialTransparentBindGroups::new(ctx).await?;
        let effects_bg = effects::bind_group::EffectsBindGroups::new(ctx).await?;
        let display_bg = display::bind_group::DisplayBindGroups::new(ctx).await?;

        // Pre-build the static-pipeline subsystems whose pipelines
        // aren't part of the cross-renderer pool: transparent (no
        // per-mesh pipelines at startup; transparents compile
        // during gltf populate via
        // `set_render_pipeline_keys_batched`), effects (5 pipelines
        // compile only after AA + PP config is known, through the
        // orchestrator pool in `AwsmRendererBuilder::build`), and
        // display (1 pipeline, ditto). These constructors only
        // register bind-group + pipeline layouts; no Dawn compile.
        let transparent_pipelines =
            material_transparent::pipeline::MaterialTransparentPipelines::new(ctx, &transparent_bg)
                .await?;
        let effects_pipelines = effects::pipeline::EffectsPipelines::new(ctx, &effects_bg).await?;
        let display_pipelines = display::pipeline::DisplayPipelines::new(ctx, &display_bg).await?;

        // Collect every shader cache key the pipeline-pool phase
        // will need. The orchestrator concatenates this onto the
        // cross-renderer shader pool — see `AwsmRendererBuilder::build`.
        let mut shader_cache_keys: Vec<ShaderCacheKey> = Vec::new();
        // Geometry MSAA-lazy: only the active branch's 3 shader keys
        // at cold-boot. Inactive branch fills on first
        // set_anti_aliasing flip.
        let multisampled_geometry = ctx.anti_aliasing.has_msaa_checked()?;
        shader_cache_keys.extend(GeometryPipelines::shader_cache_keys(multisampled_geometry));
        // Light culling: one module, two entry points, MSAA-agnostic.
        shader_cache_keys.extend(LightCullingPipelines::shader_cache_keys());
        // Material prep: active MSAA branch only (the megashader module also
        // covers cs_prep_edge); the blur module rides along while denoise is
        // configured on.
        shader_cache_keys.extend(MaterialPrepPipelines::shader_cache_keys(
            multisampled_geometry,
            ctx.prep_config,
        ));
        if features.gpu_culling {
            shader_cache_keys.extend(HzbPipelines::shader_cache_keys(ctx.anti_aliasing));
            shader_cache_keys.extend(OcclusionPipelines::shader_cache_keys());
            shader_cache_keys.extend(CompactionPipeline::shader_cache_keys(features));
        }
        // Builder-time prewarm — no dynamic materials can be registered
        // before `AwsmRendererBuilder::build` returns, so the bucket
        // list is the first-party-only baseline. Mid-session
        // `register_material` changes the bucket list, which changes the
        // classify shader's cache key and triggers a recompile via the
        // same `ensure_keys` plumbing the orchestrator uses.
        let first_party_entries = crate::dynamic_materials::first_party_bucket_entries();
        shader_cache_keys.extend(MaterialClassifyPipelines::shader_cache_keys(
            ctx.gpu,
            &first_party_entries,
            ctx.anti_aliasing,
        ));
        if let Some(bg) = coverage_bg_single.as_ref() {
            shader_cache_keys.extend(CoveragePipelines::shader_cache_keys(bg));
        }
        if let Some(bg) = coverage_bg_msaa.as_ref() {
            shader_cache_keys.extend(CoveragePipelines::shader_cache_keys(bg));
        }
        if let Some(bg) = decal_bg.as_ref() {
            shader_cache_keys.extend(MaterialDecalPipelines::build_shader_cache_keys(ctx, bg)?);
        }
        if let Some(bg) = decal_classify_bg.as_ref() {
            shader_cache_keys.extend(DecalClassifyPipelines::shader_cache_keys(bg));
        }
        shader_cache_keys.extend(MaterialOpaquePipelines::build_shader_cache_keys(
            ctx, &opaque_bg,
        )?);

        Ok(RenderPassesShaderPlan {
            bindings: RenderPassesBindings {
                geometry_bg,
                geometry_masked_bg,
                geometry_masked_pipelines,
                geometry_custom_vertex_pipelines,
                shadow_masked_bg,
                shadow_masked_pipelines,
                shadow_custom_vertex_pipelines,
                shadow_masked_custom_vertex_pipelines,
                geometry_masked_custom_vertex_pipelines,
                coverage_bg_single,
                coverage_bg_msaa,
                hzb_bg,
                occlusion_bg,
                compaction_bg,
                light_culling_bg,
                #[cfg(feature = "lod")]
                cluster_lod,
                material_prep_bg,
                classify_bg,
                decal_bg,
                decal_classify_bg,
                opaque_bg,
                opaque_edge_bind_group_layouts,
                transparent_bg,
                transparent_pipelines,
                effects_bg,
                effects_pipelines,
                display_bg,
                display_pipelines,
            },
            shader_cache_keys,
        })
    }

    /// Phase 2 — with the shader cache warm, build every per-pass
    /// pipeline cache key into one compute pool + one render pool,
    /// recording per-pass slice ranges for [`Self::from_resolved`].
    /// Sync apart from cache-hit `shaders.get_key` calls inside each
    /// pass's `build_descriptors` (which the cross-renderer
    /// `Shaders::ensure_keys` makes sub-millisecond) and the
    /// `MaterialDecalComposite::new` call (which uses inline WGSL +
    /// internal `try_join`, staying outside the cross-renderer pool
    /// by design — its 2 pipelines bypass the shared shader cache).
    pub async fn describe_pipelines<'a>(
        plan: RenderPassesShaderPlan,
        ctx: &mut RenderPassInitContext<'a>,
        _features: &RendererFeatures,
    ) -> Result<RenderPassesDescriptors> {
        use crate::render_passes::coverage::pipeline::CoveragePipelines;
        use crate::render_passes::geometry::pipeline::GeometryPipelines;
        use crate::render_passes::hzb::pipeline::HzbPipelines;
        use crate::render_passes::material_classify::pipeline::MaterialClassifyPipelines;
        use crate::render_passes::material_decal::classify::pipeline::DecalClassifyPipelines;
        use crate::render_passes::material_decal::pipeline::MaterialDecalPipelines;
        use crate::render_passes::material_opaque::pipeline::MaterialOpaquePipelines;
        use crate::render_passes::occlusion::compaction::CompactionPipeline;
        use crate::render_passes::occlusion::pipeline::OcclusionPipelines;

        let bindings = plan.bindings;
        let mut compute_pool: Vec<ComputePipelineCacheKey> = Vec::new();
        let mut render_pool: Vec<RenderPipelineCacheKey> = Vec::new();

        // Geometry MSAA-lazy: only the active branch's 9 descriptors.
        let multisampled_geometry = ctx.anti_aliasing.has_msaa_checked()?;
        let geometry_descs =
            GeometryPipelines::build_descriptors(ctx, &bindings.geometry_bg, multisampled_geometry)
                .await?;
        let geometry_range =
            render_pool.len()..render_pool.len() + geometry_descs.pipeline_cache_keys.len();
        render_pool.extend(geometry_descs.pipeline_cache_keys.iter().cloned());

        let (hzb_range, hzb_slot) = if let Some(bg) = bindings.hzb_bg.as_ref() {
            let descs = HzbPipelines::build_descriptors(ctx, bg).await?;
            let start = compute_pool.len();
            let end = start + descs.pipeline_cache_keys.len();
            compute_pool.extend(descs.pipeline_cache_keys.iter().cloned());
            (Some(start..end), descs.slot)
        } else {
            (None, Vec::new())
        };
        let occlusion_range = if let Some(bg) = bindings.occlusion_bg.as_ref() {
            let descs = OcclusionPipelines::build_descriptors(ctx, bg).await?;
            let start = compute_pool.len();
            let end = start + descs.pipeline_cache_keys.len();
            compute_pool.extend(descs.pipeline_cache_keys.iter().cloned());
            Some(start..end)
        } else {
            None
        };
        let compaction_range = if let Some(bg) = bindings.compaction_bg.as_ref() {
            let descs = CompactionPipeline::build_descriptors(ctx, bg).await?;
            let start = compute_pool.len();
            let end = start + descs.pipeline_cache_keys.len();
            compute_pool.extend(descs.pipeline_cache_keys.iter().cloned());
            Some(start..end)
        } else {
            None
        };

        let classify_first_party_entries = crate::dynamic_materials::first_party_bucket_entries();
        let classify_descs = MaterialClassifyPipelines::build_descriptors(
            ctx,
            &bindings.classify_bg,
            &classify_first_party_entries,
        )
        .await?;
        let classify_range =
            compute_pool.len()..compute_pool.len() + classify_descs.pipeline_cache_keys.len();
        compute_pool.extend(classify_descs.pipeline_cache_keys.iter().cloned());

        // Light culling: 2 keys (cs_main + cs_tile), one shared module.
        let light_culling_descs =
            LightCullingPipelines::build_descriptors(ctx, &bindings.light_culling_bg).await?;
        let light_culling_range =
            compute_pool.len()..compute_pool.len() + light_culling_descs.pipeline_cache_keys.len();
        compute_pool.extend(light_culling_descs.pipeline_cache_keys.iter().cloned());

        // Material prep: ACTIVE MSAA branch only (the other branch fills on the
        // first set_anti_aliasing flip); cs_prep_edge + the compact edge-shadow
        // texture only when the MSAA edge-resolve path is actually live; the
        // blur pair only while denoise is configured on.
        let edge_resolve_enabled = multisampled_geometry && crate::edge_resolve_supported(ctx.gpu);
        let material_prep_descs = MaterialPrepPipelines::build_descriptors_for_config(
            ctx,
            &bindings.material_prep_bg,
            multisampled_geometry,
            edge_resolve_enabled,
        )
        .await?;
        let material_prep_range =
            compute_pool.len()..compute_pool.len() + material_prep_descs.pipeline_cache_keys.len();
        compute_pool.extend(material_prep_descs.pipeline_cache_keys.iter().cloned());
        // Allocated here (gpu handle in scope; `from_resolved` is sync).
        // Gated on MSAA alone — NOT on `edge_resolve_supported` — because the
        // opaque MSAA main bind group binds this view (binding 27)
        // unconditionally under MSAA; only the cs_prep_edge pipeline is
        // additionally support-gated. Single-sampled sessions skip the ~8 MB.
        let prep_edge_shadow = if multisampled_geometry {
            Some(EdgeShadowBuffer::new(
                ctx.gpu,
                ctx.max_edge_budget,
                ctx.prep_config.shadow_visibility_layers(),
            )?)
        } else {
            None
        };

        let coverage_single_range = if let Some(bg) = bindings.coverage_bg_single.as_ref() {
            let descs = CoveragePipelines::build_descriptors(ctx, bg).await?;
            let start = compute_pool.len();
            let end = start + descs.pipeline_cache_keys.len();
            compute_pool.extend(descs.pipeline_cache_keys.iter().cloned());
            Some(start..end)
        } else {
            None
        };
        let coverage_msaa_range = if let Some(bg) = bindings.coverage_bg_msaa.as_ref() {
            let descs = CoveragePipelines::build_descriptors(ctx, bg).await?;
            let start = compute_pool.len();
            let end = start + descs.pipeline_cache_keys.len();
            compute_pool.extend(descs.pipeline_cache_keys.iter().cloned());
            Some(start..end)
        } else {
            None
        };

        let (decal_range, decal_classify_range, decal_is_msaa) =
            if let (Some(decal_bg), Some(decal_classify_bg)) = (
                bindings.decal_bg.as_ref(),
                bindings.decal_classify_bg.as_ref(),
            ) {
                let descs = MaterialDecalPipelines::build_descriptors(ctx, decal_bg).await?;
                let start = compute_pool.len();
                let end = start + descs.pipeline_cache_keys.len();
                compute_pool.extend(descs.pipeline_cache_keys.iter().cloned());
                let decal_range = start..end;
                let is_msaa = descs.is_msaa;

                let classify_descs =
                    DecalClassifyPipelines::build_descriptors(ctx, decal_classify_bg).await?;
                let start = compute_pool.len();
                let end = start + classify_descs.pipeline_cache_keys.len();
                compute_pool.extend(classify_descs.pipeline_cache_keys.iter().cloned());
                let decal_classify_range = start..end;

                (Some(decal_range), Some(decal_classify_range), Some(is_msaa))
            } else {
                (None, None, None)
            };

        let opaque_descs =
            MaterialOpaquePipelines::build_descriptors(ctx, &bindings.opaque_bg).await?;
        let opaque_range =
            compute_pool.len()..compute_pool.len() + opaque_descs.pipeline_cache_keys.len();
        compute_pool.extend(opaque_descs.pipeline_cache_keys.iter().cloned());

        // Decal composite — deferred-boot: NOT built here anymore. Its two
        // inline-WGSL pipelines compile in `ensure_config_pipelines`
        // (`ensure_decal_composite_compiled`), so build() stays compile-free.
        let material_decal_composite: Option<material_decal::composite::MaterialDecalComposite> =
            None;

        // HZB texture — tiny initial allocation, recreated against
        // the live viewport on the first frame. Allocated here
        // because `from_resolved` is sync and doesn't have a gpu
        // handle; this is sub-millisecond GPU work.
        let hzb_texture = if bindings.hzb_bg.is_some() {
            Some(hzb::texture::HzbTexture::new(ctx.gpu, 1, 1)?)
        } else {
            None
        };

        // Bloom — self-contained (own bind groups + pipelines + params + tiny
        // initial texture). Built here where the async ctx + gpu handle are
        // available; moved unchanged into `from_resolved`'s output.
        let bloom = BloomRenderPass::new(ctx).await?;

        // SSR — self-contained like bloom (own bind groups + pipeline + params).
        // Its reflection target lives in RenderTextures; the pass just needs its
        // bind group recreated once the views exist (via bind_groups.rs).
        let ssr = SsrRenderPass::new(ctx).await?;

        // SSR min-Z pyramid (M2c) — self-contained like bloom + the SSR pass
        // itself: built at boot whenever Hi-Z is the PRODUCTION trace, so it
        // exists to satisfy the SSR trace layout's always-present pyramid binding
        // (the layout's `hiz` flag is the SAME compile-const, so the two can
        // never disagree — this is what the earlier `ssr.enabled` gate got wrong:
        // SSR is disabled at boot but enabled later without rebuilding passes, so
        // the pyramid was absent exactly when the layout demanded it → white
        // frame). When DDA is PRODUCTION the pyramid is never read, so `is_hiz()`
        // gates it to `None` (zero-cost). The texture stays 1×1 until SSR is
        // actually enabled (see the `ssr.enabled`-gated `ensure_size` in
        // render.rs), so an inactive Hi-Z SSR still costs only a 1×1 pyramid.
        let ssr_minz = if crate::render_passes::ssr::shader::cache_key::SsrTrace::PRODUCTION.is_hiz()
        {
            Some(ssr_minz::render_pass::SsrMinzRenderPass::new(ctx).await?)
        } else {
            None
        };

        Ok(RenderPassesDescriptors {
            bindings,
            compute_pipeline_cache_keys: compute_pool,
            render_pipeline_cache_keys: render_pool,
            ranges: RenderPassesRanges {
                geometry: geometry_range,
                coverage_single: coverage_single_range,
                coverage_msaa: coverage_msaa_range,
                hzb: hzb_range,
                occlusion: occlusion_range,
                compaction: compaction_range,
                classify: classify_range,
                light_culling: light_culling_range,
                material_prep: material_prep_range,
                decal: decal_range,
                decal_classify: decal_classify_range,
                opaque: opaque_range,
            },
            per_pass_descs: RenderPassesPerPassDescs {
                geometry: geometry_descs,
                light_culling: light_culling_descs,
                material_prep: material_prep_descs,
                prep_edge_shadow,
                opaque_slots: opaque_descs.slots,
                classify_slot_msaa: classify_descs.slot_msaa,
                hzb_slot,
                decal_is_msaa,
            },
            material_decal_composite,
            hzb_texture,
            bloom,
            ssr,
            ssr_minz,
        })
    }

    /// Phase 3 — sync fold-up. Each pass's typed `from_resolved`
    /// consumes its slice of the resolved compute + render keys.
    /// `compute_keys` / `render_keys` are typically the resolved
    /// outputs of the cross-renderer
    /// `ComputePipelines::ensure_keys` / `RenderPipelines::ensure_keys`
    /// in `AwsmRendererBuilder::build`, sliced via
    /// [`RenderPassesDescriptors::compute_pipeline_cache_keys`] /
    /// [`RenderPassesDescriptors::render_pipeline_cache_keys`]
    /// ranges. Sync; no Dawn / GPU calls.
    pub fn from_resolved(
        descs: RenderPassesDescriptors,
        compute_keys: Vec<ComputePipelineKey>,
        render_keys: Vec<RenderPipelineKey>,
    ) -> Result<Self> {
        use crate::render_passes::coverage::pipeline::CoveragePipelines;
        use crate::render_passes::geometry::pipeline::GeometryPipelines;
        use crate::render_passes::hzb::pipeline::HzbPipelines;
        use crate::render_passes::material_decal::classify::pipeline::DecalClassifyPipelines;
        use crate::render_passes::material_decal::pipeline::MaterialDecalPipelines;
        use crate::render_passes::material_opaque::pipeline::MaterialOpaquePipelines;
        use crate::render_passes::occlusion::compaction::CompactionPipeline;
        use crate::render_passes::occlusion::pipeline::OcclusionPipelines;

        let RenderPassesDescriptors {
            bindings,
            ranges,
            per_pass_descs,
            material_decal_composite,
            hzb_texture,
            bloom,
            ssr,
            ssr_minz,
            ..
        } = descs;
        let RenderPassesBindings {
            geometry_bg,
            geometry_masked_bg,
            geometry_masked_pipelines,
            geometry_custom_vertex_pipelines,
            shadow_masked_bg,
            shadow_masked_pipelines,
            shadow_custom_vertex_pipelines,
            shadow_masked_custom_vertex_pipelines,
            geometry_masked_custom_vertex_pipelines,
            coverage_bg_single,
            coverage_bg_msaa,
            hzb_bg,
            occlusion_bg,
            compaction_bg,
            light_culling_bg,
            #[cfg(feature = "lod")]
            cluster_lod,
            material_prep_bg,
            classify_bg,
            decal_bg,
            decal_classify_bg,
            opaque_bg,
            opaque_edge_bind_group_layouts,
            transparent_bg,
            transparent_pipelines,
            effects_bg,
            effects_pipelines,
            display_bg,
            display_pipelines,
        } = bindings;

        let geometry = GeometryRenderPass {
            bind_groups: geometry_bg,
            pipelines: GeometryPipelines::from_resolved(
                &per_pass_descs.geometry,
                render_keys[ranges.geometry].to_vec(),
            )?,
            masked_bind_group: geometry_masked_bg,
            masked_pipelines: geometry_masked_pipelines,
            custom_vertex_pipelines: geometry_custom_vertex_pipelines,
            masked_custom_vertex_pipelines: geometry_masked_custom_vertex_pipelines,
        };

        let shadow_masked = shadow_masked::ShadowMaskedRenderPass {
            bind_group: shadow_masked_bg,
            pipelines: shadow_masked_pipelines,
        };

        let shadow_custom_vertex = shadow_custom_vertex::ShadowCustomVertexRenderPass {
            pipelines: shadow_custom_vertex_pipelines,
        };

        let shadow_masked_custom_vertex =
            shadow_masked_custom_vertex::ShadowMaskedCustomVertexRenderPass {
                pipelines: shadow_masked_custom_vertex_pipelines,
            };

        let coverage = match (
            coverage_bg_single,
            ranges.coverage_single,
            coverage_bg_msaa,
            ranges.coverage_msaa,
        ) {
            (Some(bg_s), Some(r_s), Some(bg_m), Some(r_m)) => Some(CoverageRenderPass {
                bind_groups_singlesampled: bg_s,
                bind_groups_multisampled: bg_m,
                pipelines_singlesampled: CoveragePipelines::from_resolved(
                    compute_keys[r_s].to_vec(),
                ),
                pipelines_multisampled: CoveragePipelines::from_resolved(
                    compute_keys[r_m].to_vec(),
                ),
            }),
            _ => None,
        };

        let hzb = match (hzb_bg, ranges.hzb, hzb_texture) {
            (Some(bg), Some(range), Some(texture)) => Some(HzbRenderPass {
                bind_groups: bg,
                pipelines: HzbPipelines::from_resolved(
                    per_pass_descs.hzb_slot,
                    compute_keys[range].to_vec(),
                ),
                texture,
            }),
            _ => None,
        };

        let occlusion = match (occlusion_bg, ranges.occlusion) {
            (Some(bg), Some(range)) => Some(OcclusionRenderPass {
                bind_groups: bg,
                pipelines: OcclusionPipelines::from_resolved(compute_keys[range].to_vec()),
            }),
            _ => None,
        };

        let occlusion_compaction = match (compaction_bg, ranges.compaction) {
            (Some(bg), Some(range)) => Some(CompactionRenderPass {
                bind_groups: bg,
                pipeline: CompactionPipeline::from_resolved(compute_keys[range].to_vec()),
            }),
            _ => None,
        };

        // The boot batch already compiled the classify pipeline into the shared
        // compute pool (warming it); `ensure_scene_pipelines` (via `prewarm`)
        // installs it into `pipeline_cache` before the first frame. So we don't
        // store the resolved key here — the cache is the single source of truth.
        let _ = (&per_pass_descs.classify_slot_msaa, &ranges.classify);
        let material_classify = MaterialClassifyRenderPass {
            bind_groups: classify_bg,
            pipeline_cache: std::cell::RefCell::new(std::collections::HashMap::new()),
        };

        let light_culling = LightCullingRenderPass {
            bind_groups: light_culling_bg,
            pipelines: LightCullingPipelines::from_resolved(
                &per_pass_descs.light_culling,
                compute_keys[ranges.light_culling].to_vec(),
            )?,
        };

        let material_prep = {
            let mut pipelines = MaterialPrepPipelines::default();
            pipelines.merge_resolved(
                &per_pass_descs.material_prep.slots,
                compute_keys[ranges.material_prep].to_vec(),
            );
            // Kept as an always-`Some` `Option` so the `if let Some(prep)`
            // dispatch sites stay valid (prep is unconditional).
            Some(MaterialPrepRenderPass::from_resolved(
                material_prep_bg,
                pipelines,
                per_pass_descs.prep_edge_shadow,
            ))
        };

        let material_decal = match (
            decal_bg,
            ranges.decal,
            decal_classify_bg,
            ranges.decal_classify,
            per_pass_descs.decal_is_msaa,
        ) {
            (
                Some(decal_bg),
                Some(decal_range),
                Some(decal_classify_bg),
                Some(decal_classify_range),
                Some(decal_is_msaa),
            ) => Some(MaterialDecalRenderPass {
                bind_groups: decal_bg,
                pipelines: MaterialDecalPipelines::from_resolved(
                    decal_is_msaa,
                    compute_keys[decal_range].to_vec(),
                ),
                // Deferred-boot: compiled by `ensure_decal_composite_compiled`.
                composite: material_decal_composite,
                classify_pass: material_decal::classify::render_pass::DecalClassifyRenderPass {
                    bind_groups: decal_classify_bg,
                    pipelines: DecalClassifyPipelines::from_resolved(
                        compute_keys[decal_classify_range].to_vec(),
                    ),
                },
            }),
            _ => None,
        };

        let material_opaque = MaterialOpaqueRenderPass {
            bind_groups: opaque_bg,
            pipelines: MaterialOpaquePipelines::from_resolved(
                per_pass_descs.opaque_slots,
                compute_keys[ranges.opaque].to_vec(),
            ),
            // Edge-resolve pipelines are scheduler-managed — empty
            // at cold-boot, populate lazily as edge_resolve compile
            // futures resolve.
            edge_pipelines:
                crate::render_passes::material_opaque::edge_pipeline::MaterialEdgePipelines::new(),
            edge_bind_group_layouts: opaque_edge_bind_group_layouts,
        };

        let material_transparent = MaterialTransparentRenderPass {
            bind_groups: transparent_bg,
            pipelines: transparent_pipelines,
        };

        let effects = EffectsRenderPass {
            bind_groups: effects_bg,
            pipelines: effects_pipelines,
        };

        let display = DisplayRenderPass {
            bind_groups: display_bg,
            pipelines: display_pipelines,
            last_exposure_scale: std::cell::Cell::new(None),
        };

        Ok(Self {
            geometry,
            shadow_masked,
            shadow_custom_vertex,
            shadow_masked_custom_vertex,
            coverage,
            hzb,
            occlusion,
            occlusion_compaction,
            #[cfg(feature = "lod")]
            cluster_lod,
            light_culling,
            material_classify,
            material_prep,
            material_decal,
            material_opaque,
            material_transparent,
            effects,
            bloom,
            ssr,
            ssr_minz,
            display,
        })
    }
}

/// Shared context used to initialize render passes.
///
/// `gpu` is `&` (not `&mut`) on purpose — no init path mutates the
/// `AwsmRendererWebGpu` handle; everything goes through the shared
/// `device` / `queue` JS handles which are `Clone`-cheap on
/// `wasm-bindgen` types. Keeping it shared lets `RenderPasses::new`
/// and `RenderTextures::new` run inside the same `futures::try_join`
/// in `lib.rs` — both want `&gpu`, neither contends on the other's
/// `&mut` fields.
pub struct RenderPassInitContext<'a> {
    pub gpu: &'a AwsmRendererWebGpu,
    pub bind_group_layouts: &'a mut BindGroupLayouts,
    pub textures: &'a mut Textures,
    pub pipeline_layouts: &'a mut PipelineLayouts,
    pub pipelines: &'a mut Pipelines,
    pub shaders: &'a mut Shaders,
    pub render_texture_formats: &'a mut RenderTextureFormats,
    /// Active feature gates. Lets construction-time code (e.g. the
    /// decal classify pass's HZB binding switch) pick the variant
    /// that matches the live feature set.
    pub features: &'a RendererFeatures,
    /// Active MSAA + mipmap state. Lazy-pool passes use this to
    /// compile only the variant matching the live config; the
    /// other (msaa, mipmap) combinations get compiled on demand
    /// when the caller invokes `AwsmRenderer::set_anti_aliasing`.
    pub anti_aliasing: &'a crate::anti_alias::AntiAliasing,
    /// Active post-processing state (bloom, tonemapping, DoF, ...).
    /// Same role as `anti_aliasing`: lazy-pool passes only compile
    /// the live-config variant up-front.
    pub post_processing: &'a crate::post_process::PostProcessing,
    /// Plan B shared-prep config (carries the `K` shadow-caster sizing knob).
    /// The prep pass is unconditional.
    pub prep_config: &'a crate::render_passes::material_prep::PrepPassConfig,
    /// Resolved edge-pixel budget (matches `MaterialEdgeBuffers::max_edge_budget`).
    /// Sizes the prep pass's compact per-edge-sample shadow texture (Stage
    /// 5b-shadow); only consulted by the MSAA prep pipeline.
    pub max_edge_budget: u32,
}
