//! Shader template for the **masked** (alpha-tested) geometry raster variant.
//!
//! Renders: masked bind groups (augmented group 0 + reused groups 1-3) + the
//! plain geometry **vertex** shader (reused verbatim, non-instanced /
//! uniform-meta) + the masked **fragment** (cutoff `discard` + the same
//! visibility-buffer write as the plain pass).

use askama::Template;

use crate::{
    dynamic_materials::ShadingBase,
    render_passes::geometry::shader::{
        cache_key::ShaderCacheKeyGeometry,
        masked_cache_key::ShaderCacheKeyGeometryMasked,
        template::ShaderTemplateGeometryVertex,
    },
    shaders::{AwsmShaderError, Result},
};

/// Masked geometry shader template components.
#[derive(Debug)]
pub struct ShaderTemplateGeometryMasked {
    pub bind_groups: ShaderTemplateGeometryMaskedBindGroups,
    pub vertex: ShaderTemplateGeometryVertex,
    pub fragment: ShaderTemplateGeometryMaskedFragment,
}

/// Bind-group template for the masked geometry variant.
#[derive(Template, Debug)]
#[template(path = "masked_wgsl/bind_groups.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateGeometryMaskedBindGroups {
    texture_pool_arrays_len: u32,
    texture_pool_samplers_len: u32,
}

/// Fragment template for the masked geometry variant.
#[derive(Template, Debug)]
#[template(path = "masked_wgsl/fragment.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateGeometryMaskedFragment {
    texture_pool_arrays_len: u32,
    texture_pool_samplers_len: u32,
    /// Built-in shading family (selects the base-color path) or `Custom`
    /// (emits the author's alpha-only fragment).
    base: ShadingBase,
    /// Auto-generated `MaterialData` struct (custom only; empty otherwise).
    dynamic_struct_decl: String,
    /// Auto-generated `material_data_load` accessor (custom only).
    dynamic_loader_decl: String,
    /// Auto-generated per-texture `material_sample_<name>` helpers (custom only).
    dynamic_texture_helpers: String,
    /// The author's alpha-only WGSL fragment body (custom only).
    dynamic_alpha_wgsl: String,
}

impl TryFrom<&ShaderCacheKeyGeometryMasked> for ShaderTemplateGeometryMasked {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyGeometryMasked) -> Result<Self> {
        // Reuse the plain geometry vertex shader: masked meshes always take the
        // non-instanced, uniform-meta path, so build the vertex for exactly
        // that shape.
        let vertex_key = ShaderCacheKeyGeometry {
            instancing_transforms: false,
            meta_storage_array: false,
            msaa_samples: None,
        };

        let (struct_decl, loader_decl, texture_helpers, alpha_wgsl) = match &value.dynamic_alpha {
            Some(info) => (
                info.struct_decl.clone(),
                info.loader_decl.clone(),
                info.texture_helpers.clone(),
                info.alpha_wgsl.clone(),
            ),
            None => (String::new(), String::new(), String::new(), String::new()),
        };

        Ok(Self {
            bind_groups: ShaderTemplateGeometryMaskedBindGroups {
                texture_pool_arrays_len: value.texture_pool_arrays_len,
                texture_pool_samplers_len: value.texture_pool_samplers_len,
            },
            vertex: ShaderTemplateGeometryVertex::new(&vertex_key),
            fragment: ShaderTemplateGeometryMaskedFragment {
                texture_pool_arrays_len: value.texture_pool_arrays_len,
                texture_pool_samplers_len: value.texture_pool_samplers_len,
                base: value.base,
                dynamic_struct_decl: struct_decl,
                dynamic_loader_decl: loader_decl,
                dynamic_texture_helpers: texture_helpers,
                dynamic_alpha_wgsl: alpha_wgsl,
            },
        })
    }
}

impl ShaderTemplateGeometryMasked {
    /// Renders the masked geometry shader template into WGSL.
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = self.bind_groups.render()?;
        let vertex_source = self.vertex.render()?;
        let fragment_source = self.fragment.render()?;
        Ok(format!(
            "{}\n{}\n{}",
            bind_groups_source, vertex_source, fragment_source
        ))
    }

    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Geometry Masked")
    }
}
