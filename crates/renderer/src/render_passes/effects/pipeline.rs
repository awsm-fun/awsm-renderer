//! Effects pass pipeline setup.

use awsm_renderer_core::renderer::AwsmRendererWebGpu;

use crate::{
    anti_alias::AntiAliasing,
    error::Result,
    pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey, PipelineLayouts},
    pipelines::{
        compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey},
        Pipelines,
    },
    post_process::PostProcessing,
    render_passes::{
        effects::{
            bind_group::EffectsBindGroups,
            shader::cache_key::{BloomPhase, ShaderCacheKeyEffects},
        },
        RenderPassInitContext,
    },
    render_textures::RenderTextureFormats,
    shaders::{ShaderCacheKey, Shaders},
};

/// Number of bloom blur passes (more = smoother but slower).
/// Total passes = 1 extract + BLOOM_BLUR_PASSES + 1 blend.
pub const BLOOM_BLUR_PASSES: u32 = 3;

/// Compute pipelines for post-processing effects.
pub struct EffectsPipelines {
    multisampled_pipeline_layout_key: PipelineLayoutKey,
    singlesampled_pipeline_layout_key: PipelineLayoutKey,

    // When bloom is disabled - single pass for other effects
    no_bloom_pipeline: Option<ComputePipelineKey>,

    // When bloom is enabled - multi-pass pipelines
    bloom_extract_pipeline: Option<ComputePipelineKey>, // Always ping_pong=false
    bloom_blur_pipeline_a: Option<ComputePipelineKey>,  // ping_pong=false
    bloom_blur_pipeline_b: Option<ComputePipelineKey>,  // ping_pong=true
    bloom_blend_pipeline: Option<ComputePipelineKey>, // Always ping_pong=false (to write to effects_tex)
}

/// Pre-resolved shader + compute-pipeline cache keys for the effects
/// pass. Returned by [`EffectsPipelines::build_descriptors`] and
/// consumed by [`EffectsPipelines::install_resolved`] after the
/// orchestrator pools the 5 entries into the cross-system tail batch.
pub struct EffectsPipelinesDescriptors {
    /// 5 shader cache keys to fold into the cross-tail
    /// `Shaders::ensure_keys` batch.
    pub shader_cache_keys: Vec<ShaderCacheKey>,
    /// 5 compute pipeline cache keys — resolved against the pre-warmed
    /// shader cache. To fold into the cross-tail
    /// `ComputePipelines::ensure_keys` batch.
    pub pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
}

