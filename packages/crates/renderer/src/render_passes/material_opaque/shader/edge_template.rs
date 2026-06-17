//! Askama template for the global final_blend shader (the MSAA
//! edge-resolve flow's compositor).

use askama::Template;

use crate::{
    dynamic_materials::BucketEntry,
    render_passes::material_opaque::shader::edge_cache_key::ShaderCacheKeyMaterialFinalBlend,
    shaders::{AwsmShaderError, Result},
};

// ─────────────────────────────────────────────────────────────────────
// Final blend template.
// ─────────────────────────────────────────────────────────────────────

pub struct ShaderTemplateMaterialFinalBlend {
    pub bind_groups: ShaderTemplateMaterialFinalBlendBindGroups,
    pub compute: ShaderTemplateMaterialFinalBlendCompute,
}

#[derive(Template, Debug)]
#[template(
    path = "material_opaque_wgsl/final_blend_bind_groups.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateMaterialFinalBlendBindGroups {
    pub bucket_entries: Vec<BucketEntry>,
    pub color_format: String,
}

#[derive(Template, Debug)]
#[template(
    path = "material_opaque_wgsl/final_blend.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateMaterialFinalBlendCompute {
    /// §5 edge slot-map width (8/16); gates the slot_map read + the widened
    /// empty sentinel. Derived from the live bucket count.
    pub edge_slot_bits: u32,
}

impl TryFrom<&ShaderCacheKeyMaterialFinalBlend> for ShaderTemplateMaterialFinalBlend {
    type Error = AwsmShaderError;
    fn try_from(value: &ShaderCacheKeyMaterialFinalBlend) -> Result<Self> {
        let edge_slot_bits =
            crate::dynamic_materials::edge_slot_bits(value.bucket_entries.len() as u32) as u32;
        Ok(Self {
            bind_groups: ShaderTemplateMaterialFinalBlendBindGroups {
                bucket_entries: value.bucket_entries.clone(),
                color_format: value.color_format.clone(),
            },
            compute: ShaderTemplateMaterialFinalBlendCompute { edge_slot_bits },
        })
    }
}

impl ShaderTemplateMaterialFinalBlend {
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = self.bind_groups.render()?;
        let compute_source = self.compute.render()?;
        Ok(format!("{}\n{}", bind_groups_source, compute_source))
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("Material Final Blend")
    }
}

// Shader-module completeness (every `<base>_get_material(` call has a matching
// definition) is now guarded centrally for ALL material-bearing templates —
// opaque-compute and transparent — in `crate::shader_completeness`.
