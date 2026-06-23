//! Shader template for the **custom-vertex** geometry raster variant.
//!
//! Composes: the MASKED geometry bind groups (they declare the `materials`
//! storage buffer + texture pool the hook's `material_data_load` /
//! `material_sample_<name>` read — the plain geometry bind groups do NOT) + the
//! geometry VERTEX shader built with `has_custom_vertex` (compiles the gated
//! `custom_displace_vertex` hook) + the PLAIN geometry FRAGMENT (writes the
//! visibility buffer; this variant is opaque, not alpha-tested).

use askama::Template;

use crate::{
    render_passes::geometry::shader::{
        cache_key::ShaderCacheKeyGeometry,
        custom_vertex_cache_key::ShaderCacheKeyGeometryCustomVertex,
        template::{ShaderTemplateGeometryFragment, ShaderTemplateGeometryVertex},
    },
    shaders::{AwsmShaderError, Result},
};

/// Bind-group template for the custom-vertex geometry variant.
///
/// Includes the shared type-definition modules + the MASKED bind-group
/// declarations (so `materials` + the texture pool the hook's loader reads are
/// in scope). See `custom_vertex_wgsl/bind_groups.wgsl`. Carries the same
/// texture-pool length fields the included masked declarations interpolate.
#[derive(Template, Debug)]
#[template(path = "custom_vertex_wgsl/bind_groups.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateGeometryCustomVertexBindGroups {
    texture_pool_arrays_len: u32,
    texture_pool_samplers_len: u32,
}

/// Custom-vertex geometry shader template components.
#[derive(Debug)]
pub struct ShaderTemplateGeometryCustomVertex {
    pub bind_groups: ShaderTemplateGeometryCustomVertexBindGroups,
    pub vertex: ShaderTemplateGeometryVertex,
    pub fragment: ShaderTemplateGeometryFragment,
}

impl TryFrom<&ShaderCacheKeyGeometryCustomVertex> for ShaderTemplateGeometryCustomVertex {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyGeometryCustomVertex) -> Result<Self> {
        // The vertex stage is the plain geometry vertex compiled with the
        // custom-vertex hook: feed it the same dynamic info the cache key
        // carries (struct/loader + author body) via `dynamic_vertex_shader`.
        let vertex_key = ShaderCacheKeyGeometry {
            instancing_transforms: value.instancing_transforms,
            meta_storage_array: value.meta_storage_array,
            msaa_samples: value.msaa_samples,
            dynamic_vertex_shader: Some(value.dynamic_vertex.clone()),
        };
        let fragment_key = ShaderCacheKeyGeometry {
            instancing_transforms: value.instancing_transforms,
            meta_storage_array: value.meta_storage_array,
            msaa_samples: value.msaa_samples,
            dynamic_vertex_shader: None,
        };

        Ok(Self {
            // Reuse the masked bind groups (they declare `materials` + the
            // texture pool the hook's loader/helpers reference) plus the shared
            // type modules the plain fragment doesn't include.
            bind_groups: ShaderTemplateGeometryCustomVertexBindGroups {
                texture_pool_arrays_len: value.texture_pool_arrays_len,
                texture_pool_samplers_len: value.texture_pool_samplers_len,
            },
            vertex: ShaderTemplateGeometryVertex::new(&vertex_key),
            fragment: ShaderTemplateGeometryFragment::new(&fragment_key),
        })
    }
}

impl ShaderTemplateGeometryCustomVertex {
    /// Renders the custom-vertex geometry shader template into WGSL.
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
        Some("Geometry Custom Vertex")
    }
}
