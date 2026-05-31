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
    dynamic_materials::BucketEntry,
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
    /// The full bucket list (first-party + dynamic) the templated
    /// `ClassifyOutput` struct emits `args_<name>` / `<name>_offset`
    /// fields for.
    pub bucket_entries: Vec<BucketEntry>,
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
    /// Generated `const SHADER_ID_X: u32 = N;` lines — same source
    /// as the opaque pass uses (built from `bucket_entries`).
    pub shader_id_consts: String,
    /// The full bucket list (first-party + dynamic). The template
    /// walks this to emit:
    /// - `const BUCKET_BIT_<NAME>: u32 = (1u << index);` per entry
    /// - the `shader_id == SHADER_ID_<NAME>` if/else chain
    /// - one per-bucket extract block per entry
    pub bucket_entries: Vec<BucketEntry>,
    /// When `true`, emit the Priority-3 edge-data emission block
    /// (per-pixel 4-sample shader_id scan + edge_pixel_id allocation +
    /// edge_to_xy / edge_slot_map / per-shader sample list writes).
    pub emit_edge_data: bool,
    /// Number of `atomic<u32>` words in the workgroup `tile_mask`
    /// array (= [`crate::dynamic_materials::MAX_BUCKET_WORDS`]). The
    /// mask is sized to the *max* bucket budget, not the live bucket
    /// count, so it's a compile-time constant. At `1` the generated
    /// WGSL is equivalent to the original single-`atomic<u32>` form.
    pub n_words: u32,
    /// `0..n_words` — iterated by the template to zero + load each
    /// mask word.
    pub words_iter: Vec<u32>,
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
pub fn build_shader_id_consts_from_entries(entries: &[BucketEntry]) -> String {
    let mut out = String::new();
    for entry in entries {
        out.push_str(&format!(
            "const SHADER_ID_{}: u32 = {}u;\n",
            entry.name.to_uppercase(),
            entry.shader_id.as_u32(),
        ));
    }
    out
}

impl TryFrom<&ShaderCacheKeyMaterialClassify> for ShaderTemplateMaterialClassify {
    type Error = AwsmShaderError;
    fn try_from(key: &ShaderCacheKeyMaterialClassify) -> Result<Self> {
        let multisampled_geometry = key.msaa_sample_count.is_some();
        let bucket_entries = key.bucket_entries.clone();
        let pad_words_iter = (0..pad_words_count(key.bucket_count())).collect();
        let shader_id_consts = build_shader_id_consts_from_entries(&bucket_entries);
        Ok(ShaderTemplateMaterialClassify {
            bind_groups: ShaderTemplateMaterialClassifyBindGroups {
                multisampled_geometry,
                bucket_entries: bucket_entries.clone(),
                pad_words_iter,
                emit_edge_data: key.emit_edge_data,
            },
            compute: ShaderTemplateMaterialClassifyCompute {
                multisampled_geometry,
                shader_id_consts,
                bucket_entries,
                emit_edge_data: key.emit_edge_data,
                n_words: crate::dynamic_materials::MAX_BUCKET_WORDS,
                words_iter: (0..crate::dynamic_materials::MAX_BUCKET_WORDS).collect(),
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
