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

use awsm_renderer_core::renderer::AwsmRendererWebGpu;

use crate::error::Result;
use crate::features::RendererFeatures;
use crate::render_passes::effects::render_pass::EffectsRenderPass;
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

impl RenderPasses {
    /// Creates all render passes for the renderer. Passes gated by
    /// [`RendererFeatures`] are skipped at construction; their slots
    /// stay `None`.
    ///
    /// The construction flow pools every compile across every pass:
    ///
    /// 1. Each pass's bind groups are built (sync — pure hash-key
    ///    registrations against `BindGroupLayouts` /
    ///    `PipelineLayouts`; no Dawn compile).
    /// 2. Single `Shaders::ensure_keys` across all passes.
    /// 3. Each pass's pipeline cache keys are built (sync via
    ///    `get_key` cache hits on warm shaders).
    /// 4. Two batched calls: one `ComputePipelines::ensure_keys`
    ///    pooling every compute pipeline across opaque + decal +
    ///    decal-classify + classify + hzb + occlusion + compaction +
    ///    coverage; one `RenderPipelines::ensure_keys` pooling every
    ///    render pipeline across geometry. Each ensure_keys fires
    ///    every Promise back-to-back before awaiting any, so Dawn's
    ///    compile pool parallelises the full workload.
    /// 5. Per-pass `from_resolved` folds resolved keys back into each
    ///    typed pipelines struct; the typed `RenderPass` structs are
    ///    constructed from `(bind_groups, pipelines, …)`.
    ///
    /// On a cold PSO disk cache that turns the previous flow's
    /// N × 2 sequential per-pass ensure_keys awaits (one for shaders,
    /// one for pipelines, per pass) into exactly three — one shader,
    /// one compute pipeline, one render pipeline. Wall-clock drops
    /// from `sum(per-pass-batch)` to `max(t_compile)` × 3 bounded by
    /// Dawn's pool size.
    ///
    /// Effects + display pipelines are NOT part of the pool — their
    /// shader cache keys depend on anti-aliasing / post-processing
    /// config that isn't resolved until after `RenderPasses::new`
    /// returns (the builder calls `set_anti_aliasing` /
    /// `set_post_processing` later). They compile on those calls.
    /// The decal composite's two render pipelines also stay outside
    /// the pool — they use an inline WGSL source that bypasses the
    /// shared shader cache and are already batched via
    /// `create_render_pipeline_promise` + `try_join` internally.
    pub async fn new<'a>(
        ctx: &mut RenderPassInitContext<'a>,
        features: &RendererFeatures,
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

        // ----------------------------------------------------------
        // Phase 2 — pool every shader cache key, single ensure_keys
        // ----------------------------------------------------------
        let mut shader_keys: Vec<crate::shaders::ShaderCacheKey> = Vec::new();
        shader_keys.extend(GeometryPipelines::shader_cache_keys());
        if features.gpu_culling {
            shader_keys.extend(HzbPipelines::shader_cache_keys());
            shader_keys.extend(OcclusionPipelines::shader_cache_keys());
            shader_keys.extend(CompactionPipeline::shader_cache_keys(features));
        }
        shader_keys.extend(MaterialClassifyPipelines::shader_cache_keys());
        if let Some(bg) = coverage_bg_single.as_ref() {
            shader_keys.extend(CoveragePipelines::shader_cache_keys(bg));
        }
        if let Some(bg) = coverage_bg_msaa.as_ref() {
            shader_keys.extend(CoveragePipelines::shader_cache_keys(bg));
        }
        if let Some(bg) = decal_bg.as_ref() {
            shader_keys.extend(MaterialDecalPipelines::build_shader_cache_keys(ctx, bg)?);
        }
        if let Some(bg) = decal_classify_bg.as_ref() {
            shader_keys.extend(DecalClassifyPipelines::shader_cache_keys(bg));
        }
        shader_keys.extend(MaterialOpaquePipelines::build_shader_cache_keys(
            ctx, &opaque_bg,
        )?);
        // Pre-warm Shadows caster shaders + Picker + Line shaders
        // too. Shadows::new / Picker::new / LineRenderer::load run
        // LATER in `AwsmRendererBuilder::build`, but adding their
        // shader keys to this single batch means Dawn compiles them
        // in parallel with everything else; by the time those calls
        // execute, their `shaders.get_key` lookups are cache hits.
        // Pipelines for those passes still compile per-pass-serial
        // because their bind_groups aren't materialised until each
        // pass's `new()` runs — only the shader-compile half can be
        // hoisted up here.
        {
            use crate::picker::ShaderCacheKeyPicker;
            use crate::render_passes::lines::shader::cache_key::ShaderCacheKeyLine;
            use crate::shadows::shader::cache_key::ShaderCacheKeyShadow;
            shader_keys.push(
                ShaderCacheKeyShadow {
                    instancing_transforms: false,
                }
                .into(),
            );
            shader_keys.push(
                ShaderCacheKeyShadow {
                    instancing_transforms: true,
                }
                .into(),
            );
            shader_keys.push(
                ShaderCacheKeyPicker {
                    multisampled_geometry: false,
                }
                .into(),
            );
            shader_keys.push(
                ShaderCacheKeyPicker {
                    multisampled_geometry: true,
                }
                .into(),
            );
            shader_keys.push(ShaderCacheKeyLine.into());
        }
        // Transparent has no per-mesh shader keys at startup (no
        // meshes yet); effects + display compile their shaders later
        // via set_anti_aliasing / set_post_processing.
        ctx.shaders.ensure_keys(ctx.gpu, shader_keys).await?;

