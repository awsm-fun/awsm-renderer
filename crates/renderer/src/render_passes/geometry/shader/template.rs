//! Shader templates for the geometry pass.

use askama::Template;

use crate::{
    render_passes::geometry::shader::cache_key::ShaderCacheKeyGeometry,
    shaders::{AwsmShaderError, Result},
};

/// Geometry pass shader template components.
#[derive(Debug)]
pub struct ShaderTemplateGeometry {
    pub bind_groups: ShaderTemplateGeometryBindGroups,
    pub vertex: ShaderTemplateGeometryVertex,
    pub fragment: ShaderTemplateGeometryFragment,
}

/// Bind group template for the geometry pass.
#[derive(Template, Debug)]
#[template(path = "geometry_wgsl/bind_groups.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateGeometryBindGroups {
    /// `true` when this shader variant takes per-instance attribute
    /// inputs (`location(6..9)` instance transform rows). In that
    /// mode `@group(2) @binding(0)` keeps the legacy
    /// `uniform`-with-dynamic-offset shape; the non-instanced
    /// variant uses a `storage, read` array indexed by
    /// `@builtin(instance_index)` (plan §16.7/§16.8).
    instancing_transforms: bool,
}

impl ShaderTemplateGeometryBindGroups {
    /// Creates a bind group template from the cache key.
    pub fn new(cache_key: &ShaderCacheKeyGeometry) -> Self {
        Self {
            instancing_transforms: cache_key.instancing_transforms,
        }
    }
}

/// Vertex shader template for the geometry pass.
#[derive(Template, Debug)]
#[template(path = "geometry_wgsl/vertex.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateGeometryVertex {
    max_morph_unroll: u32,
    max_skin_unroll: u32,
    instancing_transforms: bool,
}

impl ShaderTemplateGeometryVertex {
    /// Creates a vertex shader template from the cache key.
    pub fn new(cache_key: &ShaderCacheKeyGeometry) -> Self {
        Self {
            max_morph_unroll: 2,
            max_skin_unroll: 2,
            instancing_transforms: cache_key.instancing_transforms,
        }
    }
}

/// Fragment shader template for the geometry pass.
#[derive(Template, Debug)]
#[template(path = "geometry_wgsl/fragment.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateGeometryFragment {}

impl ShaderTemplateGeometryFragment {
    /// Creates a fragment shader template from the cache key.
    pub fn new(_cache_key: &ShaderCacheKeyGeometry) -> Self {
        Self {}
    }
}

impl TryFrom<&ShaderCacheKeyGeometry> for ShaderTemplateGeometry {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyGeometry) -> Result<Self> {
        Ok(Self {
            bind_groups: ShaderTemplateGeometryBindGroups::new(value),
            vertex: ShaderTemplateGeometryVertex::new(value),
            fragment: ShaderTemplateGeometryFragment::new(value),
        })
    }
}

impl ShaderTemplateGeometry {
    /// Renders the geometry shader template into WGSL.
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = self.bind_groups.render()?;
        let vertex_source = self.vertex.render()?;
        let fragment_source = self.fragment.render()?;
        let source = format!(
            "{}\n{}\n{}",
            bind_groups_source, vertex_source, fragment_source
        );

        // print_shader_source(&vertex_source, false);
        //print_shader_source(&source, false);

        Ok(source)
    }

    #[cfg(debug_assertions)]
    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Geometry")
    }
}
