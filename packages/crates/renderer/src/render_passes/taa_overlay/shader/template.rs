//! Shader template for the TAA deforming-mesh velocity overlay.
//!
//! Composes the geometry pass's own modules: its bind groups (the plain ones,
//! or the custom-vertex ones that declare the materials + texture pool a
//! vertex hook reads), its vertex shader built with `velocity` (the current
//! and previous evaluation, `shared_wgsl/vertex/motion_prev.wgsl`), and the
//! overlay fragment (`taa_overlay_wgsl/fragment.wgsl`). The vertex stage is
//! therefore exactly the one the geometry-target path runs.

use askama::Template;

use crate::{
    render_passes::geometry::shader::{
        cache_key::ShaderCacheKeyGeometry,
        template::{ShaderTemplateGeometryBindGroups, ShaderTemplateGeometryVertex},
    },
    render_passes::taa_overlay::shader::cache_key::ShaderCacheKeyTaaOverlay,
    shaders::{AwsmShaderError, Result},
};

/// The custom-vertex variant's bind groups (same file and inputs as the
/// geometry pass's custom-vertex template).
#[derive(Template, Debug)]
#[template(path = "custom_vertex_wgsl/bind_groups.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateTaaOverlayCustomVertexBindGroups {
    texture_pool_arrays_len: u32,
    texture_pool_samplers_len: u32,
}

#[derive(Template, Debug)]
#[template(path = "taa_overlay_wgsl/fragment.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateTaaOverlayFragment {
    debug: bool,
    reverse_z: bool,
    meta_slot_bytes: usize,
}

#[derive(Debug)]
pub enum ShaderTemplateTaaOverlayBindGroups {
    Plain(ShaderTemplateGeometryBindGroups),
    CustomVertex(ShaderTemplateTaaOverlayCustomVertexBindGroups),
}

#[derive(Debug)]
pub struct ShaderTemplateTaaOverlay {
    pub bind_groups: ShaderTemplateTaaOverlayBindGroups,
    pub vertex: ShaderTemplateGeometryVertex,
    pub fragment: ShaderTemplateTaaOverlayFragment,
}

impl TryFrom<&ShaderCacheKeyTaaOverlay> for ShaderTemplateTaaOverlay {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyTaaOverlay) -> Result<Self> {
        let geometry_key = ShaderCacheKeyGeometry {
            instancing_transforms: value.instancing_transforms,
            meta_storage_array: value.meta_storage_array,
            msaa_samples: None,
            velocity: true,
            mip_bias: false,
            dynamic_vertex_shader: value
                .custom_vertex
                .as_ref()
                .map(|c| c.dynamic_vertex.clone()),
        };
        let bind_groups = match value.custom_vertex.as_ref() {
            Some(c) => ShaderTemplateTaaOverlayBindGroups::CustomVertex(
                ShaderTemplateTaaOverlayCustomVertexBindGroups {
                    texture_pool_arrays_len: c.texture_pool_arrays_len,
                    texture_pool_samplers_len: c.texture_pool_samplers_len,
                },
            ),
            None => ShaderTemplateTaaOverlayBindGroups::Plain(
                ShaderTemplateGeometryBindGroups::new(&geometry_key),
            ),
        };
        Ok(Self {
            bind_groups,
            vertex: ShaderTemplateGeometryVertex::new(&geometry_key),
            fragment: ShaderTemplateTaaOverlayFragment {
                debug: value.debug,
                reverse_z: value.reverse_z,
                meta_slot_bytes:
                    crate::meshes::meta::material_meta::MATERIAL_MESH_META_BYTE_ALIGNMENT,
            },
        })
    }
}

impl ShaderTemplateTaaOverlay {
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = match self.bind_groups {
            ShaderTemplateTaaOverlayBindGroups::Plain(t) => t.render()?,
            ShaderTemplateTaaOverlayBindGroups::CustomVertex(t) => t.render()?,
        };
        let vertex_source = self.vertex.render()?;
        let fragment_source = self.fragment.render()?;
        Ok(format!(
            "{}\n{}\n{}",
            bind_groups_source, vertex_source, fragment_source
        ))
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("TAA Velocity Overlay")
    }
}
