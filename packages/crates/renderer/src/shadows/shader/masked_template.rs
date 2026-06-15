//! Askama templates for the **masked** (alpha-tested) shadow-generation shader.
//!
//! Renders: masked bind groups (augmented group 0 + the geometry pass's
//! transforms/meta/animation groups) + a masked **vertex** (forwards
//! triangle_index / barycentric / material_mesh_meta_offset to the fragment) +
//! a depth-only masked **fragment** (cutoff `discard`, no color outputs).

use askama::Template;

use crate::{
    dynamic_materials::ShadingBase,
    shaders::{AwsmShaderError, Result},
    shadows::shader::masked_cache_key::ShaderCacheKeyShadowMasked,
};

/// Masked shadow shader template components.
#[derive(Debug)]
pub struct ShaderTemplateShadowMasked {
    pub bind_groups: ShaderTemplateShadowMaskedBindGroups,
    pub vertex: ShaderTemplateShadowMaskedVertex,
    pub fragment: ShaderTemplateShadowMaskedFragment,
}

/// Bind-group template for the masked shadow variant.
#[derive(Template, Debug)]
#[template(path = "shadow_masked_wgsl/bind_groups.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateShadowMaskedBindGroups {
    texture_pool_arrays_len: u32,
    texture_pool_samplers_len: u32,
    instancing_transforms: bool,
}

/// Vertex template for the masked shadow variant.
#[derive(Template, Debug)]
#[template(path = "shadow_masked_wgsl/vertex.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateShadowMaskedVertex {
    instancing_transforms: bool,
    max_morph_unroll: u32,
    max_skin_unroll: u32,
}

/// Fragment template for the masked shadow variant.
#[derive(Template, Debug)]
#[template(path = "shadow_masked_wgsl/fragment.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateShadowMaskedFragment {
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
    /// The shared sprite-sheet cell math (Flipbook only; empty otherwise) —
    /// `awsm_materials::flipbook::FLIPBOOK_CELL_WGSL`, injected so the masked
    /// cutout evaluates the SAME cell the shaded material shows.
    flipbook_cell_wgsl: String,
}

impl TryFrom<&ShaderCacheKeyShadowMasked> for ShaderTemplateShadowMasked {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyShadowMasked) -> Result<Self> {
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
            bind_groups: ShaderTemplateShadowMaskedBindGroups {
                texture_pool_arrays_len: value.texture_pool_arrays_len,
                texture_pool_samplers_len: value.texture_pool_samplers_len,
                instancing_transforms: value.instancing_transforms,
            },
            vertex: ShaderTemplateShadowMaskedVertex {
                instancing_transforms: value.instancing_transforms,
                max_morph_unroll: 2,
                max_skin_unroll: 2,
            },
            fragment: ShaderTemplateShadowMaskedFragment {
                texture_pool_arrays_len: value.texture_pool_arrays_len,
                texture_pool_samplers_len: value.texture_pool_samplers_len,
                base: value.base,
                dynamic_struct_decl: struct_decl,
                dynamic_loader_decl: loader_decl,
                dynamic_texture_helpers: texture_helpers,
                dynamic_alpha_wgsl: alpha_wgsl,
                flipbook_cell_wgsl: if value.base == ShadingBase::Flipbook {
                    awsm_materials::flipbook::FLIPBOOK_CELL_WGSL.to_string()
                } else {
                    String::new()
                },
            },
        })
    }
}

impl ShaderTemplateShadowMasked {
    /// Renders the masked shadow shader template into WGSL.
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = self.bind_groups.render()?;
        let vertex_source = self.vertex.render()?;
        let fragment_source = self.fragment.render()?;
        Ok(format!(
            "{}\n{}\n{}",
            bind_groups_source, vertex_source, fragment_source
        ))
    }

    /// Optional debug label for shader compilation diagnostics.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Shadow Masked")
    }
}
