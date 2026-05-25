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
    MaterialShaderId::FlipBook,
];

impl MaterialOpaquePipelines {
    /// Creates pipelines for the opaque material pass.
    ///
    /// Two batched compile passes: first all 14 shader variants
    /// concurrently via `Shaders::ensure_keys`, then all 14 compute
    /// pipelines concurrently via `ComputePipelines::ensure_keys`.
    /// On a cold PSO disk cache that turns the previous 14× per-
    /// shader and 14× per-pipeline strict-serial wall-clock into
    /// roughly `max(t_i)` for each batch (bounded by Dawn's compile
    /// pool size, typically `num_cpus`).
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

        // ------------------------------------------------------------
        // Build descriptors for the 14 variants in input order.
        //
        // Slots 0..(OPAQUE_SHADER_IDS.len() * 4) are the
        // (shader_id, msaa, mipmaps) main cube; the trailing two
        // slots are the MSAA-on/off empty pipelines (skybox-only
        // fallback). Holding two parallel `Vec`s keyed by the same
        // index lets us issue one batched shader-compile + one
        // batched pipeline-compile, then fold the results back into
        // the typed map below.
        // ------------------------------------------------------------
        struct PipelineDesc {
            shader_cache: crate::shaders::ShaderCacheKey,
            layout_key: PipelineLayoutKey,
            slot: PipelineSlot,
        }
        enum PipelineSlot {
            Main(PipelineKeyId),
            EmptyMsaa4,
            EmptySingle,
        }

        let mut pending: Vec<PipelineDesc> = Vec::with_capacity(OPAQUE_SHADER_IDS.len() * 4 + 2);

        for &shader_id in OPAQUE_SHADER_IDS {
            for &(msaa, layout_key) in &[
                (Some(4_u32), multisampled_pipeline_layout_key),
                (None, singlesampled_pipeline_layout_key),
            ] {
                for &mipmaps in &[true, false] {
                    pending.push(PipelineDesc {
                        shader_cache: ShaderCacheKeyMaterialOpaque {
                            texture_pool_arrays_len,
                            texture_pool_samplers_len,
                            msaa_sample_count: msaa,
                            mipmaps,
                            shader_id,
                        }
                        .into(),
                        layout_key,
                        slot: PipelineSlot::Main(PipelineKeyId {
                            msaa_sample_count: msaa,
                            mipmaps,
                            shader_id,
                        }),
                    });
                }
            }
        }
        pending.push(PipelineDesc {
            shader_cache: ShaderCacheKeyMaterialOpaqueEmpty {
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                msaa_sample_count: Some(4),
            }
            .into(),
            layout_key: multisampled_pipeline_layout_key,
            slot: PipelineSlot::EmptyMsaa4,
        });
        pending.push(PipelineDesc {
            shader_cache: ShaderCacheKeyMaterialOpaqueEmpty {
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                msaa_sample_count: None,
            }
            .into(),
            layout_key: singlesampled_pipeline_layout_key,
            slot: PipelineSlot::EmptySingle,
        });

        // Batch 1: 14 shader compiles in parallel.
        ctx.shaders
            .ensure_keys(ctx.gpu, pending.iter().map(|d| d.shader_cache.clone()))
            .await?;

        // Resolve shader keys (all cache hits after ensure_keys).
        let mut pipeline_cache_keys: Vec<ComputePipelineCacheKey> =
            Vec::with_capacity(pending.len());
        for d in &pending {
            let shader_key = ctx.shaders.get_key(ctx.gpu, d.shader_cache.clone()).await?;
            pipeline_cache_keys.push(ComputePipelineCacheKey::new(shader_key, d.layout_key));
        }

        // Batch 2: 14 compute pipelines in parallel.
        let pipeline_keys = ctx
            .pipelines
            .compute
            .ensure_keys(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                pipeline_cache_keys,
            )
            .await?;

        // Fold results back into the typed buckets.
        let mut main = HashMap::with_capacity(OPAQUE_SHADER_IDS.len() * 4);
        let mut msaa_4_empty_compute_pipeline_key: Option<ComputePipelineKey> = None;
        let mut singlesampled_empty_compute_pipeline_key: Option<ComputePipelineKey> = None;
        for (desc, key) in pending.into_iter().zip(pipeline_keys) {
            match desc.slot {
                PipelineSlot::Main(id) => {
                    main.insert(id, key);
                }
                PipelineSlot::EmptyMsaa4 => {
                    msaa_4_empty_compute_pipeline_key = Some(key);
                }
                PipelineSlot::EmptySingle => {
                    singlesampled_empty_compute_pipeline_key = Some(key);
                }
            }
        }

        Ok(Self {
            main,
            msaa_4_empty_compute_pipeline_key: msaa_4_empty_compute_pipeline_key
                .expect("empty MSAA-4 pipeline slot must be filled"),
            singlesampled_empty_compute_pipeline_key: singlesampled_empty_compute_pipeline_key
                .expect("empty singlesampled pipeline slot must be filled"),
        })
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
