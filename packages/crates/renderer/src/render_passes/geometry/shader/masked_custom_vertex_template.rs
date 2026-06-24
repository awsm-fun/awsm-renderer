//! Shader template for the COMBINED **masked + custom-vertex** geometry raster
//! variant — a material that is BOTH glTF `MASK` (alpha-tested cutout) AND
//! carries a `wgsl_vertex` displacement body.
//!
//! It is the UNION of the masked + custom-vertex variants:
//!   * the **masked bind groups** (augmented group 0 + reused groups 1-3 — the
//!     `materials` storage buffer + texture pool the hooks read),
//!   * the geometry **vertex** built with `has_custom_vertex` (compiles the
//!     `custom_displace_vertex` hook — so the cutout silhouette is DISPLACED),
//!   * the **masked fragment** (the alpha-test `discard` / MSAA coverage — so the
//!     displaced surface is also alpha-tested).
//!
//! The shared material-load helpers (`material_load_*` + `texture_pool_sample`)
//! live in `shared_wgsl/material_load_helpers.wgsl`, included EXACTLY ONCE by the
//! masked fragment's `masked_alpha.wgsl`; the masked bind groups reference (but do
//! not declare) them, so the vertex hook's generated `material_data_load` /
//! `material_sample_<name>` resolve against that single copy — no redefinition.
//!
//! For a **Custom** (dynamic) material the `MaterialData` struct + `material_data_load`
//! loader are emitted by the VERTEX hook; the fragment alpha path reuses them, so
//! the fragment's struct/loader fields are passed EMPTY to avoid a second
//! definition (the per-texture `material_sample_<name>` helpers + the author's
//! alpha body still come from the fragment). Built-in (PBR/Unlit/Toon/Flipbook)
//! masked materials carry no fragment struct/loader anyway.

use askama::Template;

use crate::{
    dynamic_materials::ShadingBase,
    render_passes::geometry::shader::{
        cache_key::ShaderCacheKeyGeometry,
        masked_custom_vertex_cache_key::ShaderCacheKeyGeometryMaskedCustomVertex,
        masked_template::{
            ShaderTemplateGeometryMaskedBindGroups, ShaderTemplateGeometryMaskedFragment,
        },
        template::ShaderTemplateGeometryVertex,
    },
    shaders::{AwsmShaderError, Result},
};

/// Combined masked + custom-vertex geometry shader template components.
#[derive(Debug)]
pub struct ShaderTemplateGeometryMaskedCustomVertex {
    pub bind_groups: ShaderTemplateGeometryMaskedBindGroups,
    pub vertex: ShaderTemplateGeometryVertex,
    pub fragment: ShaderTemplateGeometryMaskedFragment,
}

impl TryFrom<&ShaderCacheKeyGeometryMaskedCustomVertex>
    for ShaderTemplateGeometryMaskedCustomVertex
{
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyGeometryMaskedCustomVertex) -> Result<Self> {
        // VERTEX: the geometry vertex compiled WITH the custom-vertex hook. It
        // emits the dynamic `MaterialData` struct + loader the hook reads.
        let vertex_key = ShaderCacheKeyGeometry {
            instancing_transforms: false,
            meta_storage_array: false,
            msaa_samples: value.msaa_samples,
            dynamic_vertex_shader: Some(value.dynamic_vertex.clone()),
        };

        // FRAGMENT (alpha test): for a Custom base the struct/loader come from the
        // vertex (above) — pass them EMPTY here so they aren't redefined. The
        // texture helpers + author alpha body are still the fragment's. Built-in
        // bases carry none of these.
        let (struct_decl, loader_decl, texture_helpers, alpha_wgsl) = match &value.dynamic_alpha {
            Some(info) => (
                // Custom struct/loader emitted by the vertex hook — suppress here.
                String::new(),
                String::new(),
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
                    awsm_renderer_materials::flipbook::FLIPBOOK_CELL_WGSL.to_string()
                } else {
                    String::new()
                },
            ),
        })
    }
}

impl ShaderTemplateGeometryMaskedCustomVertex {
    /// Renders the combined masked + custom-vertex geometry shader into WGSL.
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
        Some("Geometry Masked Custom Vertex")
    }
}
