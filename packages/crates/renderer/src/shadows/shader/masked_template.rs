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

impl ShaderTemplateShadowMaskedBindGroups {
    /// Builds the masked-shadow bind-group template. Shared with the combined
    /// masked + custom-vertex shadow template (identical augmented group 0).
    pub fn new(
        texture_pool_arrays_len: u32,
        texture_pool_samplers_len: u32,
        instancing_transforms: bool,
    ) -> Self {
        Self {
            texture_pool_arrays_len,
            texture_pool_samplers_len,
            instancing_transforms,
        }
    }
}

/// Vertex template for the masked shadow variant.
#[derive(Template, Debug)]
#[template(path = "shadow_masked_wgsl/vertex.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateShadowMaskedVertex {
    instancing_transforms: bool,
    max_morph_unroll: u32,
    max_skin_unroll: u32,
    /// INERT scaffolding for the programmable vertex-displacement hook
    /// (mirrors the fragment `custom_shade_dynamic` machinery). Always
    /// `false` here — the gated WGSL is never rendered yet, so output is
    /// byte-identical to today.
    has_custom_vertex: bool,
    /// The author's WGSL displacement body, wrapped into
    /// `custom_displace_vertex` at render time. Empty until wired up.
    dynamic_wgsl_vertex: String,
    /// Auto-generated `struct MaterialData { ... }` decl for the hook.
    /// Empty until wired up.
    dynamic_vertex_struct_decl: String,
    /// Auto-generated `material_data_load` accessor for the hook.
    /// Empty until wired up.
    dynamic_vertex_loader_decl: String,
}

impl ShaderTemplateShadowMaskedVertex {
    /// Builds the masked-shadow vertex template. With `has_custom_vertex = true`
    /// (the combined masked + custom-vertex shadow variant) it renders the
    /// `custom_displace_vertex` hook + the struct/loader the hook reads; with
    /// `false` (plain masked shadow) the output is byte-identical to before.
    pub fn new(
        instancing_transforms: bool,
        has_custom_vertex: bool,
        dynamic_wgsl_vertex: String,
        dynamic_vertex_struct_decl: String,
        dynamic_vertex_loader_decl: String,
    ) -> Self {
        Self {
            instancing_transforms,
            max_morph_unroll: 2,
            max_skin_unroll: 2,
            has_custom_vertex,
            dynamic_wgsl_vertex,
            dynamic_vertex_struct_decl,
            dynamic_vertex_loader_decl,
        }
    }
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
    /// `awsm_renderer_materials::flipbook::FLIPBOOK_CELL_WGSL`, injected so the masked
    /// cutout evaluates the SAME cell the shaded material shows.
    flipbook_cell_wgsl: String,
}

impl ShaderTemplateShadowMaskedFragment {
    /// Builds the masked-shadow fragment template. Shared with the combined
    /// masked + custom-vertex shadow template (which suppresses the Custom
    /// struct/loader so the vertex hook's single copy is reused).
    pub fn new(
        texture_pool_arrays_len: u32,
        texture_pool_samplers_len: u32,
        base: ShadingBase,
        dynamic_struct_decl: String,
        dynamic_loader_decl: String,
        dynamic_texture_helpers: String,
        dynamic_alpha_wgsl: String,
        flipbook_cell_wgsl: String,
    ) -> Self {
        Self {
            texture_pool_arrays_len,
            texture_pool_samplers_len,
            base,
            dynamic_struct_decl,
            dynamic_loader_decl,
            dynamic_texture_helpers,
            dynamic_alpha_wgsl,
            flipbook_cell_wgsl,
        }
    }
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
            bind_groups: ShaderTemplateShadowMaskedBindGroups::new(
                value.texture_pool_arrays_len,
                value.texture_pool_samplers_len,
                value.instancing_transforms,
            ),
            vertex: ShaderTemplateShadowMaskedVertex::new(
                value.instancing_transforms,
                false,
                String::new(),
                String::new(),
                String::new(),
            ),
            fragment: ShaderTemplateShadowMaskedFragment::new(
                value.texture_pool_arrays_len,
                value.texture_pool_samplers_len,
                value.base,
                struct_decl,
                loader_decl,
                texture_helpers,
                alpha_wgsl,
                if value.base == ShadingBase::Flipbook {
                    awsm_renderer_materials::flipbook::FLIPBOOK_CELL_WGSL.to_string()
                } else {
                    String::new()
                },
            ),
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