        // ----------------------------------------------------------
        // Phase 3 — build pipeline cache keys (shaders warm), record
        // per-pass ranges into the pooled vecs.
        // ----------------------------------------------------------
        let mut compute_pool: Vec<crate::pipelines::compute_pipeline::ComputePipelineCacheKey> =
            Vec::new();
        let mut render_pool: Vec<crate::pipelines::render_pipeline::RenderPipelineCacheKey> =
            Vec::new();

        let geometry_descs = GeometryPipelines::build_descriptors(ctx, &geometry_bg).await?;
        let geometry_range =
            render_pool.len()..render_pool.len() + geometry_descs.pipeline_cache_keys.len();
        render_pool.extend(geometry_descs.pipeline_cache_keys.iter().cloned());

        let (hzb_descs, hzb_range) = if let Some(bg) = hzb_bg.as_ref() {
            let descs = HzbPipelines::build_descriptors(ctx, bg).await?;
            let start = compute_pool.len();
            let end = start + descs.pipeline_cache_keys.len();
            compute_pool.extend(descs.pipeline_cache_keys.iter().cloned());
            (Some(descs), Some(start..end))
        } else {
            (None, None)
        };
        let (occlusion_descs, occlusion_range) = if let Some(bg) = occlusion_bg.as_ref() {
            let descs = OcclusionPipelines::build_descriptors(ctx, bg).await?;
            let start = compute_pool.len();
            let end = start + descs.pipeline_cache_keys.len();
            compute_pool.extend(descs.pipeline_cache_keys.iter().cloned());
            (Some(descs), Some(start..end))
        } else {
            (None, None)
        };
        let (compaction_descs, compaction_range) = if let Some(bg) = compaction_bg.as_ref() {
            let descs = CompactionPipeline::build_descriptors(ctx, bg).await?;
            let start = compute_pool.len();
            let end = start + descs.pipeline_cache_keys.len();
            compute_pool.extend(descs.pipeline_cache_keys.iter().cloned());
            (Some(descs), Some(start..end))
        } else {
            (None, None)
        };

        let classify_descs =
            MaterialClassifyPipelines::build_descriptors(ctx, &classify_bg).await?;
        let classify_range =
            compute_pool.len()..compute_pool.len() + classify_descs.pipeline_cache_keys.len();
        compute_pool.extend(classify_descs.pipeline_cache_keys.iter().cloned());

        let (coverage_single_descs, coverage_single_range) =
            if let Some(bg) = coverage_bg_single.as_ref() {
                let descs = CoveragePipelines::build_descriptors(ctx, bg).await?;
                let start = compute_pool.len();
                let end = start + descs.pipeline_cache_keys.len();
                compute_pool.extend(descs.pipeline_cache_keys.iter().cloned());
                (Some(descs), Some(start..end))
            } else {
                (None, None)
            };
        let (coverage_msaa_descs, coverage_msaa_range) = if let Some(bg) = coverage_bg_msaa.as_ref()
        {
            let descs = CoveragePipelines::build_descriptors(ctx, bg).await?;
            let start = compute_pool.len();
            let end = start + descs.pipeline_cache_keys.len();
            compute_pool.extend(descs.pipeline_cache_keys.iter().cloned());
            (Some(descs), Some(start..end))
        } else {
            (None, None)
        };

        let (decal_descs, decal_range, decal_classify_descs, decal_classify_range) =
            if let (Some(decal_bg), Some(decal_classify_bg)) =
                (decal_bg.as_ref(), decal_classify_bg.as_ref())
            {
                let descs = MaterialDecalPipelines::build_descriptors(ctx, decal_bg).await?;
                let start = compute_pool.len();
                let end = start + descs.pipeline_cache_keys.len();
                compute_pool.extend(descs.pipeline_cache_keys.iter().cloned());
                let decal_range = start..end;

                let classify_descs =
                    DecalClassifyPipelines::build_descriptors(ctx, decal_classify_bg).await?;
                let start = compute_pool.len();
                let end = start + classify_descs.pipeline_cache_keys.len();
                compute_pool.extend(classify_descs.pipeline_cache_keys.iter().cloned());
                let decal_classify_range = start..end;

                (
                    Some(descs),
                    Some(decal_range),
                    Some(classify_descs),
                    Some(decal_classify_range),
                )
            } else {
                (None, None, None, None)
            };

        let opaque_descs = MaterialOpaquePipelines::build_descriptors(ctx, &opaque_bg).await?;
        let opaque_range =
            compute_pool.len()..compute_pool.len() + opaque_descs.pipeline_cache_keys.len();
        compute_pool.extend(opaque_descs.pipeline_cache_keys.iter().cloned());

