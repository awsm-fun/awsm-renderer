//! Pipeline cache + descriptors for the per-shader-id MSAA edge-resolve
//! pipelines (Priority 3 in docs/plans/more-optimizations.md).
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
//! See [§ Pipeline count and packaging](../../../../docs/plans/more-optimizations.md#pipeline-count-and-packaging)
//! for the cost model.

use std::collections::HashMap;

use awsm_materials::MaterialShaderId;

use crate::anti_alias::AntiAliasing;
use crate::dynamic_materials::BucketEntry;
use crate::error::Result;
use crate::pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey};
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::material_opaque::bind_group::MaterialOpaqueBindGroups;
use crate::render_passes::material_opaque::edge_bind_group::MaterialEdgeBindGroupLayouts;
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

/// Pre-resolved descriptors for the edge-resolve pipelines. Threaded
/// through the same `Shaders::ensure_keys` → pipeline-`ensure_keys` →
/// `from_resolved` flow as the other render-pass pipeline pools.
pub struct MaterialEdgePipelineDescriptors {
    pub shader_cache_keys: Vec<crate::shaders::ShaderCacheKey>,
    pub pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
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
        }
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
    pub fn insert_per_shader_pipeline(
        &mut self,
        key_id: EdgeResolvePipelineKeyId,
        pipeline_key: ComputePipelineKey,
    ) {
        self.per_shader.insert(key_id, pipeline_key);
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
    ) -> Result<()> {
        // No MSAA → no edges → no compile.
        if anti_aliasing.msaa_sample_count.is_none() {
            return Ok(());
        }
        tracing::info!(
            target: "awsm_renderer::boot_timing",
            "MaterialEdgePipelines::ensure_compiled: compiling {} buckets + skybox + final_blend",
            bucket_entries.len()
        );

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
            // Skip dynamic shader_ids for now — they need DynamicShaderInfo,
            // which lives on the dynamic registration. First-party only at
            // this commit; dynamic wiring lands when the dynamic-material
            // scheduler integration does (Stage 1.14).
            if entry.shader_id.is_dynamic() {
                continue;
            }
            let key = ShaderCacheKeyMaterialEdgeResolve {
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                mipmaps,
                shader_id: entry.shader_id,
                dispatch_hash: 0,
                dynamic_shader: None,
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

        // Compile shaders + pipelines.
        let shader_keys = shaders
            .ensure_keys(gpu, shader_cache_keys.iter().cloned())
            .await?;
        let pipeline_cache_keys: Vec<ComputePipelineCacheKey> = shader_keys
            .iter()
            .zip(pipeline_layout_keys.iter())
            .map(|(sk, lk)| ComputePipelineCacheKey::new(*sk, *lk))
            .collect();
        let pipeline_keys = compute_pipelines
            .ensure_keys(gpu, shaders, pipeline_layouts, pipeline_cache_keys)
            .await?;

        self.merge_resolved(slots, pipeline_keys);
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
