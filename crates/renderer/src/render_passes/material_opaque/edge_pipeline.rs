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
use crate::pipeline_layouts::PipelineLayoutKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};

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
