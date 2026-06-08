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
///
/// Re-exposed at `pub(crate)` so the lazy-pool recompile path
/// ([`crate::AwsmRenderer::set_anti_aliasing`]) can build descriptors
/// for the *next* config and feed them back into `merge_resolved`.
pub(crate) struct OpaqueShaderDesc {
    pub(crate) shader_cache: crate::shaders::ShaderCacheKey,
    pub(crate) layout_key: PipelineLayoutKey,
    pub(crate) slot: OpaquePipelineSlot,
}

/// Compute pipelines for the opaque material pass.
///
/// `main` is keyed by `(msaa, mipmaps, shader_id)`; `empty_*` are the
/// "no geometry — just skybox" fallbacks (one per MSAA mode).
///
/// **Lazy-pool semantics:** at construction time we only compile
/// the variants matching the live `AntiAliasing` config — typically
/// 4 entries in `main` (one per first-party shader_id at the active
/// msaa+mipmap state) plus 1 empty pipeline for the active MSAA.
/// The other axes' variants are compiled on demand via
/// [`crate::AwsmRenderer::set_anti_aliasing`] (msaa/mipmap change) or
/// [`crate::AwsmRenderer::prewarm_pipelines`] (dynamic-material register).
/// Both lookups (`get_compute_pipeline_key`,
/// `get_empty_compute_pipeline_key`) already return `Option`, so the
/// dispatch path's "skip if missing" branch is the right behavior
/// before the recompile lands.
pub struct MaterialOpaquePipelines {
    main: HashMap<PipelineKeyId, ComputePipelineKey>,
    /// `None` until the user-facing config selects MSAA=Some(4) AT
    /// LEAST once. Same for the singlesampled twin.
    msaa_4_empty_compute_pipeline_key: Option<ComputePipelineKey>,
    singlesampled_empty_compute_pipeline_key: Option<ComputePipelineKey>,
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
    /// per-variant shader descriptors for the *live* anti-aliasing
    /// config. Sync, no `ensure_keys`. Pure cache-key construction.
    ///
    /// **Lazy-pool reduction:** the previous build compiled all
    /// `[Some(4), None] × [true, false] × 4 shader_ids = 16` main
    /// variants + 2 empty variants = 18 entries. Now we emit
    /// `4 main + 1 empty = 5` for the configured MSAA + mipmap state.
    /// The other variants get compiled on demand by
    /// [`Self::recompile_for_anti_aliasing`] when the user changes
    /// MSAA / mipmap mode.
    fn shader_descriptors_and_layouts(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialOpaqueBindGroups,
    ) -> Result<Vec<OpaqueShaderDesc>> {
        // First-party extension: the eager-batch
        // path (called from `AwsmRendererBuilder::build`) emits ONLY
        // the empty-opaque pipeline. First-party material opaque
        // pipelines (PBR / UNLIT / TOON / FLIPBOOK) defer until the
        // render-driven `ensure_scene_pipelines` compiles them — a
        // gltf-driven material register flags the reconcile that drives it.
        //
        // At builder-build time no dynamic material can be registered
        // yet (build() returns the renderer before any
        // `register_material` call), so the empty-state bucket list
        // is exactly `first_party_bucket_entries()`.
        let bucket_entries = crate::dynamic_materials::first_party_bucket_entries();
        Self::shader_descriptors_for_config_with(
            ctx.gpu,
            ctx.bind_group_layouts,
            ctx.pipeline_layouts,
            bind_groups,
            ctx.anti_aliasing,
            &bucket_entries,
            false,
        )
    }

