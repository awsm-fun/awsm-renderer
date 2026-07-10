//! Compute pipeline for the material decal pass.

use crate::error::Result;
use crate::pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey};
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::material_decal::{
    bind_group::MaterialDecalBindGroups, shader::cache_key::ShaderCacheKeyMaterialDecal,
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

/// Compute pipelines for the decal pass — one per MSAA mode.
/// Both pipelines write to a single-sample `decal_color` (via the
/// shared binding shape); the MSAA path then alpha-blits it onto the
/// frame's `transparent` via a composite step.
pub struct MaterialDecalPipelines {
    pub singlesampled_pipeline_key: ComputePipelineKey,
    pub multisampled_pipeline_key: ComputePipelineKey,
}

/// One decal variant before pipeline-key resolution. Used by the
/// pooled finalize-textures path so opaque + decal + transparent
/// share one cross-pass `Shaders::ensure_keys` batch.
struct DecalShaderDesc {
    shader_cache: ShaderCacheKey,
    layout_key: PipelineLayoutKey,
    is_msaa: bool,
}

/// Pre-resolved decal pipeline descriptors — output of
/// [`MaterialDecalPipelines::build_descriptors`] and input to
/// [`MaterialDecalPipelines::from_resolved`].
pub struct MaterialDecalPipelineDescriptors {
    pub shader_cache_keys: Vec<ShaderCacheKey>,
    pub pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
    /// True for the MSAA-4 variant slot, false for singlesampled.
    pub is_msaa: Vec<bool>,
}

impl MaterialDecalPipelines {
    /// Builds both MSAA variants concurrently via batched
    /// `Shaders::ensure_keys` + `ComputePipelines::ensure_keys`.
    ///
    /// Thin wrapper over [`Self::build_descriptors`] +
    /// [`Self::from_resolved`]. The pooled-finalize path uses those
    /// directly.
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialDecalBindGroups,
    ) -> Result<Self> {
        let shader_descs = Self::shader_descs(ctx, bind_groups)?;

        ctx.shaders
            .ensure_keys(ctx.gpu, shader_descs.iter().map(|d| d.shader_cache.clone()))
            .await?;

        let descs =
            Self::build_descriptors_from_shader_descs(ctx.gpu, ctx.shaders, shader_descs).await?;

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

        Ok(Self::from_resolved(descs.is_msaa, pipeline_keys))
    }

    fn shader_descs(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialDecalBindGroups,
    ) -> Result<Vec<DecalShaderDesc>> {
        let singlesampled_pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![
                bind_groups.main_layout_key_singlesampled,
                bind_groups.texture_pool_layout_key,
            ]),
        )?;
        let multisampled_pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![
                bind_groups.main_layout_key_multisampled,
                bind_groups.texture_pool_layout_key,
            ]),
        )?;

        // A.4: the divisor packing a decal's flat `texture_index` into the pool's
        // (array_index, layer_index). The scene-loader packs with the SAME value
        // via `decals::decal_texture_index_stride` — single source of truth.
        let texture_pool_layers_per_array = crate::decals::decal_texture_index_stride(ctx.gpu);
        let reverse_z = ctx.features.reverse_z;

        Ok(vec![
            DecalShaderDesc {
                shader_cache: ShaderCacheKey::from(ShaderCacheKeyMaterialDecal {
                    msaa_sample_count: None,
                    texture_pool_arrays_len: bind_groups.texture_pool_arrays_len,
                    texture_pool_samplers_len: bind_groups.texture_pool_samplers_len,
                    texture_pool_layers_per_array,
                    reverse_z,
                }),
                layout_key: singlesampled_pipeline_layout_key,
                is_msaa: false,
            },
            DecalShaderDesc {
                shader_cache: ShaderCacheKey::from(ShaderCacheKeyMaterialDecal {
                    msaa_sample_count: Some(4),
                    texture_pool_arrays_len: bind_groups.texture_pool_arrays_len,
                    texture_pool_samplers_len: bind_groups.texture_pool_samplers_len,
                    texture_pool_layers_per_array,
                    reverse_z,
                }),
                layout_key: multisampled_pipeline_layout_key,
                is_msaa: true,
            },
        ])
    }

    /// Shader cache keys this pass would compile.
    pub fn build_shader_cache_keys(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialDecalBindGroups,
    ) -> Result<Vec<ShaderCacheKey>> {
        Ok(Self::shader_descs(ctx, bind_groups)?
            .into_iter()
            .map(|d| d.shader_cache)
            .collect())
    }

    /// Builds descriptors with shader keys resolved (requires
    /// `Shaders::ensure_keys` of [`Self::build_shader_cache_keys`]).
    pub async fn build_descriptors(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialDecalBindGroups,
    ) -> Result<MaterialDecalPipelineDescriptors> {
        let shader_descs = Self::shader_descs(ctx, bind_groups)?;
        Self::build_descriptors_from_shader_descs(ctx.gpu, ctx.shaders, shader_descs).await
    }

    async fn build_descriptors_from_shader_descs(
        gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
        shaders: &mut crate::shaders::Shaders,
        shader_descs: Vec<DecalShaderDesc>,
    ) -> Result<MaterialDecalPipelineDescriptors> {
        let mut shader_cache_keys = Vec::with_capacity(shader_descs.len());
        let mut pipeline_cache_keys = Vec::with_capacity(shader_descs.len());
        let mut is_msaa = Vec::with_capacity(shader_descs.len());

        for d in shader_descs {
            let shader_key = shaders.get_key(gpu, d.shader_cache.clone()).await?;
            shader_cache_keys.push(d.shader_cache);
            pipeline_cache_keys.push(ComputePipelineCacheKey::new(shader_key, d.layout_key));
            is_msaa.push(d.is_msaa);
        }

        Ok(MaterialDecalPipelineDescriptors {
            shader_cache_keys,
            pipeline_cache_keys,
            is_msaa,
        })
    }

    /// Assemble from a slot list + matching resolved pipeline keys.
    pub fn from_resolved(is_msaa: Vec<bool>, pipeline_keys: Vec<ComputePipelineKey>) -> Self {
        let mut singlesampled_pipeline_key: Option<ComputePipelineKey> = None;
        let mut multisampled_pipeline_key: Option<ComputePipelineKey> = None;
        for (msaa, key) in is_msaa.into_iter().zip(pipeline_keys) {
            if msaa {
                multisampled_pipeline_key = Some(key);
            } else {
                singlesampled_pipeline_key = Some(key);
            }
        }
        Self {
            singlesampled_pipeline_key: singlesampled_pipeline_key
                .expect("decal singlesampled pipeline slot must be filled"),
            multisampled_pipeline_key: multisampled_pipeline_key
                .expect("decal multisampled pipeline slot must be filled"),
        }
    }
}
