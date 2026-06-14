//! Pipeline cache + descriptors for the per-shader-id MSAA edge-resolve
//! pipelines (Priority 3 in https://github.com/dakom/awsm-renderer/pull/99).
//!
//! Three categories of pipeline:
//!
//! 1. **`material_edge_resolve_{shader_id}`** — one per first-party
//!    shader_id (PBR / UNLIT / TOON / FLIPBOOK) plus one per registered
//!    dynamic shader_id. Indirect-dispatched over the shader_id's edge
//!    sample list. Each pipeline contains only its own shading code
//!    (single-sample shading with mask), so the SPIR-V is small —
//!    roughly 1/4 the size of today's primary opaque pipeline.
//!
//! 2. **`skybox_edge_resolve`** — one global. Indirect-dispatched over
//!    the skybox-sample edge list; shades skybox samples + writes to
//!    the accumulator's reserved skybox slot.
//!
//! 3. **`final_blend`** — one global. Indirect-dispatched over edge
//!    pixels. Reads up to 4 accumulator slots per edge pixel, blends
//!    weighted by per-slot sample count, writes to `opaque_tex`.
//!
//! See [§ Pipeline count and packaging](../../../../https://github.com/dakom/awsm-renderer/pull/99#pipeline-count-and-packaging)
//! for the cost model.

use std::collections::{HashMap, HashSet};

use awsm_materials::MaterialShaderId;

use crate::anti_alias::AntiAliasing;
use crate::dynamic_materials::{BucketEntry, DynamicMaterials};
use crate::error::Result;
use crate::pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey};
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::material_opaque::bind_group::MaterialOpaqueBindGroups;
use crate::render_passes::material_opaque::edge_bind_group::MaterialEdgeBindGroupLayouts;
use crate::render_passes::material_opaque::shader::cache_key::DynamicShaderInfo;
use crate::render_passes::material_opaque::shader::edge_cache_key::{
    ShaderCacheKeyMaterialEdgeResolve, ShaderCacheKeyMaterialFinalBlend,
    ShaderCacheKeyMaterialSkyboxEdgeResolve,
};
use crate::shaders::ShaderCacheKey;

/// Lookup key for the per-shader-id edge_resolve pipeline cache.
///
/// Edge_resolve pipelines specialize on `(shader_id, mipmap)` — they
/// don't have MSAA variants because they always run against
/// multisampled geometry (the only context in which edge pixels exist).
#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug)]
pub struct EdgeResolvePipelineKeyId {
    pub mipmaps: bool,
    pub shader_id: MaterialShaderId,
}

/// Slot identity used by the descriptor → resolved-key fold.
#[derive(Clone, Copy, Debug)]
pub enum EdgePipelineSlot {
    /// Per-shader-id edge resolve.
    PerShader(EdgeResolvePipelineKeyId),
    /// Global skybox edge resolve.
    Skybox,
    /// Global final blend compositor.
    FinalBlend,
}

/// Pre-resolved descriptors for the edge-resolve pipelines. Used by
/// both the legacy async [`MaterialEdgePipelines::ensure_compiled`]
/// path AND the scheduler-driven launch path in
/// `pipeline_scheduler::launch` (which feeds the resolved shader
/// keys into `ComputePipelines::ensure_keys_prepare` and pushes the
/// returned promises into the scheduler's `inflight_compile`).
pub struct MaterialEdgePipelineDescriptors {
    /// Shader cache key for each entry (per-shader + skybox + final_blend).
    pub shader_cache_keys: Vec<crate::shaders::ShaderCacheKey>,
    /// Pipeline-layout key per entry (parallel to `shader_cache_keys`
    /// and `slots`). The compile path combines this with the
    /// resolved shader key into the final `ComputePipelineCacheKey`.
    pub pipeline_layout_keys: Vec<PipelineLayoutKey>,
    /// Install identity per entry (parallel to the above two).
    pub slots: Vec<EdgePipelineSlot>,
}

