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
        cache_key::ShaderCacheKeyGeometry, masked_cache_key::ShaderCacheKeyGeometryMasked,
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

impl ShaderTemplateGeometryMaskedBindGroups {
    /// Builds the masked bind-group template for the given texture-pool lengths.
    /// Shared with the combined masked + custom-vertex template, which reuses the
    /// identical (augmented group-0) layout verbatim.
    pub fn new(texture_pool_arrays_len: u32, texture_pool_samplers_len: u32) -> Self {
        Self {
            texture_pool_arrays_len,
            texture_pool_samplers_len,
        }
    }
}

/// Fragment template for the masked geometry variant.
#[derive(Template, Debug)]
#[template(path = "masked_wgsl/fragment.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateGeometryMaskedFragment {
    texture_pool_arrays_len: u32,
    texture_pool_samplers_len: u32,
    /// MSAA sample count (0 = single-sampled). When > 1 the fragment emits a
    /// `sample_mask` of analytic cutout coverage so the MSAA edge-resolve
    /// anti-aliases the cutout boundary; when 0 it uses a binary `discard`.
    msaa_sample_count: u32,
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

impl ShaderTemplateGeometryMaskedFragment {
    /// Builds the masked fragment template. Shared with the combined masked +
    /// custom-vertex template (which suppresses the Custom struct/loader so the
    /// vertex hook's single copy is reused).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        texture_pool_arrays_len: u32,
        texture_pool_samplers_len: u32,
        msaa_sample_count: u32,
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
            msaa_sample_count,
            base,
            dynamic_struct_decl,
            dynamic_loader_decl,
            dynamic_texture_helpers,
            dynamic_alpha_wgsl,
            flipbook_cell_wgsl,
        }
    }
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
            // Masked geometry reuses the plain vertex; custom-vertex masked is a
            // CV2 follow-on, so the vertex stays non-custom here.
            dynamic_vertex_shader: None,
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
            bind_groups: ShaderTemplateGeometryMaskedBindGroups::new(
                value.texture_pool_arrays_len,
                value.texture_pool_samplers_len,
            ),
            vertex: ShaderTemplateGeometryVertex::new(&vertex_key),
            fragment: ShaderTemplateGeometryMaskedFragment::new(
                value.texture_pool_arrays_len,
                value.texture_pool_samplers_len,
                value.msaa_samples.unwrap_or(0),
                value.base,
                struct_decl,
                loader_decl,
                texture_helpers,
                alpha_wgsl,
                if value.base == ShadingBase::Flipbook {
                    awsm_materials::flipbook::FLIPBOOK_CELL_WGSL.to_string()
                } else {
                    String::new()
                },
            ),
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
