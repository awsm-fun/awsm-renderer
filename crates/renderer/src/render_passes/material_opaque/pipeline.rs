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
pub struct PipelineKeyId {
    pub msaa_sample_count: Option<u32>,
    pub mipmaps: bool,
    pub shader_id: MaterialShaderId,
}

/// One opaque variant before pipeline-key resolution. Internal to
/// the descriptor-build flow; `shader_cache_keys` extracts only the
/// `shader_cache` field, the full descriptor adds the pipeline
/// layout + slot identity needed to fold the result back into the
/// typed struct.
struct OpaqueShaderDesc {
    shader_cache: crate::shaders::ShaderCacheKey,
    layout_key: PipelineLayoutKey,
    slot: OpaquePipelineSlot,
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
    MaterialShaderId::PBR,
    MaterialShaderId::UNLIT,
    MaterialShaderId::TOON,
    MaterialShaderId::FLIPBOOK,
];

/// Slot identifier used by the batched-build path to fold a flat
/// `Vec<ComputePipelineKey>` back into the typed struct.
#[derive(Clone, Copy, Debug)]
pub enum OpaquePipelineSlot {
    Main(PipelineKeyId),
    EmptyMsaa4,
    EmptySingle,
}

/// Pre-resolved opaque pipeline descriptors — the output of
/// [`MaterialOpaquePipelines::build_descriptors`] and the input to
/// [`MaterialOpaquePipelines::from_resolved`]. The pooled
/// finalize-textures path hands these between the cross-pass
/// `ensure_keys` batches and the per-pass assembly.
pub struct MaterialOpaquePipelineDescriptors {
    pub shader_cache_keys: Vec<crate::shaders::ShaderCacheKey>,
    pub pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
    pub slots: Vec<OpaquePipelineSlot>,
}

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
    ///
    /// Thin wrapper over [`Self::build_descriptors`] +
    /// [`Self::from_resolved`]. The pooled-finalize path
    /// (`finalize_gpu_textures`) reuses those primitives directly so
    /// the cross-pass `ensure_keys` batches absorb the opaque +
    /// decal + transparent recompiles into one shader batch + one
    /// compute-pipeline batch + one render-pipeline batch.
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialOpaqueBindGroups,
    ) -> Result<Self> {
        let shader_descs = Self::shader_descriptors_and_layouts(ctx, bind_groups)?;

        // Batch 1: shader compiles.
        ctx.shaders
            .ensure_keys(ctx.gpu, shader_descs.iter().map(|d| d.shader_cache.clone()))
            .await?;

        let descs =
            Self::build_descriptors_from_shader_descs(ctx.gpu, ctx.shaders, shader_descs).await?;

        // Batch 2: compute pipeline compiles.
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

        Ok(Self::from_resolved(descs.slots, pipeline_keys))
    }

    /// Resolves the bind-group-derived pipeline layout keys + the
    /// per-variant shader descriptors. Sync, no `ensure_keys`. Pure
    /// cache-key construction.
    fn shader_descriptors_and_layouts(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialOpaqueBindGroups,
    ) -> Result<Vec<OpaqueShaderDesc>> {
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

        let mut shader_descs: Vec<OpaqueShaderDesc> =
            Vec::with_capacity(OPAQUE_SHADER_IDS.len() * 4 + 2);

        for &shader_id in OPAQUE_SHADER_IDS {
            for &(msaa, layout_key) in &[
                (Some(4_u32), multisampled_pipeline_layout_key),
                (None, singlesampled_pipeline_layout_key),
            ] {
                for &mipmaps in &[true, false] {
                    shader_descs.push(OpaqueShaderDesc {
                        shader_cache: ShaderCacheKeyMaterialOpaque {
                            texture_pool_arrays_len,
                            texture_pool_samplers_len,
                            msaa_sample_count: msaa,
                            mipmaps,
                            shader_id,
                            // Builder-time prewarm — no dynamic materials
                            // can be registered before `build()` returns,
                            // so the stable empty-state sentinel applies.
                            // Phase 4+ wires this from
                            // `dynamic_materials.dispatch_hash()` at
                            // ensure_keys time so a registration triggers
                            // the right pipeline recompile.
                            dispatch_hash: 0,
                        }
                        .into(),
                        layout_key,
                        slot: OpaquePipelineSlot::Main(PipelineKeyId {
                            msaa_sample_count: msaa,
                            mipmaps,
                            shader_id,
                        }),
                    });
                }
            }
        }
        shader_descs.push(OpaqueShaderDesc {
            shader_cache: ShaderCacheKeyMaterialOpaqueEmpty {
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                msaa_sample_count: Some(4),
            }
            .into(),
            layout_key: multisampled_pipeline_layout_key,
            slot: OpaquePipelineSlot::EmptyMsaa4,
        });
        shader_descs.push(OpaqueShaderDesc {
            shader_cache: ShaderCacheKeyMaterialOpaqueEmpty {
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                msaa_sample_count: None,
            }
            .into(),
            layout_key: singlesampled_pipeline_layout_key,
            slot: OpaquePipelineSlot::EmptySingle,
        });

        let _ = (
            multisampled_pipeline_layout_key,
            singlesampled_pipeline_layout_key,
        );
        Ok(shader_descs)
    }

    /// Returns the shader cache keys this pass would compile if its
    /// `new` were called. Used by the pooled `finalize_gpu_textures`
    /// path to merge opaque + decal + transparent shader-warm into
    /// one cross-pass `Shaders::ensure_keys`.
    pub fn build_shader_cache_keys(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialOpaqueBindGroups,
    ) -> Result<Vec<crate::shaders::ShaderCacheKey>> {
        let shader_descs = Self::shader_descriptors_and_layouts(ctx, bind_groups)?;
        Ok(shader_descs.into_iter().map(|d| d.shader_cache).collect())
    }

    /// Builds the full descriptor blob (shader cache keys, resolved
    /// pipeline cache keys, slots). Requires that the shader keys
    /// have already been ensured in `shaders` — call
    /// `Shaders::ensure_keys` with [`Self::build_shader_cache_keys`]
    /// first.
    pub async fn build_descriptors(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialOpaqueBindGroups,
    ) -> Result<MaterialOpaquePipelineDescriptors> {
        let shader_descs = Self::shader_descriptors_and_layouts(ctx, bind_groups)?;
        Self::build_descriptors_from_shader_descs(ctx.gpu, ctx.shaders, shader_descs).await
    }

    async fn build_descriptors_from_shader_descs(
        gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
        shaders: &mut crate::shaders::Shaders,
        shader_descs: Vec<OpaqueShaderDesc>,
    ) -> Result<MaterialOpaquePipelineDescriptors> {
        let mut shader_cache_keys = Vec::with_capacity(shader_descs.len());
        let mut pipeline_cache_keys = Vec::with_capacity(shader_descs.len());
        let mut slots = Vec::with_capacity(shader_descs.len());

        for d in shader_descs {
            let shader_key = shaders.get_key(gpu, d.shader_cache.clone()).await?;
            shader_cache_keys.push(d.shader_cache);
            pipeline_cache_keys.push(ComputePipelineCacheKey::new(shader_key, d.layout_key));
            slots.push(d.slot);
        }

        Ok(MaterialOpaquePipelineDescriptors {
            shader_cache_keys,
            pipeline_cache_keys,
            slots,
        })
    }

    /// Assembles a `MaterialOpaquePipelines` from a slot-list + the
    /// matching resolved pipeline keys (output of one
    /// `ComputePipelines::ensure_keys` call). Sync; the caller is
    /// responsible for running the actual pipeline compile.
    pub fn from_resolved(
        slots: Vec<OpaquePipelineSlot>,
        pipeline_keys: Vec<ComputePipelineKey>,
    ) -> Self {
        let mut main = HashMap::with_capacity(OPAQUE_SHADER_IDS.len() * 4);
        let mut msaa_4_empty_compute_pipeline_key: Option<ComputePipelineKey> = None;
        let mut singlesampled_empty_compute_pipeline_key: Option<ComputePipelineKey> = None;

        for (slot, key) in slots.into_iter().zip(pipeline_keys) {
            match slot {
                OpaquePipelineSlot::Main(id) => {
                    main.insert(id, key);
                }
                OpaquePipelineSlot::EmptyMsaa4 => {
                    msaa_4_empty_compute_pipeline_key = Some(key);
                }
                OpaquePipelineSlot::EmptySingle => {
                    singlesampled_empty_compute_pipeline_key = Some(key);
                }
            }
        }

        Self {
            main,
            msaa_4_empty_compute_pipeline_key: msaa_4_empty_compute_pipeline_key
                .expect("empty MSAA-4 pipeline slot must be filled"),
            singlesampled_empty_compute_pipeline_key: singlesampled_empty_compute_pipeline_key
                .expect("empty singlesampled pipeline slot must be filled"),
        }
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
