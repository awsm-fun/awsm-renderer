//! Render pass orchestration and initialization.

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
pub mod material_transparent;
pub mod occlusion;
pub mod shader_cache_key;
pub mod shader_template;
pub mod shared;

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
        coverage::render_pass::CoverageRenderPass, display::render_pass::DisplayRenderPass,
        geometry::render_pass::GeometryRenderPass, hzb::render_pass::HzbRenderPass,
        light_culling::render_pass::LightCullingRenderPass,
        material_classify::render_pass::MaterialClassifyRenderPass,
        material_decal::render_pass::MaterialDecalRenderPass,
        material_opaque::render_pass::MaterialOpaqueRenderPass,
        material_transparent::render_pass::MaterialTransparentRenderPass,
        occlusion::compaction::CompactionRenderPass, occlusion::render_pass::OcclusionRenderPass,
    },
    render_textures::RenderTextureFormats,
    shaders::Shaders,
    textures::Textures,
};

/// Collection of render passes used by the renderer.
pub struct RenderPasses {
    pub geometry: GeometryRenderPass,
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
    pub light_culling: LightCullingRenderPass,
    pub material_classify: MaterialClassifyRenderPass,
    /// Decal classify + shading + composite pass. `None` when
    /// `features.decals == false`.
    pub material_decal: Option<MaterialDecalRenderPass>,
    pub material_opaque: MaterialOpaqueRenderPass,
    pub material_transparent: MaterialTransparentRenderPass,
    pub effects: EffectsRenderPass,
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
    coverage_bg_single: Option<coverage::bind_group::CoverageBindGroups>,
    coverage_bg_msaa: Option<coverage::bind_group::CoverageBindGroups>,
    hzb_bg: Option<hzb::bind_group::HzbBindGroups>,
    occlusion_bg: Option<occlusion::bind_group::OcclusionBindGroups>,
    compaction_bg: Option<occlusion::compaction::CompactionBindGroups>,
    light_culling: LightCullingRenderPass,
    classify_bg: material_classify::bind_group::MaterialClassifyBindGroups,
    decal_bg: Option<material_decal::bind_group::MaterialDecalBindGroups>,
    decal_classify_bg: Option<material_decal::classify::bind_group::DecalClassifyBindGroups>,
    opaque_bg: material_opaque::bind_group::MaterialOpaqueBindGroups,
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
    opaque_slots: Vec<crate::render_passes::material_opaque::pipeline::OpaquePipelineSlot>,
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
        let light_culling = LightCullingRenderPass::new(ctx).await?;
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
        shader_cache_keys.extend(GeometryPipelines::shader_cache_keys());
        if features.gpu_culling {
            shader_cache_keys.extend(HzbPipelines::shader_cache_keys());
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
            &first_party_entries,
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
                coverage_bg_single,
                coverage_bg_msaa,
                hzb_bg,
                occlusion_bg,
                compaction_bg,
                light_culling,
                classify_bg,
                decal_bg,
                decal_classify_bg,
                opaque_bg,
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

        let geometry_descs =
            GeometryPipelines::build_descriptors(ctx, &bindings.geometry_bg).await?;
        let geometry_range =
            render_pool.len()..render_pool.len() + geometry_descs.pipeline_cache_keys.len();
        render_pool.extend(geometry_descs.pipeline_cache_keys.iter().cloned());

        let hzb_range = if let Some(bg) = bindings.hzb_bg.as_ref() {
            let descs = HzbPipelines::build_descriptors(ctx, bg).await?;
            let start = compute_pool.len();
            let end = start + descs.pipeline_cache_keys.len();
            compute_pool.extend(descs.pipeline_cache_keys.iter().cloned());
            Some(start..end)
        } else {
            None
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

        // Decal composite — only the typed handle, no cache keys to
        // pool. The composite uses inline WGSL and internal
        // `try_join` for its 2 pipelines; building it here keeps the
        // construction ordering identical to the pre-refactor flow.
        let material_decal_composite = if bindings.decal_bg.is_some() {
            Some(material_decal::composite::MaterialDecalComposite::new(ctx).await?)
        } else {
            None
        };

        // HZB texture — tiny initial allocation, recreated against
        // the live viewport on the first frame. Allocated here
        // because `from_resolved` is sync and doesn't have a gpu
        // handle; this is sub-millisecond GPU work.
        let hzb_texture = if bindings.hzb_bg.is_some() {
            Some(hzb::texture::HzbTexture::new(ctx.gpu, 1, 1)?)
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
                decal: decal_range,
                decal_classify: decal_classify_range,
                opaque: opaque_range,
            },
            per_pass_descs: RenderPassesPerPassDescs {
                geometry: geometry_descs,
                opaque_slots: opaque_descs.slots,
                decal_is_msaa,
            },
            material_decal_composite,
            hzb_texture,
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
        use crate::render_passes::material_classify::pipeline::MaterialClassifyPipelines;
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
            ..
        } = descs;
        let RenderPassesBindings {
            geometry_bg,
            coverage_bg_single,
            coverage_bg_msaa,
            hzb_bg,
            occlusion_bg,
            compaction_bg,
            light_culling,
            classify_bg,
            decal_bg,
            decal_classify_bg,
            opaque_bg,
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
                pipelines: HzbPipelines::from_resolved(compute_keys[range].to_vec()),
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

        let material_classify = MaterialClassifyRenderPass {
            bind_groups: classify_bg,
            pipelines: MaterialClassifyPipelines::from_resolved(
                compute_keys[ranges.classify].to_vec(),
            ),
        };

        let material_decal = match (
            decal_bg,
            ranges.decal,
            decal_classify_bg,
            ranges.decal_classify,
            per_pass_descs.decal_is_msaa,
            material_decal_composite,
        ) {
            (
                Some(decal_bg),
                Some(decal_range),
                Some(decal_classify_bg),
                Some(decal_classify_range),
                Some(decal_is_msaa),
                Some(composite),
            ) => Some(MaterialDecalRenderPass {
                bind_groups: decal_bg,
                pipelines: MaterialDecalPipelines::from_resolved(
                    decal_is_msaa,
                    compute_keys[decal_range].to_vec(),
                ),
                composite,
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
        };

        Ok(Self {
            geometry,
            coverage,
            hzb,
            occlusion,
            occlusion_compaction,
            light_culling,
            material_classify,
            material_decal,
            material_opaque,
            material_transparent,
            effects,
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
}
