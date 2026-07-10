//! Compute pipeline for the light culling pass.
//!
//! Single shader module, two pipelines (`cs_main` Z-refine + `cs_tile`
//! side-plane cull) selected by entry point. The cull is MSAA-agnostic
//! (it doesn't sample the visibility or depth textures). The cache key
//! carries only `slice_count`: the per-froxel index-buffer capacity is a
//! *runtime* `cull_params` field, so auto-grow resizes buffers and
//! rewrites the uniform without recompiling.

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::light_culling::{
    bind_group::LightCullingBindGroups, buffers::DEFAULT_SLICE_COUNT,
    shader::cache_key::ShaderCacheKeyLightCulling,
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

/// Phase-2 output of [`LightCullingPipelines::build_descriptors`]: the two
/// pooled compute cache keys, in the fixed order `[cs_main, cs_tile]`.
pub struct LightCullingPrewarmDescriptors {
    pub pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
    pub slice_count: u32,
}

/// Compiled compute pipelines for the cull shader plus the `slice_count`
/// they were compiled against. Per-froxel capacity is deliberately not
/// tracked here — it's a runtime `cull_params` field that never triggers
/// a recompile (see the module doc and `set_max_per_froxel_capacity`).
pub struct LightCullingPipelines {
    /// Stage B (`cs_main`) — per-froxel Z-refine pipeline.
    pub pipeline_key: ComputePipelineKey,
    /// Stage A (`cs_tile`) — per-2D-tile side-plane cull pipeline.
    /// Compiled from the same shader module as `pipeline_key`, selected
    /// via the `cs_tile` entry point.
    pub tile_pipeline_key: ComputePipelineKey,
    pub slice_count: u32,
}

impl LightCullingPipelines {
    /// The cull shader's cache key (one module for both entry points) —
    /// pooled into the cross-renderer `Shaders::ensure_keys` batch.
    pub fn shader_cache_keys(reverse_z: bool) -> Vec<ShaderCacheKey> {
        vec![ShaderCacheKeyLightCulling {
            slice_count: DEFAULT_SLICE_COUNT,
            reverse_z,
        }
        .into()]
    }

    /// Build the two pooled pipeline cache keys (`cs_main` + `cs_tile`) at
    /// the default `DEFAULT_SLICE_COUNT`. `LightCullingBuffers::new` is
    /// allocated at the matching slice count so the consumer / cull shaders
    /// agree on the WGSL constants. Sync apart from the cache-hit
    /// `shaders.get_key` await (the pooled shader batch ran first).
    pub async fn build_descriptors(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &LightCullingBindGroups,
    ) -> Result<LightCullingPrewarmDescriptors> {
        let slice_count = DEFAULT_SLICE_COUNT;
        let pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.bind_group_layout_key]),
        )?;
        let shader_key = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeyLightCulling {
                    slice_count,
                    reverse_z: ctx.features.reverse_z,
                },
            )
            .await?;
        Ok(LightCullingPrewarmDescriptors {
            pipeline_cache_keys: vec![
                ComputePipelineCacheKey::new(shader_key, pipeline_layout_key)
                    .with_entry_point("cs_main"),
                ComputePipelineCacheKey::new(shader_key, pipeline_layout_key)
                    .with_entry_point("cs_tile"),
            ],
            slice_count,
        })
    }

    /// Fold the resolved pool slice (order `[cs_main, cs_tile]`) back into
    /// the typed pipelines. Sync; no Dawn / GPU calls.
    pub fn from_resolved(
        descs: &LightCullingPrewarmDescriptors,
        keys: Vec<ComputePipelineKey>,
    ) -> Result<Self> {
        let [pipeline_key, tile_pipeline_key] = keys[..] else {
            return Err(crate::error::AwsmError::PipelineVariantNotCompiled(
                "light culling expected exactly 2 resolved pipelines (cs_main + cs_tile)",
            ));
        };
        Ok(Self {
            pipeline_key,
            tile_pipeline_key,
            slice_count: descs.slice_count,
        })
    }
}
