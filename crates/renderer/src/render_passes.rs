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
    pub async fn new<'a>(
        ctx: &mut RenderPassInitContext<'a>,
        features: &RendererFeatures,
    ) -> Result<Self> {
        // Cross-pass shader prewarm. Every render pass below has its
        // own per-pass `shaders.ensure_keys` call inside `new()`, but
        // those awaits serialise across passes (each pass holds
        // `&mut ctx` for the full duration of its `new`). On a cold
        // PSO disk cache that turned the renderer-init window into
        // a ~16 s wall-clock cliff on the user's machine.
        //
        // Hoist the shader-key enumeration up to this top-level
        // function so all variants — across geometry, opaque,
        // classify, hzb, occlusion, decal, coverage, shadows, picker,
        // line — go through a single `Shaders::ensure_keys` call
        // that fires every `compile_shader` synchronously before
        // awaiting any `validate_shader`. Per-pass `ensure_keys`
        // calls inside `new()` then see every key already cached and
        // become no-ops; only the pipeline-compile awaits remain
        // per-pass-serial. Pure additive: passes that don't appear
        // in the prewarm list (effects, display — both depend on
        // anti-aliasing / post-processing config that hasn't been
        // resolved yet) still compile their own shaders on demand.
        prewarm_render_pass_shaders(ctx, features).await?;

        Ok(Self {
            geometry: GeometryRenderPass::new(ctx).await?,
            coverage: if features.coverage_lod {
                Some(CoverageRenderPass::new(ctx).await?)
            } else {
                None
            },
            hzb: if features.gpu_culling {
                Some(HzbRenderPass::new(ctx).await?)
            } else {
                None
            },
            occlusion: if features.gpu_culling {
                Some(OcclusionRenderPass::new(ctx).await?)
            } else {
                None
            },
            occlusion_compaction: if features.gpu_culling {
                Some(CompactionRenderPass::new(ctx).await?)
            } else {
                None
            },
            light_culling: LightCullingRenderPass::new(ctx).await?,
            material_classify: MaterialClassifyRenderPass::new(ctx).await?,
            material_decal: if features.decals {
                Some(MaterialDecalRenderPass::new(ctx).await?)
            } else {
                None
            },
            material_opaque: MaterialOpaqueRenderPass::new(ctx).await?,
            material_transparent: MaterialTransparentRenderPass::new(ctx).await?,
            effects: EffectsRenderPass::new(ctx).await?,
            display: DisplayRenderPass::new(ctx).await?,
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

/// Cross-pass shader pre-warm. Enumerates every shader cache key
/// the about-to-be-constructed render passes will compile and runs
/// one batched `Shaders::ensure_keys`. The per-pass constructors
/// then see cache hits for every shader they need, so the only
/// remaining per-pass-serial work is pipeline compile.
///
/// Variants whose shader key depends on bind-group-computed state
/// (effects: anti-aliasing + post-processing flags; display:
/// tone-mapping config) compile inside their per-pass `new()`
/// as before — that's fine because they're singletons / small
/// counts.
async fn prewarm_render_pass_shaders<'a>(
    ctx: &mut RenderPassInitContext<'a>,
    features: &RendererFeatures,
) -> Result<()> {
    use crate::shaders::ShaderCacheKey;

    let mut keys: Vec<ShaderCacheKey> = Vec::new();

    // Geometry: 8 unique variants — (instancing × meta_storage_array ×
    // msaa) collapsed across cull mode (which has no shader effect).
    {
        use crate::render_passes::geometry::shader::cache_key::ShaderCacheKeyGeometry;
        for msaa_samples in [None, Some(4u32)] {
            for (instancing, meta_storage_array) in [(false, true), (false, false), (true, false)] {
                keys.push(
                    ShaderCacheKeyGeometry {
                        instancing_transforms: instancing,
                        meta_storage_array,
                        msaa_samples,
                    }
                    .into(),
                );
            }
        }
    }

    // HZB: 3 variants.
    if features.gpu_culling {
        use crate::render_passes::hzb::shader::cache_key::{
            ShaderCacheKeyHzbReduce, ShaderCacheKeyHzbSeed,
        };
        keys.push(
            ShaderCacheKeyHzbSeed {
                msaa_sample_count: Some(4),
            }
            .into(),
        );
        keys.push(
            ShaderCacheKeyHzbSeed {
                msaa_sample_count: None,
            }
            .into(),
        );
        keys.push(ShaderCacheKeyHzbReduce.into());
    }

    // Material classify: 2 variants.
    {
        use crate::render_passes::material_classify::shader::cache_key::ShaderCacheKeyMaterialClassify;
        keys.push(
            ShaderCacheKeyMaterialClassify {
                msaa_sample_count: Some(4),
            }
            .into(),
        );
        keys.push(
            ShaderCacheKeyMaterialClassify {
                msaa_sample_count: None,
            }
            .into(),
        );
    }

    // Occlusion + compaction (gated by gpu_culling).
    if features.gpu_culling {
        use crate::render_passes::occlusion::shader::cache_key::{
            ShaderCacheKeyOcclusionCompaction, ShaderCacheKeyOcclusionCull,
        };
        keys.push(ShaderCacheKeyOcclusionCull.into());
        keys.push(
            ShaderCacheKeyOcclusionCompaction {
                write_first_instance: true,
            }
            .into(),
        );
        keys.push(
            ShaderCacheKeyOcclusionCompaction {
                write_first_instance: false,
            }
            .into(),
        );
    }

    // Coverage (gated by coverage_lod): both multisampled variants.
    if features.coverage_lod {
        use crate::render_passes::coverage::shader::cache_key::ShaderCacheKeyCoverage;
        keys.push(ShaderCacheKeyCoverage { multisampled: true }.into());
        keys.push(
            ShaderCacheKeyCoverage {
                multisampled: false,
            }
            .into(),
        );
    }

    // Material opaque: 12 main + 2 empty = 14 variants. Texture pool
    // sizes are 0/0 at startup (no models loaded); finalize_gpu_textures
    // will recompile when the pool grows, but the texture-pool=0
    // variants are what the renderer draws between init and the first
    // model load.
    {
        use crate::render_passes::material_opaque::shader::cache_key::{
            ShaderCacheKeyMaterialOpaque, ShaderCacheKeyMaterialOpaqueEmpty,
        };
        use awsm_materials::MaterialShaderId;
        const OPAQUE_SHADER_IDS: &[MaterialShaderId] = &[
            MaterialShaderId::Pbr,
            MaterialShaderId::Unlit,
            MaterialShaderId::Toon,
            MaterialShaderId::FlipBook,
        ];
        for &shader_id in OPAQUE_SHADER_IDS {
            for msaa in [Some(4u32), None] {
                for mipmaps in [true, false] {
                    keys.push(
                        ShaderCacheKeyMaterialOpaque {
                            texture_pool_arrays_len: 0,
                            texture_pool_samplers_len: 0,
                            msaa_sample_count: msaa,
                            mipmaps,
                            shader_id,
                        }
                        .into(),
                    );
                }
            }
        }
        for msaa in [Some(4u32), None] {
            keys.push(
                ShaderCacheKeyMaterialOpaqueEmpty {
                    texture_pool_arrays_len: 0,
                    texture_pool_samplers_len: 0,
                    msaa_sample_count: msaa,
                }
                .into(),
            );
        }
    }

    // Material decal (gated by features.decals): 2 variants at
    // texture_pool=(0,0).
    if features.decals {
        use crate::render_passes::material_decal::shader::cache_key::ShaderCacheKeyMaterialDecal;
        for msaa in [None, Some(4)] {
            keys.push(
                ShaderCacheKeyMaterialDecal {
                    msaa_sample_count: msaa,
                    texture_pool_arrays_len: 0,
                    texture_pool_samplers_len: 0,
                }
                .into(),
            );
        }
    }

    // Shadow caster shaders (called from Shadows::new later in the
    // builder; pre-warming them here means they compile in parallel
    // with the render-pass shaders rather than serialising after).
    {
        use crate::shadows::shader::cache_key::ShaderCacheKeyShadow;
        keys.push(
            ShaderCacheKeyShadow {
                instancing_transforms: false,
            }
            .into(),
        );
        keys.push(
            ShaderCacheKeyShadow {
                instancing_transforms: true,
            }
            .into(),
        );
    }

    // Picker + Line (also built later in the builder; same
    // motivation as shadows). Picker has both MSAA / non-MSAA
    // geometry variants; Line is parameter-free.
    {
        use crate::picker::ShaderCacheKeyPicker;
        use crate::render_passes::lines::shader::cache_key::ShaderCacheKeyLine;
        keys.push(
            ShaderCacheKeyPicker {
                multisampled_geometry: false,
            }
            .into(),
        );
        keys.push(
            ShaderCacheKeyPicker {
                multisampled_geometry: true,
            }
            .into(),
        );
        keys.push(ShaderCacheKeyLine.into());
    }

    // Single cross-pass shader compile batch. Subsequent per-pass
    // ensure_keys / get_key calls find every key cached and don't
    // issue more Promises.
    ctx.shaders.ensure_keys(ctx.gpu, keys).await?;
    Ok(())
}
