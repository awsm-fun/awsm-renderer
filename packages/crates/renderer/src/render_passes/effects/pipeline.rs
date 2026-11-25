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
    post_process::{AtmosphereMode, PostProcessing},
    render_passes::{
        effects::{
            bind_group::EffectsBindGroups,
            shader::cache_key::{AtmospherePhase, BloomPhase, ShaderCacheKeyEffects},
        },
        RenderPassInitContext,
    },
    render_textures::RenderTextureFormats,
    shaders::{ShaderCacheKey, Shaders},
};

/// Compute pipelines for post-processing effects.
pub struct EffectsPipelines {
    multisampled_pipeline_layout_key: PipelineLayoutKey,
    singlesampled_pipeline_layout_key: PipelineLayoutKey,

    // When bloom is disabled - single pass for other effects
    no_bloom_pipeline: Option<ComputePipelineKey>,

    // When bloom is enabled - blends the dedicated BloomRenderPass's
    // pre-built pyramid over the composite
    bloom_blend_pipeline: Option<ComputePipelineKey>,

    // The dispatch ahead of the other two while DoF is on: fills the CoC
    // buffer they gather from. `None` while DoF is off.
    dof_prepare_pipeline: Option<ComputePipelineKey>,
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

/// The compiled haze variant for a post-processing config.
///
/// A near-identity map now that the config carries a three-way `mode` rather
/// than an enable plus a style flag — which is the point: the meaningless
/// "volumetric but disabled" state can no longer be constructed, so nothing
/// here has to decide what it would mean.
fn atmosphere_phase(post_processing: &PostProcessing) -> AtmospherePhase {
    match post_processing.atmosphere.mode {
        AtmosphereMode::Off => AtmospherePhase::None,
        AtmosphereMode::Fog => AtmospherePhase::Analytic,
        AtmosphereMode::Volumetric => AtmospherePhase::Volumetric,
    }
}

/// Whether the effects pass compiles depth of field: DoF is on and does not
/// run before the TAA resolve instead (`TaaDofOrder::BeforeResolve`, where
/// the TAA pass's `dof_pre` blurs `composite` and this pass keeps only haze
/// and bloom).
pub fn effects_dof(anti_aliasing: &AntiAliasing, post_processing: &PostProcessing) -> bool {
    post_processing.dof && !anti_aliasing.dof_before_resolve()
}

impl EffectsPipelines {
    /// Drops the pass's reference to its bloom-blend variant (the caller
    /// releases the slot). For a variant that failed to compile: the field
    /// otherwise survives a bloom toggle, and a later enable would find a key
    /// with no pipeline behind it instead of compiling afresh.
    pub(crate) fn forget_bloom_blend(&mut self) {
        self.bloom_blend_pipeline = None;
    }

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
            bloom_blend_pipeline: None,
            dof_prepare_pipeline: None,
        })
    }

    /// The DoF prepare dispatch of the installed config; `None` without DoF.
    pub fn dof_prepare_pipeline(&self) -> Option<ComputePipelineKey> {
        self.dof_prepare_pipeline
    }

    /// Get pipeline for a specific bloom phase
    pub fn get_bloom_pipeline(&self, phase: BloomPhase) -> Option<ComputePipelineKey> {
        match phase {
            BloomPhase::None => self.no_bloom_pipeline,
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

    /// Returns the shader cache keys for the *live* AA + PP config.
    ///
    /// **Lazy-pool reduction:** the previous build emitted all 5
    /// phases unconditionally (no_bloom + 4 bloom passes). Now we
    /// emit only the phases the live config actually dispatches —
    /// 1 when bloom is off, 5 when bloom is on. The slot order
    /// matches [`Self::install_resolved`]'s slot indexing.
    ///
    /// Toggling bloom mid-session goes through
    /// [`Self::set_render_pipeline_keys`] which calls back through
    /// this function and re-runs the batched ensure for the new set.
    pub fn shader_cache_keys_for(
        anti_aliasing: &AntiAliasing,
        post_processing: &PostProcessing,
        reverse_z: bool,
    ) -> Result<Vec<ShaderCacheKey>> {
        let slot_inputs = Self::slot_inputs_for(post_processing);
        let multisampled_geometry = anti_aliasing.has_msaa_checked()?;
        let dof = effects_dof(anti_aliasing, post_processing);
        Ok(slot_inputs
            .iter()
            .map(|&bloom_phase| {
                ShaderCacheKey::from(ShaderCacheKeyEffects {
                    smaa_anti_alias: anti_aliasing.smaa,
                    bloom_phase,
                    dof,
                    atmosphere: atmosphere_phase(post_processing),
                    multisampled_geometry,
                    reverse_z,
                    taa_post: anti_aliasing.taa.is_some(),
                })
            })
            .collect())
    }

    /// The canonical phase slots (no_bloom, blend) — when bloom is off,
    /// only the no_bloom slot is returned, so the cross-tail pool stays
    /// minimal at cold-boot. The old in-pass extract/blur slots died with
    /// the migration to the dedicated `BloomRenderPass` pyramid.
    fn slot_inputs_for(post_processing: &PostProcessing) -> Vec<BloomPhase> {
        if !post_processing.bloom {
            return vec![BloomPhase::None];
        }
        vec![BloomPhase::None, BloomPhase::Blend]
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
        reverse_z: bool,
    ) -> Result<EffectsPipelinesDescriptors> {
        let shader_cache_keys =
            Self::shader_cache_keys_for(anti_aliasing, post_processing, reverse_z)?;
        let multisampled_geometry = anti_aliasing.has_msaa_checked()?;
        let layout_key = self.layout_key_for(multisampled_geometry);

        let mut pipeline_cache_keys: Vec<ComputePipelineCacheKey> =
            Vec::with_capacity(shader_cache_keys.len());
        for cache_key in &shader_cache_keys {
            let shader_key = shaders.get_key(gpu, cache_key.clone()).await?;
            // Named, because the DoF variants hold a second entry point and
            // WebGPU then refuses a pipeline that doesn't say which.
            pipeline_cache_keys.push(
                ComputePipelineCacheKey::new(shader_key, layout_key).with_entry_point("main"),
            );
        }
        if effects_dof(anti_aliasing, post_processing) {
            // `dof_prepare` is the same in every slot's module. The no-bloom
            // one is always there, so a bloom toggle keeps this pipeline.
            let shader_key = shaders.get_key(gpu, shader_cache_keys[0].clone()).await?;
            pipeline_cache_keys.push(
                ComputePipelineCacheKey::new(shader_key, layout_key)
                    .with_entry_point("dof_prepare"),
            );
        }

        Ok(EffectsPipelinesDescriptors {
            shader_cache_keys,
            pipeline_cache_keys,
        })
    }

    /// Writes the resolved keys into the per-phase slots.
    /// Lazy-pool aware: the input length matches the live
    /// `slot_inputs_for(post_processing)` size — 1 when bloom is off,
    /// 2 when on — plus, last, the DoF prepare pipeline when DoF is on. Slot
    /// identity comes from the matching `slot_inputs` list rather than
    /// positional indexing. Pure sync.
    ///
    /// Returns the keys this DISPLACED (a slot that held a different key —
    /// the config re-keyed it) so the caller can hand them to
    /// `ComputePipelines::release_reserved`: a displaced key that was only
    /// ever reserved (the boot-time variant, re-keyed by a scene's
    /// post-processing before the first commit) must not be compiled.
    pub fn install_resolved(
        &mut self,
        anti_aliasing: &AntiAliasing,
        post_processing: &PostProcessing,
        mut resolved: Vec<ComputePipelineKey>,
    ) -> Vec<ComputePipelineKey> {
        let slot_inputs = Self::slot_inputs_for(post_processing);
        let dof = effects_dof(anti_aliasing, post_processing);
        debug_assert_eq!(resolved.len(), slot_inputs.len() + usize::from(dof));
        let mut displaced = Vec::new();
        let dof_prepare = if dof { resolved.pop() } else { None };
        if let Some(old) = self.dof_prepare_pipeline {
            if dof_prepare != Some(old) {
                displaced.push(old);
            }
        }
        self.dof_prepare_pipeline = dof_prepare;
        for (phase, key) in slot_inputs.into_iter().zip(resolved) {
            let field = match phase {
                BloomPhase::None => &mut self.no_bloom_pipeline,
                BloomPhase::Blend => &mut self.bloom_blend_pipeline,
            };
            if let Some(old) = field.replace(key) {
                if old != key {
                    displaced.push(old);
                }
            }
        }
        displaced
    }

    /// Updates pipelines for the current anti-aliasing and
    /// post-processing settings. Used by the dynamic setters
    /// ([`crate::AwsmRenderer::set_anti_aliasing`] /
    /// [`crate::AwsmRenderer::set_post_processing`]) — at startup the
    /// orchestrator goes through [`Self::build_descriptors`] +
    /// [`Self::install_resolved`] directly via the cross-tail pool.
    /// Builds the live bloom-phase variants concurrently via two
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
        reverse_z: bool,
    ) -> Result<()> {
        let shader_cache_keys =
            Self::shader_cache_keys_for(anti_aliasing, post_processing, reverse_z)?;
        // Batch 1: 5 shader compiles in parallel.
        shaders
            .ensure_keys(gpu, shader_cache_keys.iter().cloned())
            .await?;
        // Resolve descriptors (sync cache hits) + batch the pipelines.
        let descs = self
            .build_descriptors(anti_aliasing, post_processing, gpu, shaders, reverse_z)
            .await?;
        let resolved = pipelines
            .compute
            .ensure_keys(gpu, shaders, pipeline_layouts, descs.pipeline_cache_keys)
            .await?;
        let displaced = self.install_resolved(anti_aliasing, post_processing, resolved);
        pipelines.compute.release_reserved(displaced);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::post_process::Atmosphere;

    fn pp(mode: AtmosphereMode) -> PostProcessing {
        PostProcessing {
            atmosphere: Atmosphere {
                mode,
                ..Atmosphere::default()
            },
            ..PostProcessing::default()
        }
    }

    /// Haze off must resolve to the `None` phase. That's the whole basis of
    /// "zero cost when off": the shader templates gate every haze include and
    /// every haze uniform on this phase, so `None` here is what makes the
    /// compiled effects shader identical to a build with no haze feature.
    #[test]
    fn off_compiles_no_haze_phase() {
        assert_eq!(
            atmosphere_phase(&pp(AtmosphereMode::Off)),
            AtmospherePhase::None
        );
        assert_eq!(
            atmosphere_phase(&pp(AtmosphereMode::Fog)),
            AtmospherePhase::Analytic
        );
    }

    /// Volumetric mode must compile the volumetric phase, NOT the analytic
    /// one. The two would look broadly similar in a screenshot — same medium,
    /// same colour — so a regression to the fog term is the kind that hides:
    /// you'd lose the beams and keep plausible haze.
    #[test]
    fn volumetric_compiles_the_volumetric_phase() {
        assert_eq!(
            atmosphere_phase(&pp(AtmosphereMode::Volumetric)),
            AtmospherePhase::Volumetric
        );
    }
}