/// Compiled compute pipelines for the MSAA edge-resolve flow.
///
/// **Lazy-pool semantics:** populated lazily — first-party shader_ids
/// land on first mesh insertion; dynamic shader_ids land on
/// `register_material`. Empty at cold-boot. The `skybox_edge_resolve`
/// and `final_blend` pipelines are tiny enough to live in the cold-boot
/// eager set, but for cleanliness they're scheduler-managed and submit
/// on first opaque material registration (see Stage 3.7 wiring TODO).
pub struct MaterialEdgePipelines {
    /// `(shader_id, mipmap) → pipeline key`. First-party + dynamic
    /// shader_ids share this map; dispatch site walks
    /// `bucket_entries_cached()` to enumerate them.
    pub per_shader: HashMap<EdgeResolvePipelineKeyId, ComputePipelineKey>,
    /// Global skybox-sample edge-resolve pipeline. `None` until the
    /// first MSAA opaque material registers.
    pub skybox_edge_resolve_pipeline_key: Option<ComputePipelineKey>,
    /// Global final-blend compositor. `None` until the first MSAA
    /// opaque material registers.
    pub final_blend_pipeline_key: Option<ComputePipelineKey>,
    /// Cached pipeline layout for per-shader-id edge_resolve. Reused
    /// across every shader_id's compile since their bind-group shape
    /// is identical (only the shading body differs).
    pub edge_resolve_layout_key: Option<PipelineLayoutKey>,
    /// Cached pipeline layout for skybox edge resolve.
    pub skybox_edge_resolve_layout_key: Option<PipelineLayoutKey>,
    /// Cached pipeline layout for final blend.
    pub final_blend_layout_key: Option<PipelineLayoutKey>,
    /// The set of compute-pipeline cache keys the CURRENT bucket layout
    /// wants for its edge chain (every per-shader + skybox + final_blend).
    /// Replaced wholesale each time the edge set is (re)built for a layout
    /// (`build_descriptors` consumers: `ensure_compiled` + the scheduler
    /// `launch_edge_resolve_compile`). This is the authoritative
    /// "is-this-edge-pipeline-still-valid?" signal: a background edge
    /// compile that resolves is installed iff its key is still in this set
    /// (i.e. the layout it was built for is still current), and dropped
    /// otherwise. Edge resolve is a property of the LAYOUT, so its install
    /// validity is keyed on layout-content — NOT on any material's
    /// generation (which is why the install path needs no material owner /
    /// no canonical-PBR assumption; see `apply_compile_resolution_inline`).
    desired_keys: HashSet<ComputePipelineCacheKey>,
    /// Edge cache keys with a scheduler compile promise currently in flight.
    /// Cross-call dedup so two layout-change launches in the same window
    /// don't double-compile the same pipeline. Entries are cleared when the
    /// promise resolves (installed or dropped).
    in_flight_keys: HashSet<ComputePipelineCacheKey>,
}

impl Default for MaterialEdgePipelines {
    fn default() -> Self {
        Self::new()
    }
}

impl MaterialEdgePipelines {
    /// Builds an empty pipeline cache. Pipelines populate lazily as
    /// the scheduler resolves their compile futures.
    pub fn new() -> Self {
        Self {
            per_shader: HashMap::new(),
            skybox_edge_resolve_pipeline_key: None,
            final_blend_pipeline_key: None,
            edge_resolve_layout_key: None,
            skybox_edge_resolve_layout_key: None,
            final_blend_layout_key: None,
            desired_keys: HashSet::new(),
            in_flight_keys: HashSet::new(),
        }
    }

    /// Replace the set of edge compute-pipeline cache keys the current
    /// bucket layout wants. Called whenever the full edge set is (re)built
    /// for a layout. A resolved scheduler edge compile installs iff its key
    /// is in this set (see [`Self::is_edge_key_desired`]).
    pub(crate) fn set_desired_edge_keys(
        &mut self,
        keys: impl IntoIterator<Item = ComputePipelineCacheKey>,
    ) {
        self.desired_keys = keys.into_iter().collect();
    }

    /// True if `key` is one the current layout still wants — i.e. a
    /// resolved edge compile with this key is safe to install (not built
    /// against a superseded layout).
    pub(crate) fn is_edge_key_desired(&self, key: &ComputePipelineCacheKey) -> bool {
        self.desired_keys.contains(key)
    }

    /// True if a scheduler compile promise for `key` is already in flight.
    pub(crate) fn edge_key_in_flight(&self, key: &ComputePipelineCacheKey) -> bool {
        self.in_flight_keys.contains(key)
    }

    /// Mark `key` as having an in-flight scheduler compile promise.
    pub(crate) fn mark_edge_key_in_flight(&mut self, key: ComputePipelineCacheKey) {
        self.in_flight_keys.insert(key);
    }

    /// Clear `key`'s in-flight marker (its promise resolved — installed or
    /// dropped).
    pub(crate) fn clear_edge_key_in_flight(&mut self, key: &ComputePipelineCacheKey) {
        self.in_flight_keys.remove(key);
    }

