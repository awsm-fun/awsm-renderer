//! Shader templates for the light culling pass.

use askama::Template;

use crate::{
    render_passes::light_culling::shader::cache_key::ShaderCacheKeyLightCulling,
    shaders::{AwsmShaderError, Result},
};

/// Light culling shader template components.
pub struct ShaderTemplateLightCulling {
    pub bind_groups: ShaderTemplateLightCullingBindGroups,
    pub compute: ShaderTemplateLightCullingCompute,
}

/// Bind group template for the light culling pass.
#[derive(Template, Debug)]
#[template(path = "light_culling_wgsl/bind_groups.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateLightCullingBindGroups {}

impl ShaderTemplateLightCullingBindGroups {
    /// Creates a bind group template from the cache key.
    pub fn new(_cache_key: &ShaderCacheKeyLightCulling) -> Self {
        Self {}
    }
}

/// Compute shader template for the light culling pass.
#[derive(Template, Debug)]
#[template(path = "light_culling_wgsl/compute.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateLightCullingCompute {}

impl ShaderTemplateLightCullingCompute {
    /// Creates a compute shader template from the cache key.
    pub fn new(_cache_key: &ShaderCacheKeyLightCulling) -> Self {
        Self {}
    }
}

impl TryFrom<&ShaderCacheKeyLightCulling> for ShaderTemplateLightCulling {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyLightCulling) -> Result<Self> {
        Ok(Self {
            bind_groups: ShaderTemplateLightCullingBindGroups::new(value),
            compute: ShaderTemplateLightCullingCompute::new(value),
        })
    }
}

impl ShaderTemplateLightCulling {
    /// Renders the light culling shader template into WGSL.
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = self.bind_groups.render()?;
        let compute_source = self.compute.render()?;
        Ok(format!("{}\n{}", bind_groups_source, compute_source))
    }

    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Light Culling")
    }
}
