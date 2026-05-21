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
    render_passes::material_classify::shader::cache_key::ShaderCacheKeyMaterialClassify,
    shaders::{AwsmShaderError, Result},
};

/// Classify pass shader template — bind groups + compute in one
/// askama-rendered string (concatenated by [`into_source`]). Mirrors
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
}

/// Compute shader body for the classify pass.
#[derive(Template, Debug)]
#[template(path = "material_classify_wgsl/compute.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateMaterialClassifyCompute {
    pub multisampled_geometry: bool,
    /// Generated `const SHADER_ID_X: u32 = N;` lines — same source
    /// as the opaque pass uses, so the classify shader's
    /// `shader_id == SHADER_ID_PBR` comparisons stay in lockstep with
    /// the writer side in `awsm_materials`.
    pub shader_id_consts: String,
}

impl TryFrom<&ShaderCacheKeyMaterialClassify> for ShaderTemplateMaterialClassify {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyMaterialClassify) -> Result<Self> {
        let multisampled_geometry = value.msaa_sample_count.is_some();
        Ok(Self {
            bind_groups: ShaderTemplateMaterialClassifyBindGroups {
                multisampled_geometry,
            },
            compute: ShaderTemplateMaterialClassifyCompute {
                multisampled_geometry,
                shader_id_consts: awsm_materials::registry::build_shader_id_consts(),
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

    #[cfg(debug_assertions)]
    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Material Classify")
    }
}
