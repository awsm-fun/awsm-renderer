//! Askama template for the COMBINED **masked + custom-vertex** shadow caster — a
//! material that is BOTH glTF `MASK` AND carries a `wgsl_vertex` displacement
//! body.
//!
//! It is the UNION of the masked + custom-vertex shadow variants, assembled from
//! the masked-shadow WGSL with `has_custom_vertex` flipped on:
//!   * the **masked-shadow bind groups** (augmented group 0 + reused groups 1-3),
//!   * the masked-shadow **vertex** built with `has_custom_vertex = true` (runs
//!     the `custom_displace_vertex` hook → DISPLACED silhouette, and forwards the
//!     cutout varyings the fragment needs),
//!   * the masked-shadow **fragment** (the alpha-test `discard` → cutout shadow).
//!
//! Shared material-load helpers (`material_load_*` + `texture_pool_sample`) come
//! from the fragment's `masked_alpha.wgsl` (`shared_wgsl/material_load_helpers.wgsl`,
//! included once); the vertex hook + fragment alpha path resolve against that
//! single copy — no redefinition.
//!
//! For a **Custom** material the `MaterialData` struct + loader are emitted by the
//! VERTEX hook; the fragment alpha path reuses them, so the fragment's
//! struct/loader fields are passed EMPTY (the texture helpers + author alpha body
//! still come from the fragment).

use askama::Template;

use crate::{
    dynamic_materials::ShadingBase,
    shaders::{AwsmShaderError, Result},
    shadows::shader::{
        masked_custom_vertex_cache_key::ShaderCacheKeyShadowMaskedCustomVertex,
        masked_template::{
            ShaderTemplateShadowMaskedBindGroups, ShaderTemplateShadowMaskedFragment,
            ShaderTemplateShadowMaskedVertex,
        },
    },
};

/// Combined masked + custom-vertex shadow shader template components.
#[derive(Debug)]
pub struct ShaderTemplateShadowMaskedCustomVertex {
    pub bind_groups: ShaderTemplateShadowMaskedBindGroups,
    pub vertex: ShaderTemplateShadowMaskedVertex,
    pub fragment: ShaderTemplateShadowMaskedFragment,
}

impl TryFrom<&ShaderCacheKeyShadowMaskedCustomVertex> for ShaderTemplateShadowMaskedCustomVertex {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyShadowMaskedCustomVertex) -> Result<Self> {
        // FRAGMENT (alpha test): for a Custom base the struct/loader come from the
        // vertex hook below — pass them EMPTY here so they aren't redefined. The
        // texture helpers + author alpha body are still the fragment's. Built-in
        // bases carry none of these.
        let (texture_helpers, alpha_wgsl) = match &value.dynamic_alpha {
            Some(info) => (info.texture_helpers.clone(), info.alpha_wgsl.clone()),
            None => (String::new(), String::new()),
        };

        Ok(Self {
            bind_groups: ShaderTemplateShadowMaskedBindGroups::new(
                value.texture_pool_arrays_len,
                value.texture_pool_samplers_len,
                // Combined masked + custom-vertex shadow is non-instanced (matches
                // the masked + custom-vertex shadow parents).
                false,
            ),
            vertex: ShaderTemplateShadowMaskedVertex::new(
                false,
                true,
                value.dynamic_vertex.wgsl_vertex.clone(),
                value.dynamic_vertex.struct_decl.clone(),
                value.dynamic_vertex.loader_decl.clone(),
            ),
            fragment: ShaderTemplateShadowMaskedFragment::new(
                value.texture_pool_arrays_len,
                value.texture_pool_samplers_len,
                value.base,
                // Custom struct/loader emitted by the vertex hook — suppress here.
                String::new(),
                String::new(),
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

impl ShaderTemplateShadowMaskedCustomVertex {
    /// Renders the combined masked + custom-vertex shadow shader into WGSL.
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
        Some("Shadow Masked Custom Vertex")
    }
}
