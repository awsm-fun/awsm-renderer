//! HZB compute pipelines.
//!
//! **Lazy-pool semantics:** the seed pipeline is MSAA-specialized;
//! the reduce pipeline is shared. Cold-boot only compiles the seed
//! variant matching the live `AntiAliasing` config — toggling MSAA
//! mid-session goes through [`crate::AwsmRenderer::set_anti_aliasing`]
//! which compiles the other seed variant on demand and merges it in.

use crate::anti_alias::AntiAliasing;
use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::hzb::{
    bind_group::HzbBindGroups,
    shader::cache_key::{ShaderCacheKeyHzbReduce, ShaderCacheKeyHzbSeed},
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct HzbPipelines {
    pub seed_msaa: Option<ComputePipelineKey>,
    pub seed_single: Option<ComputePipelineKey>,
    pub reduce: ComputePipelineKey,
}

/// Descriptors for the HZB compute pipelines. `slot_msaa` records
/// which `msaa_sample_count` each entry in `pipeline_cache_keys`
/// belongs to (or `None` for the reduce pass, which is MSAA-
/// agnostic). Lazy-pool: contains 2 entries (1 seed + 1 reduce)
/// for the live config; recompiles add the second seed lazily.
pub struct HzbPrewarmDescriptors {
    pub pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
    pub slot: Vec<HzbPipelineSlot>,
}

/// Slot identity carried alongside each compiled pipeline so
/// `merge_resolved` knows which `Option` field to update.
#[derive(Clone, Copy, Debug)]
pub enum HzbPipelineSlot {
    SeedMsaa,
    SeedSingle,
    Reduce,
}

impl HzbPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &HzbBindGroups,
    ) -> Result<Self> {
        ctx.shaders
            .ensure_keys(
                ctx.gpu,
                Self::shader_cache_keys(ctx.anti_aliasing, ctx.features.reverse_z),
            )
            .await?;
        let descs = Self::build_descriptors(ctx, bind_groups).await?;
        let pipeline_keys = ctx
            .pipelines
            .compute
            .ensure_keys(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                descs.pipeline_cache_keys.clone(),
            )
            .await?;
        Ok(Self::from_resolved(descs.slot, pipeline_keys))
    }

    /// Shader cache keys for the live AA config — emits the matching
    /// seed variant + the always-needed reduce shader. Previously
    /// emitted both seed variants unconditionally.
    pub fn shader_cache_keys(anti_aliasing: &AntiAliasing, reverse_z: bool) -> Vec<ShaderCacheKey> {
        let seed_msaa = match anti_aliasing.msaa_sample_count {
            Some(4) => Some(4),
            _ => None,
        };
        vec![
            ShaderCacheKey::from(ShaderCacheKeyHzbSeed {
                msaa_sample_count: seed_msaa,
                reverse_z,
            }),
            ShaderCacheKey::from(ShaderCacheKeyHzbReduce { reverse_z }),
        ]
    }

    pub async fn build_descriptors(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &HzbBindGroups,
    ) -> Result<HzbPrewarmDescriptors> {
        Self::build_descriptors_for_config(
            ctx.gpu,
            ctx.bind_group_layouts,
            ctx.pipeline_layouts,
            ctx.shaders,
            bind_groups,
            ctx.anti_aliasing,
            ctx.features.reverse_z,
        )
        .await
    }

    /// Live-config descriptor builder used by both initial build
    /// and the mid-session [`crate::AwsmRenderer::set_anti_aliasing`]
    /// recompile path. Always emits the reduce slot (it's
    /// MSAA-agnostic and cheap); emits one seed variant matching
    /// the requested AA state.
    pub async fn build_descriptors_for_config(
        gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
        bind_group_layouts: &mut crate::bind_group_layout::BindGroupLayouts,
        pipeline_layouts: &mut crate::pipeline_layouts::PipelineLayouts,
        shaders: &mut crate::shaders::Shaders,
        bind_groups: &HzbBindGroups,
        anti_aliasing: &AntiAliasing,
        reverse_z: bool,
    ) -> Result<HzbPrewarmDescriptors> {
        let (seed_msaa, seed_layout_key, seed_slot) = match anti_aliasing.msaa_sample_count {
            Some(4) => (
                Some(4_u32),
                bind_groups.seed_layout_key_msaa,
                HzbPipelineSlot::SeedMsaa,
            ),
            _ => (
                None,
                bind_groups.seed_layout_key_single,
                HzbPipelineSlot::SeedSingle,
            ),
        };

        let seed_pipeline_layout = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![seed_layout_key]),
        )?;
        let reduce_pipeline_layout = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.reduce_layout_key]),
        )?;

        let seed_shader = shaders
            .get_key(
                gpu,
                ShaderCacheKeyHzbSeed {
                    msaa_sample_count: seed_msaa,
                    reverse_z,
                },
            )
            .await?;
        let reduce_shader = shaders
            .get_key(gpu, ShaderCacheKeyHzbReduce { reverse_z })
            .await?;

        Ok(HzbPrewarmDescriptors {
            pipeline_cache_keys: vec![
                ComputePipelineCacheKey::new(seed_shader, seed_pipeline_layout),
                ComputePipelineCacheKey::new(reduce_shader, reduce_pipeline_layout),
            ],
            slot: vec![seed_slot, HzbPipelineSlot::Reduce],
        })
    }

    pub fn from_resolved(
        slot: Vec<HzbPipelineSlot>,
        pipeline_keys: Vec<ComputePipelineKey>,
    ) -> Self {
        let mut seed_msaa = None;
        let mut seed_single = None;
        // Reduce is always present in any well-formed descriptor.
        // `expect` here is safe because `build_descriptors_for_config`
        // always emits the reduce slot, and the orchestrator never
        // partial-builds HZB.
        let mut reduce: Option<ComputePipelineKey> = None;
        for (s, key) in slot.into_iter().zip(pipeline_keys) {
            match s {
                HzbPipelineSlot::SeedMsaa => seed_msaa = Some(key),
                HzbPipelineSlot::SeedSingle => seed_single = Some(key),
                HzbPipelineSlot::Reduce => reduce = Some(key),
            }
        }
        Self {
            seed_msaa,
            seed_single,
            reduce: reduce.expect("HZB reduce pipeline must be in initial build"),
        }
    }

    /// Merge a fresh batch of resolved pipelines into `self` without
    /// dropping already-compiled variants. Used by
    /// [`crate::AwsmRenderer::set_anti_aliasing`] so the previously-
    /// compiled seed variant survives the recompile cycle.
    pub fn merge_resolved(
        &mut self,
        slot: Vec<HzbPipelineSlot>,
        pipeline_keys: Vec<ComputePipelineKey>,
    ) {
        for (s, key) in slot.into_iter().zip(pipeline_keys) {
            match s {
                HzbPipelineSlot::SeedMsaa => self.seed_msaa = Some(key),
                HzbPipelineSlot::SeedSingle => self.seed_single = Some(key),
                HzbPipelineSlot::Reduce => self.reduce = key,
            }
        }
    }
}
