//! Shader template for the material prep compute pass (Plan B). Renders bind
//! groups + compute into one WGSL string. Mirrors the other render-pass templates.

use askama::Template;

use crate::{
    render_passes::material_prep::shader::cache_key::ShaderCacheKeyMaterialPrep,
    shaders::{AwsmShaderError, Result},
};

pub struct ShaderTemplateMaterialPrep {
    pub bind_groups: ShaderTemplateMaterialPrepBindGroups,
    pub compute: ShaderTemplateMaterialPrepCompute,
}

/// Bind group declarations — must stay in lockstep with
/// `material_prep/bind_group.rs` (added in the buffer-wiring sub-stage).
#[derive(Template, Debug)]
#[template(path = "material_prep_wgsl/bind_groups.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateMaterialPrepBindGroups {
    /// Visibility texture sample count (true = multisampled binding type).
    pub multisampled_geometry: bool,
}

/// Compute body (`cs_prep`).
#[derive(Template, Debug)]
#[template(path = "material_prep_wgsl/compute.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateMaterialPrepCompute {
    pub multisampled_geometry: bool,
    /// UV / color set layer caps — kept in lockstep with the array-texture
    /// allocation (`render_textures.rs`) so the write loop never exceeds the
    /// texture's layer count.
    pub max_prep_uv_sets: u32,
    pub max_prep_color_sets: u32,
}

impl TryFrom<&ShaderCacheKeyMaterialPrep> for ShaderTemplateMaterialPrep {
    type Error = AwsmShaderError;
    fn try_from(key: &ShaderCacheKeyMaterialPrep) -> Result<Self> {
        let multisampled_geometry = key.msaa_sample_count.is_some();
        Ok(ShaderTemplateMaterialPrep {
            bind_groups: ShaderTemplateMaterialPrepBindGroups { multisampled_geometry },
            compute: ShaderTemplateMaterialPrepCompute {
                multisampled_geometry,
                max_prep_uv_sets: crate::render_passes::material_prep::MAX_PREP_UV_SETS,
                max_prep_color_sets: crate::render_passes::material_prep::MAX_PREP_COLOR_SETS,
            },
        })
    }
}

impl ShaderTemplateMaterialPrep {
    /// Renders the prep shader into a WGSL source string.
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = self.bind_groups.render()?;
        let compute_source = self.compute.render()?;
        Ok(format!("{}\n{}", bind_groups_source, compute_source))
    }

    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Material Prep")
    }
}
