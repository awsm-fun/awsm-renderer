//! Shader template for the material classify compute pass.
//!
//! Renders a single compute shader that reads the visibility buffer
//! per tile, determines which opaque `MaterialShaderId`(s) it
//! contains, and atomically appends the tile coords to each
//! shader_id's bucket — see [`super::cache_key`] for the cache key,
//! and [`crate::render_passes::material_classify::buffers`] for the
//! storage-buffer layout the shader writes into.

use askama::Template;

use crate::{
    render_passes::material_classify::{
        buffers::header_bytes, shader::cache_key::ShaderCacheKeyMaterialClassify,
    },
    shaders::{AwsmShaderError, Result},
};

/// Classify pass shader template — bind groups + compute in one
/// askama-rendered string (concatenated by [`Self::into_source`]). Mirrors
/// the layout of the other render-pass templates.
pub struct ShaderTemplateMaterialClassify {
    pub bind_groups: ShaderTemplateMaterialClassifyBindGroups,
    pub compute: ShaderTemplateMaterialClassifyCompute,
}

/// Bind group declarations for the classify compute shader. Layout
/// must stay in lockstep with
/// [`super::super::bind_group::MaterialClassifyBindGroups`].
#[derive(Template, Debug)]
#[template(
    path = "material_classify_wgsl/bind_groups.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateMaterialClassifyBindGroups {
    /// MSAA sample count of the visibility texture (0 = singlesampled).
    pub multisampled_geometry: bool,
    /// Live bucket count — the fixed size of the `ClassifyOutput`
    /// `args` / `offsets` arrays + `EdgeArgsBuffer.per_shader_edge_args`
    /// (§4b/§4c). The struct is now count-driven, not identity-driven, so
    /// no per-bucket `bucket_entries` walk is needed.
    pub bucket_count: u32,
    /// Trailing alignment-pad u32 indices. Length is the number of
    /// `_pad_align_<i>: u32` declarations the template emits — either
    /// 0, 1, 2, or 3 depending on `bucket_count` (see
    /// [`pad_words_count`]).
    pub pad_words_iter: Vec<u32>,
    /// When `true`, emit the EdgeBuffers + EdgeBufferLayout bind-group
    /// declarations (group(0) bindings 4 and 5). Priority 3 in
    /// https://github.com/dakom/awsm-renderer/pull/99.
    pub emit_edge_data: bool,
}

/// Compute shader body for the classify pass.
#[derive(Template, Debug)]
#[template(path = "material_classify_wgsl/compute.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateMaterialClassifyCompute {
    pub multisampled_geometry: bool,
    /// When `true`, emit the Priority-3 edge-data emission block
    /// (per-pixel 4-sample shader_id scan + edge_pixel_id allocation +
    /// edge_to_xy / edge_slot_map / per-shader sample list writes).
    pub emit_edge_data: bool,
    /// Number of `atomic<u32>` words in the workgroup `tile_mask`
    /// array (= [`crate::dynamic_materials::classify_mask_words`] of the
    /// *live* bucket count). Sized to the live count, not the configured
    /// cap, so a `<=32`-bucket scene gets `1` word — generating WGSL
    /// equivalent to the original single-`atomic<u32>` form — and the
    /// width grows only as the live count crosses a 32-bucket boundary.
    /// A workgroup-array size must be a WGSL compile-time constant; the
    /// live count is part of the classify cache key (via `bucket_entries`).
    pub n_words: u32,
    /// `0..n_words` — iterated by the template to zero + load each
    /// mask word.
    pub words_iter: Vec<u32>,
    /// Edge `edge_slot_map` packing width (8 or 16), from
    /// [`crate::dynamic_materials::edge_slot_bits`] of the live bucket count
    /// (§5). `8` packs the 4 per-sample bucket ids into one u32/edge
    /// (≤254 buckets, byte-identical to before); `16` packs two u32/edge
    /// (>254). Derived from `bucket_count` (in the cache key via
    /// `bucket_entries`), so writer + readers + buffer sizing all agree.
    pub edge_slot_bits: u32,
    /// Wide (32-byte) vs narrow (16-byte) accumulator slots — the clear's
    /// stride must match the allocation (see the cache key's field doc).
    pub wide_edge_slots: bool,
}

/// Returns the number of trailing `u32` padding words the templated
/// `ClassifyOutput` struct emits after `bucket_capacity` so the
/// runtime `array<vec2<u32>>` lands on a 16-byte boundary. Matches
/// [`crate::render_passes::material_classify::buffers::header_bytes`]
/// — the WGSL emit and the host-side header writer share this.
pub fn pad_words_count(bucket_count: u32) -> u32 {
    let unpadded = bucket_count * 16 + bucket_count * 4 + 4; // args + offsets + capacity
    let padded = header_bytes(bucket_count);
    (padded - unpadded) / 4
}

/// Builds the `const SHADER_ID_<NAME>: u32 = N;` lines from a bucket
/// list. The classify + opaque-substitution templates share this so
/// the consts are always in lockstep with the registry.
impl TryFrom<&ShaderCacheKeyMaterialClassify> for ShaderTemplateMaterialClassify {
    type Error = AwsmShaderError;
    fn try_from(key: &ShaderCacheKeyMaterialClassify) -> Result<Self> {
        let multisampled_geometry = key.msaa_sample_count.is_some();
        let bucket_count = key.bucket_count();
        let pad_words_iter = (0..pad_words_count(bucket_count)).collect();
        // Every templated width is a pure function of the live bucket count
        // (§0/§3): `mask_words` sizes the workgroup `tile_mask`, `edge_slot_bits`
        // picks the 8/16-bit edge packing. The classify shader is
        // identity-independent (§4a-§4d) — `bucket_count` in the cache key is
        // the only thing its text depends on, so same-count registries share
        // the compiled shader.
        let mask_words = crate::dynamic_materials::classify_mask_words(bucket_count);
        Ok(ShaderTemplateMaterialClassify {
            bind_groups: ShaderTemplateMaterialClassifyBindGroups {
                multisampled_geometry,
                bucket_count,
                pad_words_iter,
                emit_edge_data: key.emit_edge_data,
            },
            compute: ShaderTemplateMaterialClassifyCompute {
                multisampled_geometry,
                emit_edge_data: key.emit_edge_data,
                n_words: mask_words,
                words_iter: (0..mask_words).collect(),
                edge_slot_bits: crate::dynamic_materials::edge_slot_bits(bucket_count) as u32,
                wide_edge_slots: key.wide_edge_slots,
            },
        })
    }
}

impl ShaderTemplateMaterialClassify {
    /// Renders the classify shader into a WGSL source string.
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = self.bind_groups.render()?;
        let compute_source = self.compute.render()?;
        Ok(format!("{}\n{}", bind_groups_source, compute_source))
    }

    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Material Classify")
    }
}
