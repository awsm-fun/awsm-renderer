//! Material opaque pass pipeline setup.

use crate::anti_alias::AntiAliasing;
use crate::error::Result;
use crate::pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey};
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::material_opaque::shader::cache_key::ShaderCacheKeyMaterialOpaqueEmpty;
use crate::render_passes::{
    material_opaque::{
        bind_group::MaterialOpaqueBindGroups, shader::cache_key::ShaderCacheKeyMaterialOpaque,
    },
    RenderPassInitContext,
};

/// Compute pipelines for the opaque material pass.
pub struct MaterialOpaquePipelines {
    // Pipeline variants based on MSAA and mipmap settings
    msaa_4_mipmaps_compute_pipeline_key: ComputePipelineKey,
    msaa_4_no_mipmaps_compute_pipeline_key: ComputePipelineKey,
    singlesampled_mipmaps_compute_pipeline_key: ComputePipelineKey,
    singlesampled_no_mipmaps_compute_pipeline_key: ComputePipelineKey,
    // Empty variants (no geometry, just skybox)
    msaa_4_empty_compute_pipeline_key: ComputePipelineKey,
    singlesampled_empty_compute_pipeline_key: ComputePipelineKey,
}

impl MaterialOpaquePipelines {
    /// Creates pipelines for the opaque material pass.
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialOpaqueBindGroups,
    ) -> Result<Self> {
        let multisampled_pipeline_layout_cache_key = PipelineLayoutCacheKey::new(vec![
            bind_groups.multisampled_main_bind_group_layout_key,
            bind_groups.lights_bind_group_layout_key,
            bind_groups.texture_pool_textures_bind_group_layout_key,
            bind_groups.shadows_bind_group_layout_key,
        ]);
        let multisampled_pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            multisampled_pipeline_layout_cache_key,
        )?;

        let singlesampled_pipeline_layout_cache_key = PipelineLayoutCacheKey::new(vec![
            bind_groups.singlesampled_main_bind_group_layout_key,
            bind_groups.lights_bind_group_layout_key,
            bind_groups.texture_pool_textures_bind_group_layout_key,
            bind_groups.shadows_bind_group_layout_key,
        ]);
        let singlesampled_pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            singlesampled_pipeline_layout_cache_key,
        )?;

        let texture_pool_arrays_len = bind_groups.texture_pool_arrays_len;
        let texture_pool_samplers_len = bind_groups.texture_pool_sampler_keys.len() as u32;

        // Create all 4 main pipeline variants (MSAA × mipmaps)
        let msaa_4_mipmaps_compute_pipeline_key = Self::create_pipeline(
            ctx,
            texture_pool_arrays_len,
            texture_pool_samplers_len,
            Some(4),
            true,
            multisampled_pipeline_layout_key,
        )
        .await?;

        let msaa_4_no_mipmaps_compute_pipeline_key = Self::create_pipeline(
            ctx,
            texture_pool_arrays_len,
            texture_pool_samplers_len,
            Some(4),
            false,
            multisampled_pipeline_layout_key,
        )
        .await?;

        let singlesampled_mipmaps_compute_pipeline_key = Self::create_pipeline(
            ctx,
            texture_pool_arrays_len,
            texture_pool_samplers_len,
            None,
            true,
            singlesampled_pipeline_layout_key,
        )
        .await?;

        let singlesampled_no_mipmaps_compute_pipeline_key = Self::create_pipeline(
            ctx,
            texture_pool_arrays_len,
            texture_pool_samplers_len,
            None,
            false,
            singlesampled_pipeline_layout_key,
        )
        .await?;

        // Create empty pipeline variants (for skybox-only rendering)
        let msaa_4_empty_compute_pipeline_key = Self::create_empty_pipeline(
            ctx,
            texture_pool_arrays_len,
            texture_pool_samplers_len,
            Some(4),
            multisampled_pipeline_layout_key,
        )
        .await?;

        let singlesampled_empty_compute_pipeline_key = Self::create_empty_pipeline(
            ctx,
            texture_pool_arrays_len,
            texture_pool_samplers_len,
            None,
            singlesampled_pipeline_layout_key,
        )
        .await?;

        Ok(Self {
            msaa_4_mipmaps_compute_pipeline_key,
            msaa_4_no_mipmaps_compute_pipeline_key,
            singlesampled_mipmaps_compute_pipeline_key,
            singlesampled_no_mipmaps_compute_pipeline_key,
            msaa_4_empty_compute_pipeline_key,
            singlesampled_empty_compute_pipeline_key,
        })
    }

    async fn create_pipeline(
        ctx: &mut RenderPassInitContext<'_>,
        texture_pool_arrays_len: u32,
        texture_pool_samplers_len: u32,
        msaa_sample_count: Option<u32>,
        mipmaps: bool,
        pipeline_layout_key: PipelineLayoutKey,
    ) -> Result<ComputePipelineKey> {
        let shader_cache_key = ShaderCacheKeyMaterialOpaque {
            texture_pool_arrays_len,
            texture_pool_samplers_len,
            msaa_sample_count,
            mipmaps,
        };

        let shader_key = ctx.shaders.get_key(ctx.gpu, shader_cache_key).await?;

        let compute_pipeline_cache_key =
            ComputePipelineCacheKey::new(shader_key, pipeline_layout_key);

        Ok(ctx
            .pipelines
            .compute
            .get_key(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                compute_pipeline_cache_key,
            )
            .await?)
    }

    async fn create_empty_pipeline(
        ctx: &mut RenderPassInitContext<'_>,
        texture_pool_arrays_len: u32,
        texture_pool_samplers_len: u32,
        msaa_sample_count: Option<u32>,
        pipeline_layout_key: PipelineLayoutKey,
    ) -> Result<ComputePipelineKey> {
        let shader_cache_key = ShaderCacheKeyMaterialOpaqueEmpty {
            texture_pool_arrays_len,
            texture_pool_samplers_len,
            msaa_sample_count,
        };

        let shader_key = ctx.shaders.get_key(ctx.gpu, shader_cache_key).await?;

        let compute_pipeline_cache_key =
            ComputePipelineCacheKey::new(shader_key, pipeline_layout_key);

        Ok(ctx
            .pipelines
            .compute
            .get_key(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                compute_pipeline_cache_key,
            )
            .await?)
    }

    /// Returns the empty pipeline key for the current MSAA state.
    pub fn get_empty_compute_pipeline_key(
        &self,
        anti_aliasing: &AntiAliasing,
    ) -> Option<ComputePipelineKey> {
        match anti_aliasing.msaa_sample_count {
            Some(4) => Some(self.msaa_4_empty_compute_pipeline_key),
            None => Some(self.singlesampled_empty_compute_pipeline_key),
            _ => None,
        }
    }

    /// Returns the opaque material pipeline key for the current options.
    pub fn get_compute_pipeline_key(
        &self,
        anti_aliasing: &AntiAliasing,
    ) -> Option<ComputePipelineKey> {
        match (anti_aliasing.msaa_sample_count, anti_aliasing.mipmap) {
            (Some(4), true) => Some(self.msaa_4_mipmaps_compute_pipeline_key),
            (Some(4), false) => Some(self.msaa_4_no_mipmaps_compute_pipeline_key),
            (None, true) => Some(self.singlesampled_mipmaps_compute_pipeline_key),
            (None, false) => Some(self.singlesampled_no_mipmaps_compute_pipeline_key),
            _ => None,
        }
    }
}
