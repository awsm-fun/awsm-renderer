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
    /// `true` when `@group(2) @binding(0)` is a `storage, read` array
    /// of `GeometryMeshMeta` indexed by `@builtin(instance_index)`
    /// (requires the `indirect-first-instance` WebGPU feature on the
    /// device). `false` when it's a uniform with dynamic offset (the
    /// portable shape; also the only shape used by the instanced
    /// path).
    meta_storage_array: bool,
}

impl ShaderTemplateGeometryBindGroups {
    /// Creates a bind group template from the cache key.
    pub fn new(cache_key: &ShaderCacheKeyGeometry) -> Self {
        Self {
            meta_storage_array: cache_key.meta_storage_array,
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
    /// Same flag as on [`ShaderTemplateGeometryBindGroups`] — when
    /// true, the vertex `main` loads `geometry_mesh_meta` from the
    /// storage array via `@builtin(instance_index)`. When false, the
    /// uniform binding is already populated (set via dynamic offset
    /// by the CPU) and no shader-side load is needed.
    meta_storage_array: bool,
}

impl ShaderTemplateGeometryVertex {
    /// Creates a vertex shader template from the cache key.
    pub fn new(cache_key: &ShaderCacheKeyGeometry) -> Self {
        Self {
            max_morph_unroll: 2,
            max_skin_unroll: 2,
            instancing_transforms: cache_key.instancing_transforms,
            meta_storage_array: cache_key.meta_storage_array,
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

    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Geometry")
    }
}
