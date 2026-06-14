//! Compute pipeline for the material classify pass.
//!
//! **Lazy-pool semantics:** the initial build compiles only the
//! variant matching the live `AntiAliasing` config — one of the
//! two `Option` fields below is populated, the other is `None`.
//! [`crate::AwsmRenderer::set_anti_aliasing`] compiles the missing variant
//! on demand. Once compiled, a variant stays cached even after the
//! user toggles back — so MSAA-flipping back and forth pays the
//! compile cost only on the first transition in each direction.

use crate::anti_alias::AntiAliasing;
use crate::dynamic_materials::BucketEntry;
use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::material_classify::{
    bind_group::MaterialClassifyBindGroups, shader::cache_key::ShaderCacheKeyMaterialClassify,
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct MaterialClassifyPipelines {
    pub multisampled_pipeline_key: Option<ComputePipelineKey>,
    pub singlesampled_pipeline_key: Option<ComputePipelineKey>,
}

/// Output of [`MaterialClassifyPipelines::build_descriptors`]. The
/// `slot_msaa` field records which (Some(4) / None) MSAA each entry
/// in `pipeline_cache_keys` belongs to, so the recompile path can
/// merge them back into the right `Option` field via
/// [`MaterialClassifyPipelines::merge_resolved`].
pub struct MaterialClassifyPrewarmDescriptors {
    pub pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
    pub slot_msaa: Vec<Option<u32>>,
}

impl MaterialClassifyPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialClassifyBindGroups,
        bucket_entries: &[BucketEntry],
    ) -> Result<Self> {
        ctx.shaders
            .ensure_keys(
                ctx.gpu,
                Self::shader_cache_keys(ctx.gpu, bucket_entries, ctx.anti_aliasing),
            )
            .await?;
        let descs = Self::build_descriptors(ctx, bind_groups, bucket_entries).await?;
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
        Ok(Self::from_resolved(descs.slot_msaa, pipeline_keys))
    }

    /// Shader cache keys for the live AA config only. Previously
    /// emitted both MSAA variants unconditionally — the lazy path
    /// drops the unused one. Reduces classify-pass shader+pipeline
    /// compiles 2× at cold-boot.
    pub fn shader_cache_keys(
        gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
        bucket_entries: &[BucketEntry],
        anti_aliasing: &AntiAliasing,
    ) -> Vec<ShaderCacheKey> {
        let active_msaa = match anti_aliasing.msaa_sample_count {
            Some(4) => Some(4),
            _ => None,
        };
        vec![ShaderCacheKey::from(ShaderCacheKeyMaterialClassify {
            msaa_sample_count: active_msaa,
            bucket_entries: bucket_entries.to_vec(),
            // Priority-3 edge data emission requires MSAA + device
            // support for the full Stage 3 dispatch wiring (5 bind
            // groups, 11 storage buffers). When the device caps out
            // below those limits, we fall back to the inline
            // `msaa_resolve_samples` path in the primary opaque shader.
            emit_edge_data: active_msaa.is_some() && crate::edge_resolve_supported(gpu),
        })]
    }

    pub async fn build_descriptors(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialClassifyBindGroups,
        bucket_entries: &[BucketEntry],
    ) -> Result<MaterialClassifyPrewarmDescriptors> {
        Self::build_descriptors_for_config(
            ctx.gpu,
            ctx.bind_group_layouts,
            ctx.pipeline_layouts,
            ctx.shaders,
            bind_groups,
            bucket_entries,
            ctx.anti_aliasing,
        )
        .await
    }

    /// Live-config descriptor builder used by both the initial
    /// `new()` path and the mid-session
    /// [`crate::AwsmRenderer::set_anti_aliasing`] recompile. Takes the AA
    /// config explicitly so the recompile flow can target the
    /// *incoming* state without rewriting renderer fields first.
    pub async fn build_descriptors_for_config(
        gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
        bind_group_layouts: &mut crate::bind_group_layout::BindGroupLayouts,
        pipeline_layouts: &mut crate::pipeline_layouts::PipelineLayouts,
        shaders: &mut crate::shaders::Shaders,
        bind_groups: &MaterialClassifyBindGroups,
        bucket_entries: &[BucketEntry],
        anti_aliasing: &AntiAliasing,
    ) -> Result<MaterialClassifyPrewarmDescriptors> {
        let (active_msaa, bgl_key) = match anti_aliasing.msaa_sample_count {
            Some(4) => (Some(4), bind_groups.multisampled_bind_group_layout_key),
            _ => (None, bind_groups.singlesampled_bind_group_layout_key),
        };

        let pipeline_layout_key = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bgl_key]),
        )?;

        let shader_key = shaders
            .get_key(
                gpu,
                ShaderCacheKeyMaterialClassify {
                    msaa_sample_count: active_msaa,
                    bucket_entries: bucket_entries.to_vec(),
                    emit_edge_data: active_msaa.is_some() && crate::edge_resolve_supported(gpu),
                },
            )
            .await?;

        Ok(MaterialClassifyPrewarmDescriptors {
            pipeline_cache_keys: vec![ComputePipelineCacheKey::new(
                shader_key,
                pipeline_layout_key,
            )],
            slot_msaa: vec![active_msaa],
        })
    }

    pub fn from_resolved(
        slot_msaa: Vec<Option<u32>>,
        pipeline_keys: Vec<ComputePipelineKey>,
    ) -> Self {
        let mut s = Self {
            multisampled_pipeline_key: None,
            singlesampled_pipeline_key: None,
        };
        s.merge_resolved(slot_msaa, pipeline_keys);
        s
    }

    /// Merge a fresh batch of resolved pipelines into `self` without
    /// dropping already-compiled variants. Used by the recompile path
    /// so toggling MSAA back and forth doesn't re-trigger compiles
    /// for variants we've already built.
    pub fn merge_resolved(
        &mut self,
        slot_msaa: Vec<Option<u32>>,
        pipeline_keys: Vec<ComputePipelineKey>,
    ) {
        for (msaa, key) in slot_msaa.into_iter().zip(pipeline_keys) {
            match msaa {
                Some(4) => self.multisampled_pipeline_key = Some(key),
                _ => self.singlesampled_pipeline_key = Some(key),
            }
        }
    }

    /// Drop the cached pipeline-key references (the classify shader is keyed on
    /// the bucket set, so a bucket-SET change makes both stale). Called from
    /// `relayout_bucket_buffers` BEFORE the pipeline-pool sweep so the evicted
    /// pool entries aren't left dangling here — the next render recompiles the
    /// classify pipeline for the new bucket layout. Part of the dynamic-material
    /// pipeline-leak fix; see docs/plans/mesh-pipeline-overhaul.md.
    pub fn clear_dynamic_pipelines(&mut self) -> Vec<ComputePipelineKey> {
        let mut dropped = Vec::new();
        dropped.extend(self.multisampled_pipeline_key.take());
        dropped.extend(self.singlesampled_pipeline_key.take());
        dropped
    }
}
