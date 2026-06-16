//! Shader cache key for the material classify compute pass.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for the classify compute pipeline.
///
/// Fields:
/// - `msaa_sample_count` — MSAA matters because the visibility texture
///   is sampled either single- or multisampled; the classify shader
///   only reads sample 0 either way, but the declared binding type
///   has to match.
/// - `bucket_count` — the number of registered buckets (first-party +
///   currently-registered dynamic materials). Since §4a-§4d the classify
///   shader is **identity-independent**: it routes per-pixel/per-sample via
///   the `bucket_lut` storage buffer (not codegen'd per-bucket arms) and
///   sizes its `ClassifyOutput`/`tile_mask` from the count alone — so the
///   shader TEXT is a pure function of `(msaa, bucket_count, emit_edge_data)`
///   and never the bucket *identities*. Keying on the count (not the full
///   entry list) means two registries with the same count but different
///   materials share one compiled classify shader → no recompile on a
///   same-count identity change (§4d recompile reduction).
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyMaterialClassify {
    pub msaa_sample_count: Option<u32>,
    pub bucket_count: u32,
    /// When `true`, the classify shader also emits per-edge data into
    /// the [`MaterialEdgeBuffers`](crate::render_passes::material_opaque::edge_buffers::MaterialEdgeBuffers)
    /// buffer (edge_pixel_id allocation, edge_to_xy, edge_slot_map,
    /// per-shader-id sample lists). Required for the Priority 3
    /// per-shader-id edge resolve flow; orthogonal to single- vs
    /// multi-sampled visibility texture (so the single-sampled variant
    /// can still compile with this off as a zero-cost no-op).
    pub emit_edge_data: bool,
}

impl ShaderCacheKeyMaterialClassify {
    /// The live bucket count this classify shader is specialized for.
    pub fn bucket_count(&self) -> u32 {
        self.bucket_count
    }
}

impl From<ShaderCacheKeyMaterialClassify> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyMaterialClassify) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::MaterialClassify(key))
    }
}