impl EffectsPipelines {
    /// Creates pipeline layout state for the effects pass.
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &EffectsBindGroups,
    ) -> Result<Self> {
        let singlesampled_pipeline_layout_cache_key =
            PipelineLayoutCacheKey::new(vec![bind_groups.singlesampled_bind_group_layout_key]);
        let multisampled_pipeline_layout_cache_key =
            PipelineLayoutCacheKey::new(vec![bind_groups.multisampled_bind_group_layout_key]);

        let singlesampled_pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            singlesampled_pipeline_layout_cache_key,
        )?;

        let multisampled_pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            multisampled_pipeline_layout_cache_key,
        )?;

        Ok(Self {
            multisampled_pipeline_layout_key,
            singlesampled_pipeline_layout_key,
            no_bloom_pipeline: None,
            bloom_extract_pipeline: None,
            bloom_blur_pipeline_a: None,
            bloom_blur_pipeline_b: None,
            bloom_blend_pipeline: None,
        })
    }

    /// Get pipeline for a specific bloom phase and ping_pong state
    pub fn get_bloom_pipeline(
        &self,
        phase: BloomPhase,
        ping_pong: bool,
    ) -> Option<ComputePipelineKey> {
        match phase {
            BloomPhase::None => self.no_bloom_pipeline,
            BloomPhase::Extract => self.bloom_extract_pipeline,
            BloomPhase::Blur => {
                if ping_pong {
                    self.bloom_blur_pipeline_b
                } else {
                    self.bloom_blur_pipeline_a
                }
            }
            BloomPhase::Blend => self.bloom_blend_pipeline,
        }
    }

    /// Picks the pipeline layout matching the current MSAA mode.
    fn layout_key_for(&self, multisampled_geometry: bool) -> PipelineLayoutKey {
        if multisampled_geometry {
            self.multisampled_pipeline_layout_key
        } else {
            self.singlesampled_pipeline_layout_key
        }
    }

    /// Returns the 5 shader cache keys for the current AA + PP config.
    /// These are appended to the cross-tail `Shaders::ensure_keys`
    /// batch by the orchestrator, then resolved sync via cache hits
    /// inside [`Self::build_descriptors`].
    pub fn shader_cache_keys_for(
        anti_aliasing: &AntiAliasing,
        post_processing: &PostProcessing,
    ) -> Result<Vec<ShaderCacheKey>> {
        let multisampled_geometry = anti_aliasing.has_msaa_checked()?;
        let blend_ping_pong = (1 + BLOOM_BLUR_PASSES) % 2 == 1;
        let slot_inputs: [(BloomPhase, bool); 5] = [
            (BloomPhase::None, false),
            (BloomPhase::Extract, false),
            (BloomPhase::Blur, false),
            (BloomPhase::Blur, true),
            (BloomPhase::Blend, blend_ping_pong),
        ];
        Ok(slot_inputs
            .iter()
            .map(|&(bloom_phase, ping_pong)| {
                ShaderCacheKey::from(ShaderCacheKeyEffects {
                    smaa_anti_alias: anti_aliasing.smaa,
                    bloom_phase,
                    dof: post_processing.dof,
                    ping_pong,
                    multisampled_geometry,
                })
            })
            .collect())
    }

    /// Builds the 5 (shader, compute-pipeline) cache key pairs for the
    /// current AA + post-processing config. The 5 shader cache keys
    /// must already be in the `shaders` cache (cross-tail
    /// `Shaders::ensure_keys` runs ahead of this call); each
    /// `shaders.get_key` is a cache-hit lookup.
    pub async fn build_descriptors(
        &self,
        anti_aliasing: &AntiAliasing,
        post_processing: &PostProcessing,
        gpu: &AwsmRendererWebGpu,
        shaders: &mut Shaders,
    ) -> Result<EffectsPipelinesDescriptors> {
        let shader_cache_keys = Self::shader_cache_keys_for(anti_aliasing, post_processing)?;
        let multisampled_geometry = anti_aliasing.has_msaa_checked()?;
        let layout_key = self.layout_key_for(multisampled_geometry);

        let mut pipeline_cache_keys: Vec<ComputePipelineCacheKey> =
            Vec::with_capacity(shader_cache_keys.len());
        for cache_key in &shader_cache_keys {
            let shader_key = shaders.get_key(gpu, cache_key.clone()).await?;
            pipeline_cache_keys.push(ComputePipelineCacheKey::new(shader_key, layout_key));
        }

        Ok(EffectsPipelinesDescriptors {
            shader_cache_keys,
            pipeline_cache_keys,
        })
    }

    /// Writes the resolved 5 keys into the per-phase slots. Pure sync.
    pub fn install_resolved(&mut self, resolved: Vec<ComputePipelineKey>) {
        debug_assert_eq!(resolved.len(), 5);
        self.no_bloom_pipeline = Some(resolved[0]);
        self.bloom_extract_pipeline = Some(resolved[1]);
        self.bloom_blur_pipeline_a = Some(resolved[2]);
        self.bloom_blur_pipeline_b = Some(resolved[3]);
        self.bloom_blend_pipeline = Some(resolved[4]);
    }

    /// Updates pipelines for the current anti-aliasing and
    /// post-processing settings. Used by the dynamic setters
    /// ([`crate::AwsmRenderer::set_anti_aliasing`] /
    /// [`crate::AwsmRenderer::set_post_processing`]) — at startup the
    /// orchestrator goes through [`Self::build_descriptors`] +
    /// [`Self::install_resolved`] directly via the cross-tail pool.
    /// Builds all five bloom-phase variants concurrently via two
    /// batched `ensure_keys` calls (shaders then compute pipelines).
    #[allow(clippy::too_many_arguments)]
    pub async fn set_render_pipeline_keys(
        &mut self,
        anti_aliasing: &AntiAliasing,
        post_processing: &PostProcessing,
        gpu: &AwsmRendererWebGpu,
        shaders: &mut Shaders,
        pipelines: &mut Pipelines,
        pipeline_layouts: &PipelineLayouts,
        _render_texture_formats: &RenderTextureFormats,
    ) -> Result<()> {
        let shader_cache_keys = Self::shader_cache_keys_for(anti_aliasing, post_processing)?;
        // Batch 1: 5 shader compiles in parallel.
        shaders
            .ensure_keys(gpu, shader_cache_keys.iter().cloned())
            .await?;
        // Resolve descriptors (sync cache hits) + batch the pipelines.
        let descs = self
            .build_descriptors(anti_aliasing, post_processing, gpu, shaders)
            .await?;
        let resolved = pipelines
            .compute
            .ensure_keys(gpu, shaders, pipeline_layouts, descs.pipeline_cache_keys)
            .await?;
        self.install_resolved(resolved);
        Ok(())
    }
}