    /// Returns the per-shader-id edge_resolve pipeline for the given
    /// (shader_id, mipmap) config. `None` means the pipeline isn't yet
    /// compiled; the dispatch site uses
    /// `pipeline_scheduler::warn_pipeline_not_compiled` and skips that
    /// shader_id's edge contribution for the frame.
    pub fn get_per_shader_pipeline_key(
        &self,
        anti_aliasing: &AntiAliasing,
        shader_id: MaterialShaderId,
    ) -> Option<ComputePipelineKey> {
        // Edge resolve only runs under MSAA — non-MSAA returns None so
        // the dispatch site naturally short-circuits.
        anti_aliasing.msaa_sample_count?;
        self.per_shader
            .get(&EdgeResolvePipelineKeyId {
                mipmaps: anti_aliasing.mipmap,
                shader_id,
            })
            .copied()
    }

    /// Inserts a compiled per-shader-id pipeline. Called from the
    /// scheduler resolution path.
    /// Returns the DISPLACED pool key when this insert overwrote a different
    /// existing per-shader entry, so the caller can free the orphaned pipeline
    /// from the shared pool (the leak fix — re-installs under a new bucket layout
    /// used to silently orphan the previous one). `None` when the slot was empty
    /// or re-installed the identical key. See docs/plans/mesh-pipeline-overhaul.md.
    pub fn insert_per_shader_pipeline(
        &mut self,
        key_id: EdgeResolvePipelineKeyId,
        pipeline_key: ComputePipelineKey,
    ) -> Option<ComputePipelineKey> {
        self.per_shader
            .insert(key_id, pipeline_key)
            .filter(|displaced| *displaced != pipeline_key)
    }

    /// Clear every per-shader-id edge_resolve pipeline entry, plus
    /// the global skybox + final_blend keys. Used by
    /// `AwsmRenderer::register_material` to invalidate stale edge
    /// chain entries before relaunching with the new bucket layout —
    /// see `MaterialOpaquePipelines::clear_dynamic_pipelines` for
    /// the full rationale. The dispatch site's `Option` guards in
    /// `get_per_shader_pipeline_key` / `render_edge_resolve` skip
    /// the affected work until the new compiles land.
    /// Returns the dropped pool keys (per-shader + skybox + final-blend) so the
    /// caller can free them from the shared compute-pipeline pool — the leak fix
    /// (these references were dropped while the GPU pipelines lingered in the pool
    /// forever). See docs/plans/mesh-pipeline-overhaul.md.
    pub fn clear_dynamic_pipelines(&mut self) -> Vec<ComputePipelineKey> {
        let mut dropped: Vec<ComputePipelineKey> =
            self.per_shader.drain().map(|(_, k)| k).collect();
        dropped.extend(self.skybox_edge_resolve_pipeline_key.take());
        dropped.extend(self.final_blend_pipeline_key.take());
        dropped
    }

