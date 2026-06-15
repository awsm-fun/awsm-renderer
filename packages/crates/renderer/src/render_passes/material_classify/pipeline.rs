//! Compute pipeline descriptors for the material classify pass.
//!
//! The classify shader is keyed on the live bucket layout (the registry's
//! `dispatch_hash`) + MSAA, so the compiled pipeline lives in
//! [`MaterialClassifyRenderPass::pipeline_cache`](super::render_pass::MaterialClassifyRenderPass::pipeline_cache),
//! installed by `ensure_scene_pipelines` for the active config. This module is a
//! namespace for the descriptor builders that both the boot pool-warm
//! ([`MaterialClassifyPipelines::warm_pool`]) and the
//! [`crate::AwsmRenderer::set_anti_aliasing`] recompile feed into the scheduler.

use crate::anti_alias::AntiAliasing;
use crate::dynamic_materials::BucketEntry;
use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::ComputePipelineCacheKey;
use crate::render_passes::material_classify::{
    bind_group::MaterialClassifyBindGroups, shader::cache_key::ShaderCacheKeyMaterialClassify,
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

/// Namespace for the classify pipeline descriptor builders. Holds no state —
/// the compiled pipeline is cached per-config in
/// [`MaterialClassifyRenderPass::pipeline_cache`](super::render_pass::MaterialClassifyRenderPass::pipeline_cache).
pub struct MaterialClassifyPipelines;

/// Output of [`MaterialClassifyPipelines::build_descriptors`]. `slot_msaa`
/// records which MSAA (Some(4) / None) each entry in `pipeline_cache_keys`
/// belongs to, so the scheduler install path can key the cache by msaa.
pub struct MaterialClassifyPrewarmDescriptors {
    pub pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
    pub slot_msaa: Vec<Option<u32>>,
}

impl MaterialClassifyPipelines {
    /// Compile the active-config classify pipeline into the shared compute-pool
    /// at boot, so the first `ensure_scene_pipelines` (which installs it into
    /// `pipeline_cache` before the first frame) is a pool hit. The resolved key
    /// isn't stored here — `pipeline_cache` is the single source of truth.
    pub async fn warm_pool(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialClassifyBindGroups,
        bucket_entries: &[BucketEntry],
    ) -> Result<()> {
        ctx.shaders
            .ensure_keys(
                ctx.gpu,
                Self::shader_cache_keys(ctx.gpu, bucket_entries, ctx.anti_aliasing),
            )
            .await?;
        let descs = Self::build_descriptors(ctx, bind_groups, bucket_entries).await?;
        ctx.pipelines
            .compute
            .ensure_keys(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                descs.pipeline_cache_keys.clone(),
            )
            .await?;
        Ok(())
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

    /// Live-config descriptor builder used by both the boot pool-warm
    /// ([`Self::warm_pool`]) and the mid-session
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
}