    /// Extension to first-party materials: emit
    /// shader descriptors with the OPAQUE_SHADER_IDS iteration
    /// gated by `include_first_party`. When `false`, only the
    /// empty-opaque pipeline is emitted — first-party pipelines
    /// (PBR / UNLIT / TOON / FLIPBOOK) compile lazily via the
    /// render-driven `AwsmRenderer::ensure_scene_pipelines` once a
    /// material registers. Cold-boot on a zero-scene compiles 0 material
    /// pipelines (was 4).
    pub(crate) fn shader_descriptors_for_config_with(
        gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
        bind_group_layouts: &mut crate::bind_group_layout::BindGroupLayouts,
        pipeline_layouts: &mut crate::pipeline_layouts::PipelineLayouts,
        bind_groups: &MaterialOpaqueBindGroups,
        anti_aliasing: &AntiAliasing,
        bucket_entries: &[crate::dynamic_materials::BucketEntry],
        include_first_party: bool,
    ) -> Result<Vec<OpaqueShaderDesc>> {
        // Which (main_bgl, slot) is active? Only emit the descriptors
        // for the live MSAA branch — the other half stays uncompiled
        // until the next `recompile_for_anti_aliasing` lands.
        let (active_msaa, main_bgl, empty_slot) = match anti_aliasing.msaa_sample_count {
            Some(4) => (
                Some(4_u32),
                bind_groups.multisampled_main_bind_group_layout_key,
                OpaquePipelineSlot::EmptyMsaa4,
            ),
            // Treat any other request (None or unsupported sample count)
            // as singlesampled. The dispatch path's `get_compute_pipeline_key`
            // already returns `None` for unsupported sample counts, so the
            // worst case is a skipped opaque dispatch (renderer falls back
            // to clear color), not a panic.
            _ => (
                None,
                bind_groups.singlesampled_main_bind_group_layout_key,
                OpaquePipelineSlot::EmptySingle,
            ),
        };
        let active_mipmaps = anti_aliasing.mipmap;

        let layout_key = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![
                main_bgl,
                bind_groups.lights_bind_group_layout_key,
                bind_groups.texture_pool_textures_bind_group_layout_key,
                bind_groups.shadows_bind_group_layout_key,
            ]),
        )?;

        let texture_pool_arrays_len = bind_groups.texture_pool_arrays_len;
        let texture_pool_samplers_len = bind_groups.texture_pool_sampler_keys.len() as u32;

        let mut shader_descs: Vec<OpaqueShaderDesc> =
            Vec::with_capacity(OPAQUE_SHADER_IDS.len() + 1);

        if include_first_party {
            for &shader_id in OPAQUE_SHADER_IDS {
                shader_descs.push(OpaqueShaderDesc {
                    shader_cache: ShaderCacheKeyMaterialOpaque {
                        texture_pool_arrays_len,
                        texture_pool_samplers_len,
                        msaa_sample_count: active_msaa,
                        mipmaps: active_mipmaps,
                        shader_id,
                        base: crate::dynamic_materials::ShadingBase::for_shader_id(shader_id),
                        owns_skybox: shader_id == MaterialShaderId::PBR,
                        // Per-bucket feature-set from the bucket entry (never
                        // the full "uber" set). At build() only the canonical
                        // buckets exist, so this is the empty set for PBR /
                        // inert for the rest.
                        pbr_features: bucket_entries
                            .iter()
                            .find(|e| e.shader_id == shader_id)
                            .map(|e| e.pbr_features)
                            .unwrap_or_else(|| awsm_materials::pbr::PbrFeatures::default().bits()),
                        // Builder-time prewarm — no dynamic materials
                        // can be registered before `build()` returns,
                        // so the stable empty-state sentinel applies.
                        // Mid-session dynamic registrations go through
                        // `prewarm_pipelines` which builds its own cache
                        // keys with the live `dispatch_hash`.
                        dispatch_hash: 0,
                        dynamic_shader: None,
                        bucket_entries: bucket_entries.to_vec(),
                    }
                    .into(),
                    layout_key,
                    slot: OpaquePipelineSlot::Main(PipelineKeyId {
                        msaa_sample_count: active_msaa,
                        mipmaps: active_mipmaps,
                        shader_id,
                    }),
                });
            }
        }
        shader_descs.push(OpaqueShaderDesc {
            shader_cache: ShaderCacheKeyMaterialOpaqueEmpty {
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                msaa_sample_count: active_msaa,
            }
            .into(),
            layout_key,
            slot: empty_slot,
        });

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
    ///
    /// Both empty-pipeline fields default to `None`. The initial
    /// build fills the one matching the live MSAA state; the other
    /// stays `None` until the user calls `set_anti_aliasing`. The
    /// dispatch path's `get_empty_compute_pipeline_key` already
    /// returns `Option`, so a `None` there safely skips the empty
    /// dispatch (which is a no-op render-target clear anyway).
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
            msaa_4_empty_compute_pipeline_key,
            singlesampled_empty_compute_pipeline_key,
        }
    }

    /// Merge a fresh batch of resolved pipelines into `self` without
    /// dropping any previously-compiled variants. Used by
    /// [`crate::AwsmRenderer::set_anti_aliasing`] so toggling MSAA mid-session
    /// preserves the old MSAA's pipelines (which the recompile-on-
    /// every-toggle pattern would otherwise re-compile every cycle).
    pub fn merge_resolved(
        &mut self,
        slots: Vec<OpaquePipelineSlot>,
        pipeline_keys: Vec<ComputePipelineKey>,
    ) {
        for (slot, key) in slots.into_iter().zip(pipeline_keys) {
            match slot {
                OpaquePipelineSlot::Main(id) => {
                    self.main.insert(id, key);
                }
                OpaquePipelineSlot::EmptyMsaa4 => {
                    self.msaa_4_empty_compute_pipeline_key = Some(key);
                }
                OpaquePipelineSlot::EmptySingle => {
                    self.singlesampled_empty_compute_pipeline_key = Some(key);
                }
            }
        }
    }

    /// Returns the empty pipeline key for the current MSAA state.
    /// Returns `None` if the variant hasn't been compiled yet — the
    /// dispatch path treats that as "skip the empty pass", which is
    /// a no-op render-target clear so the renderer continues drawing.
    /// The caller is expected to have invoked
    /// [`crate::AwsmRenderer::set_anti_aliasing`] before changing MSAA mode,
    /// which ensures the variant is compiled before the next render.
    pub fn get_empty_compute_pipeline_key(
        &self,
        anti_aliasing: &AntiAliasing,
    ) -> Option<ComputePipelineKey> {
        match anti_aliasing.msaa_sample_count {
            Some(4) => self.msaa_4_empty_compute_pipeline_key,
            None => self.singlesampled_empty_compute_pipeline_key,
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

    /// Inserts a compiled opaque-compute pipeline for a dynamic
    /// shader_id. Called from `AwsmRenderer::prewarm_pipelines` after
    /// compiling a registered material's per-shader-id pipeline.
    pub fn insert_dynamic_pipeline(
        &mut self,
        key_id: PipelineKeyId,
        pipeline_key: ComputePipelineKey,
    ) {
        self.main.insert(key_id, pipeline_key);
    }

    /// Clear every per-shader-id opaque pipeline entry. The empty-slot
    /// pipelines (`msaa_4_empty_compute_pipeline_key` /
    /// `singlesampled_empty_compute_pipeline_key`) are preserved —
    /// they don't depend on the bucket layout.
    ///
    /// Used by `AwsmRenderer::register_material` to invalidate stale
    /// (shader_id, msaa, mipmaps) entries before relaunching the
    /// compile loop with the new bucket layout. Without this clear,
    /// dispatch in the window between relaunch + scheduler resolution
    /// reads the OLD pipelines (compiled against the previous, smaller
    /// `bucket_entries` list) against the newly-resized classify /
    /// edge buffers — every `<shader>_offset` field has shifted to a
    /// new struct offset, so dispatch fans into the wrong tile lists.
    /// After clearing, `get_compute_pipeline_key` returns `None` for
    /// those entries and the dispatch site's `Option` guard skips
    /// the draw until the new pipeline lands.
    pub fn clear_dynamic_pipelines(&mut self) {
        self.main.clear();
    }
}