    /// Build the descriptor list for the current bucket entries +
    /// AA config + color format. Sync — caller drives the actual
    /// shader/pipeline compile (either async via
    /// [`Self::ensure_compiled`] or one-promise-at-a-time via the
    /// scheduler launch path in `pipeline_scheduler::launch`).
    ///
    /// Also commits the per-pipeline-layout keys onto `self` (cheap
    /// hash registrations, no Dawn work) so subsequent
    /// `get_per_shader_pipeline_key` / dispatch-site lookups can
    /// observe them as the live layouts.
    ///
    /// Returns `None` when MSAA is off — no edges to resolve.
    #[allow(clippy::too_many_arguments)]
    pub fn build_descriptors(
        &mut self,
        gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
        pipeline_layouts: &mut crate::pipeline_layouts::PipelineLayouts,
        bind_group_layouts: &mut crate::bind_group_layout::BindGroupLayouts,
        opaque_bind_groups: &MaterialOpaqueBindGroups,
        edge_layouts: &MaterialEdgeBindGroupLayouts,
        bucket_entries: &[BucketEntry],
        anti_aliasing: &AntiAliasing,
        color_wgsl_format: &str,
        dynamic_registry: Option<&DynamicMaterials>,
    ) -> Result<Option<MaterialEdgePipelineDescriptors>> {
        // No MSAA → no edges → no compile.
        if anti_aliasing.msaa_sample_count.is_none() {
            return Ok(None);
        }

        let texture_pool_arrays_len = opaque_bind_groups.texture_pool_arrays_len;
        let texture_pool_samplers_len = opaque_bind_groups.texture_pool_sampler_keys.len() as u32;
        let mipmaps = anti_aliasing.mipmap;

        // Build per-shader-id edge-resolve pipeline layout (reused
        // across every shader_id since their bind-group shape is
        // identical). 4 groups total: main(0) / lights(1) /
        // texture-pool(2) / extended-shadows(3). The extended-shadows
        // layout is the primary opaque shadow layout with the edge
        // buffer + edge-layout uniform appended at bindings 10/11 —
        // folded in so the layout fits in 4 bind groups (macOS Metal
        // caps at `maxBindGroups = 4`).
        let main_bgl = opaque_bind_groups.multisampled_main_bind_group_layout_key;
        let edge_resolve_layout_key = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![
                main_bgl,
                opaque_bind_groups.lights_bind_group_layout_key,
                opaque_bind_groups.texture_pool_textures_bind_group_layout_key,
                edge_layouts.edge_resolve_extended_shadows_layout_key,
            ]),
        )?;
        let skybox_edge_layout_key = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![edge_layouts.skybox_edge_group0_layout_key]),
        )?;
        let final_blend_layout_key = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![edge_layouts.final_blend_group0_layout_key]),
        )?;

        self.edge_resolve_layout_key = Some(edge_resolve_layout_key);
        self.skybox_edge_resolve_layout_key = Some(skybox_edge_layout_key);
        self.final_blend_layout_key = Some(final_blend_layout_key);

        // Per-shader-id edge_resolve shader keys + slots.
        let mut shader_cache_keys: Vec<ShaderCacheKey> = Vec::new();
        let mut slots: Vec<EdgePipelineSlot> = Vec::new();
        let mut pipeline_layout_keys: Vec<PipelineLayoutKey> = Vec::new();

        for (bucket_index, entry) in bucket_entries.iter().enumerate() {
            // A dynamic-range shader_id is one of TWO things:
            //   1. A genuine author registration (`Custom` base) — needs
            //      its `DynamicShaderInfo` triple (struct_decl /
            //      loader_decl / wgsl_fragment) templated into the
            //      edge_resolve shader.
            //   2. A first-party feature-set VARIANT (e.g. a specialized
            //      PBR bucket) — has a dynamic-range id but NO custom
            //      registration. It compiles the built-in PBR/Toon body
            //      (`dynamic_shader = None`, `dispatch_hash = 0`), exactly
            //      like a canonical bucket. Skipping it here
            //      (the old `registry.get(...).else { continue }`) left the
            //      variant's per-shader edge pipeline unbuilt → dead MSAA
            //      for every mesh using a specialized first-party material.
            // Only a dynamic id that is NEITHER (removed between submit and
            // build) is skipped.
            let (dispatch_hash, dynamic_shader) = if entry.shader_id.is_dynamic() {
                let Some(registry) = dynamic_registry else {
                    continue;
                };
                match registry.get(entry.shader_id) {
                    // A Blend/Mask dynamic material is transparent-only — it has
                    // no opaque silhouette to resolve, and its author body
                    // targets the transparent contract (returns
                    // `TransparentShadingOutput`, which won't compile in the
                    // edge-resolve opaque wrapper). Skip it.
                    Some(reg)
                        if !matches!(reg.alpha_mode, awsm_materials::MaterialAlphaMode::Opaque) =>
                    {
                        continue
                    }
                    Some(reg) => {
                        let info = DynamicShaderInfo {
                            shader_includes: reg.shader_includes.resolve(),
                            struct_decl: awsm_materials::dynamic_layout::generate_wgsl_struct(
                                "MaterialData",
                                &reg.layout,
                            ),
                            loader_decl: awsm_materials::dynamic_layout::generate_wgsl_loader(
                                "MaterialData",
                                "material_data_load",
                                &reg.layout,
                            ),
                            wgsl_fragment: reg.wgsl_fragment.clone(),
                        };
                        (registry.dispatch_hash_cached(), Some(info))
                    }
                    None if registry.first_party_variant_of(entry.shader_id).is_some() => (0, None),
                    None => continue,
                }
            } else {
                (0, None)
            };
            let key = ShaderCacheKeyMaterialEdgeResolve {
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                mipmaps,
                shader_id: entry.shader_id,
                base: entry.base,
                dispatch_hash,
                dynamic_shader,
                bucket_entries: bucket_entries.to_vec(),
                bucket_index: bucket_index as u32,
            };
            shader_cache_keys.push(ShaderCacheKey::from(key));
            slots.push(EdgePipelineSlot::PerShader(EdgeResolvePipelineKeyId {
                mipmaps,
                shader_id: entry.shader_id,
            }));
            pipeline_layout_keys.push(edge_resolve_layout_key);
        }

        // Global skybox-edge shader.
        shader_cache_keys.push(ShaderCacheKey::from(
            ShaderCacheKeyMaterialSkyboxEdgeResolve {
                bucket_entries: bucket_entries.to_vec(),
            },
        ));
        slots.push(EdgePipelineSlot::Skybox);
        pipeline_layout_keys.push(skybox_edge_layout_key);

        // Global final-blend shader.
        shader_cache_keys.push(ShaderCacheKey::from(ShaderCacheKeyMaterialFinalBlend {
            bucket_entries: bucket_entries.to_vec(),
            color_format: color_wgsl_format.to_string(),
        }));
        slots.push(EdgePipelineSlot::FinalBlend);
        pipeline_layout_keys.push(final_blend_layout_key);

        Ok(Some(MaterialEdgePipelineDescriptors {
            shader_cache_keys,
            slots,
            pipeline_layout_keys,
        }))
    }

    /// Compiles the edge-resolve pipelines for the given bucket list,
    /// anti-aliasing config, color format, and texture pool shape.
    ///
    /// Walks the bucket entries to build per-shader-id edge-resolve
    /// shader/pipeline cache keys, plus the global skybox-edge and
    /// final-blend keys; runs them through `Shaders::ensure_keys` +
    /// `ComputePipelines::ensure_keys`; folds the resolved keys back
    /// into the typed cache via `merge_resolved`.
    ///
    /// No-op when MSAA is off (there are no edges to resolve).
    ///
    /// The async wrapper is retained for the cold-boot eager path
    /// (`AwsmRendererBuilder::build`) and for `prewarm_pipelines` —
    /// the per-material register path uses
    /// `pipeline_scheduler::launch::launch_edge_resolve_compile`
    /// which pushes the same descriptors through the scheduler's
    /// inflight_compile promise queue instead.
    #[allow(clippy::too_many_arguments)]
    pub async fn ensure_compiled(
        &mut self,
        gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
        shaders: &mut crate::shaders::Shaders,
        compute_pipelines: &mut crate::pipelines::compute_pipeline::ComputePipelines,
        pipeline_layouts: &mut crate::pipeline_layouts::PipelineLayouts,
        bind_group_layouts: &mut crate::bind_group_layout::BindGroupLayouts,
        opaque_bind_groups: &MaterialOpaqueBindGroups,
        edge_layouts: &MaterialEdgeBindGroupLayouts,
        bucket_entries: &[BucketEntry],
        anti_aliasing: &AntiAliasing,
        color_wgsl_format: &str,
        dynamic_registry: Option<&DynamicMaterials>,
    ) -> Result<()> {
        let Some(descs) = self.build_descriptors(
            gpu,
            pipeline_layouts,
            bind_group_layouts,
            opaque_bind_groups,
            edge_layouts,
            bucket_entries,
            anti_aliasing,
            color_wgsl_format,
            dynamic_registry,
        )?
        else {
            return Ok(());
        };
        tracing::info!(
            target: "awsm_renderer::boot_timing",
            "MaterialEdgePipelines::ensure_compiled: compiling {} buckets + skybox + final_blend",
            bucket_entries.len()
        );

        // Compile shaders + pipelines.
        let shader_keys = shaders
            .ensure_keys(gpu, descs.shader_cache_keys.iter().cloned())
            .await?;
        let pipeline_cache_keys: Vec<ComputePipelineCacheKey> = shader_keys
            .iter()
            .zip(descs.pipeline_layout_keys.iter())
            .map(|(sk, lk)| ComputePipelineCacheKey::new(*sk, *lk))
            .collect();
        // Record this layout's edge key set as the authoritative "desired"
        // set, so any still-in-flight scheduler edge compile built against a
        // PRIOR layout is dropped on resolve (its key won't be in this set).
        self.set_desired_edge_keys(pipeline_cache_keys.iter().cloned());
        let pipeline_keys = compute_pipelines
            .ensure_keys(gpu, shaders, pipeline_layouts, pipeline_cache_keys)
            .await?;

        self.merge_resolved(descs.slots, pipeline_keys);
        Ok(())
    }

    /// Folds a flat resolved-keys vec back into the typed cache via
    /// the per-slot identity. Mirrors `MaterialOpaquePipelines::merge_resolved`.
    pub fn merge_resolved(
        &mut self,
        slots: Vec<EdgePipelineSlot>,
        pipeline_keys: Vec<ComputePipelineKey>,
    ) {
        for (slot, key) in slots.into_iter().zip(pipeline_keys) {
            match slot {
                EdgePipelineSlot::PerShader(id) => {
                    self.per_shader.insert(id, key);
                }
                EdgePipelineSlot::Skybox => {
                    self.skybox_edge_resolve_pipeline_key = Some(key);
                }
                EdgePipelineSlot::FinalBlend => {
                    self.final_blend_pipeline_key = Some(key);
                }
            }
        }
    }
}
