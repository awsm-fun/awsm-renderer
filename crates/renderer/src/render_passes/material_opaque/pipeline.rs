//! Material opaque pass pipeline setup.
//!
//! Pipelines are cached per `(msaa_sample_count, mipmaps, shader_id)`.
//! With three enabled material shaders (PBR / Unlit / Toon) and
//! two-each MSAA / mipmaps axes, that's a 12-entry cache — built once
//! at construction; lookup is a direct hash hit on the hot path.

use std::collections::HashMap;

use awsm_materials::MaterialShaderId;

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

/// Lookup key for the main opaque-compute pipeline cache.
#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug)]
struct PipelineKeyId {
    msaa_sample_count: Option<u32>,
    mipmaps: bool,
    shader_id: MaterialShaderId,
}

/// Compute pipelines for the opaque material pass.
///
/// `main` is keyed by `(msaa, mipmaps, shader_id)`; `empty_*` are the
/// "no geometry — just skybox" fallbacks (one per MSAA mode).
pub struct MaterialOpaquePipelines {
    main: HashMap<PipelineKeyId, ComputePipelineKey>,
    msaa_4_empty_compute_pipeline_key: ComputePipelineKey,
    singlesampled_empty_compute_pipeline_key: ComputePipelineKey,
}

/// Every opaque-rendering material shader the renderer supports.
/// Mirror of the `MaterialShaderId` variant set in
/// `awsm_materials::shader_id`. Used at construction time to enumerate
/// the pipelines we need to build.
const OPAQUE_SHADER_IDS: &[MaterialShaderId] = &[
    MaterialShaderId::Pbr,
    MaterialShaderId::Unlit,
    MaterialShaderId::Toon,
];

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

        // Pre-warm: fire all 14 shader compiles concurrently before
        // creating any pipelines. `Shaders::ensure_keys` issues every
        // `compile_shader` synchronously, then awaits all validations
        // in parallel — so the browser can compile the (3 shader_id
        // × 2 msaa × 2 mipmaps = 12 main) + (2 empty) variants on its
        // shader-compile pool instead of strict serial. Without this,
        // every pipeline-layout-changing event (renderer init, every
        // texture-pool dirty cycle on model insert) paid 14 × wall-
        // clock per-shader cost.
        let mut warmup_keys: Vec<crate::shaders::ShaderCacheKey> =
            Vec::with_capacity(OPAQUE_SHADER_IDS.len() * 4 + 2);
        for &shader_id in OPAQUE_SHADER_IDS {
            for &msaa in &[Some(4_u32), None] {
                for &mipmaps in &[true, false] {
                    warmup_keys.push(
                        ShaderCacheKeyMaterialOpaque {
                            texture_pool_arrays_len,
                            texture_pool_samplers_len,
                            msaa_sample_count: msaa,
                            mipmaps,
                            shader_id,
                        }
                        .into(),
                    );
                }
            }
        }
        for &msaa in &[Some(4_u32), None] {
            warmup_keys.push(
                ShaderCacheKeyMaterialOpaqueEmpty {
                    texture_pool_arrays_len,
                    texture_pool_samplers_len,
                    msaa_sample_count: msaa,
                }
                .into(),
            );
        }
        ctx.shaders.ensure_keys(ctx.gpu, warmup_keys).await?;

        // Build the (msaa × mipmaps × shader_id) cube. Shader compiles
        // are now warm (cache hits), so `create_pipeline` only does
        // the cheaper pipeline-creation work; no per-iteration shader
        // wall-clock cost.
        let mut main = HashMap::with_capacity(OPAQUE_SHADER_IDS.len() * 4);
        for &shader_id in OPAQUE_SHADER_IDS {
            for &(msaa, layout_key) in &[
                (Some(4_u32), multisampled_pipeline_layout_key),
                (None, singlesampled_pipeline_layout_key),
            ] {
                for &mipmaps in &[true, false] {
                    let key = Self::create_pipeline(
                        ctx,
                        texture_pool_arrays_len,
                        texture_pool_samplers_len,
                        msaa,
                        mipmaps,
                        shader_id,
                        layout_key,
                    )
                    .await?;
                    main.insert(
                        PipelineKeyId {
                            msaa_sample_count: msaa,
                            mipmaps,
                            shader_id,
                        },
                        key,
                    );
                }
            }
        }

        // Empty pipelines (skybox-only path). One per MSAA mode — no
        // material-shader specialization needed; the empty template
        // does nothing material-dependent.
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
            main,
            msaa_4_empty_compute_pipeline_key,
            singlesampled_empty_compute_pipeline_key,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_pipeline(
        ctx: &mut RenderPassInitContext<'_>,
        texture_pool_arrays_len: u32,
        texture_pool_samplers_len: u32,
        msaa_sample_count: Option<u32>,
        mipmaps: bool,
        shader_id: MaterialShaderId,
        pipeline_layout_key: PipelineLayoutKey,
    ) -> Result<ComputePipelineKey> {
        let shader_cache_key = ShaderCacheKeyMaterialOpaque {
            texture_pool_arrays_len,
            texture_pool_samplers_len,
            msaa_sample_count,
            mipmaps,
            shader_id,
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

    /// Returns the opaque material pipeline key for a given mesh's
    /// effective material `shader_id`. Each material flavour
    /// (PBR / Unlit / Toon) routes to its own specialized compute
    /// pipeline so the runtime branch in the shader becomes a static
    /// template choice.
    pub fn get_compute_pipeline_key(
        &self,
        anti_aliasing: &AntiAliasing,
        shader_id: MaterialShaderId,
    ) -> Option<ComputePipelineKey> {
        let msaa = match anti_aliasing.msaa_sample_count {
            Some(4) => Some(4_u32),
            None => None,
            // Adapter requested an MSAA mode the pipeline cache wasn't
            // built for — bail; the renderer will fall back to the
            // empty dispatch and skip the material pass.
            _ => return None,
        };
        self.main
            .get(&PipelineKeyId {
                msaa_sample_count: msaa,
                mipmaps: anti_aliasing.mipmap,
                shader_id,
            })
            .copied()
    }
}