        // Transparent at startup just creates the pipeline layout
        // key; per-mesh pipelines are populated later via
        // `set_render_pipeline_keys_batched` once meshes load. Stays
        // outside the pool.
        let transparent_pipelines =
            material_transparent::pipeline::MaterialTransparentPipelines::new(ctx, &transparent_bg)
                .await?;

        // ----------------------------------------------------------
        // Phase 4 — two batched ensure_keys
        // ----------------------------------------------------------
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

        // ----------------------------------------------------------
        // Phase 5 — sync fold-up
        // ----------------------------------------------------------
        let geometry = GeometryRenderPass {
            bind_groups: geometry_bg,
            pipelines: GeometryPipelines::from_resolved(
                &geometry_descs,
                render_keys[geometry_range].to_vec(),
            )?,
        };

        let coverage = match (
            coverage_bg_single,
            coverage_single_descs,
            coverage_single_range,
            coverage_bg_msaa,
            coverage_msaa_descs,
            coverage_msaa_range,
        ) {
            (Some(bg_s), Some(_d_s), Some(r_s), Some(bg_m), Some(_d_m), Some(r_m)) => {
                Some(CoverageRenderPass {
                    bind_groups_singlesampled: bg_s,
                    bind_groups_multisampled: bg_m,
                    pipelines_singlesampled: CoveragePipelines::from_resolved(
                        compute_keys[r_s].to_vec(),
                    ),
                    pipelines_multisampled: CoveragePipelines::from_resolved(
                        compute_keys[r_m].to_vec(),
                    ),
                })
            }
            _ => None,
        };

        let hzb = match (hzb_bg, hzb_descs, hzb_range) {
            (Some(bg), Some(_descs), Some(range)) => Some(HzbRenderPass {
                bind_groups: bg,
                pipelines: HzbPipelines::from_resolved(compute_keys[range].to_vec()),
                // Small initial size; per-frame resize hook in
                // `render.rs` recreates against the live viewport.
                texture: hzb::texture::HzbTexture::new(ctx.gpu, 1, 1)?,
            }),
            _ => None,
        };

        let occlusion = match (occlusion_bg, occlusion_descs, occlusion_range) {
            (Some(bg), Some(_descs), Some(range)) => Some(OcclusionRenderPass {
                bind_groups: bg,
                pipelines: OcclusionPipelines::from_resolved(compute_keys[range].to_vec()),
            }),
            _ => None,
        };

        let occlusion_compaction = match (compaction_bg, compaction_descs, compaction_range) {
            (Some(bg), Some(_descs), Some(range)) => Some(CompactionRenderPass {
                bind_groups: bg,
                pipeline: CompactionPipeline::from_resolved(compute_keys[range].to_vec()),
            }),
            _ => None,
        };

        let material_classify = MaterialClassifyRenderPass {
            bind_groups: classify_bg,
            pipelines: MaterialClassifyPipelines::from_resolved(
                compute_keys[classify_range].to_vec(),
            ),
        };

        let material_decal = match (
            decal_bg,
            decal_descs,
            decal_range,
            decal_classify_bg,
            decal_classify_descs,
            decal_classify_range,
        ) {
            (
                Some(decal_bg),
                Some(decal_descs),
                Some(decal_range),
                Some(decal_classify_bg),
                Some(_decal_classify_descs),
                Some(decal_classify_range),
            ) => {
                let composite = material_decal::composite::MaterialDecalComposite::new(ctx).await?;
                Some(MaterialDecalRenderPass {
                    bind_groups: decal_bg,
                    pipelines: MaterialDecalPipelines::from_resolved(
                        decal_descs.is_msaa,
                        compute_keys[decal_range].to_vec(),
                    ),
                    composite,
                    classify_pass: material_decal::classify::render_pass::DecalClassifyRenderPass {
                        bind_groups: decal_classify_bg,
                        pipelines: DecalClassifyPipelines::from_resolved(
                            compute_keys[decal_classify_range].to_vec(),
                        ),
                    },
                })
            }
            _ => None,
        };

        let material_opaque = MaterialOpaqueRenderPass {
            bind_groups: opaque_bg,
            pipelines: MaterialOpaquePipelines::from_resolved(
                opaque_descs.slots,
                compute_keys[opaque_range].to_vec(),
            ),
        };

        let material_transparent = MaterialTransparentRenderPass {
            bind_groups: transparent_bg,
            pipelines: transparent_pipelines,
        };

        // Effects + display pipelines compile later
        // (set_anti_aliasing / set_post_processing); only their bind
        // groups + pipeline layouts are built here. Build the
        // typed Pipelines structs first, then move them into the
        // typed RenderPass struct alongside the bind groups.
        let effects_pipelines = effects::pipeline::EffectsPipelines::new(ctx, &effects_bg).await?;
        let effects = EffectsRenderPass {
            bind_groups: effects_bg,
            pipelines: effects_pipelines,
        };

        let display_pipelines = display::pipeline::DisplayPipelines::new(ctx, &display_bg).await?;
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
