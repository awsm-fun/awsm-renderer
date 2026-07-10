//! Shader templates for the light culling pass.

use askama::Template;

use crate::{
    lights::MAX_PUNCTUAL_LIGHTS,
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
pub struct ShaderTemplateLightCullingBindGroups {
    /// Length of the `lights` uniform array. Same value (`MAX_PUNCTUAL_LIGHTS`)
    /// the shading shaders use so the cull pass and consumers point at
    /// identical declarations of the same physical buffer.
    pub max_punctual_lights: u32,
}

impl ShaderTemplateLightCullingBindGroups {
    /// Creates a bind group template from the cache key.
    pub fn new(_cache_key: &ShaderCacheKeyLightCulling) -> Self {
        Self {
            max_punctual_lights: MAX_PUNCTUAL_LIGHTS as u32,
        }
    }
}

/// Compute shader template for the light culling pass.
#[derive(Template, Debug)]
#[template(path = "light_culling_wgsl/compute.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateLightCullingCompute {
    /// Number of view-space Z slices. Constant-folded into the
    /// exponential-mapping math.
    pub slice_count: u32,
    /// Depth convention: anchors the tile unproject helpers at the NEAR
    /// plane's NDC z (1.0 reverse, 0.0 forward). See the cache-key doc.
    pub reverse_z: bool,
}

impl ShaderTemplateLightCullingCompute {
    /// Creates a compute shader template from the cache key.
    ///
    /// Per-froxel capacity is intentionally absent: the WGSL reads it
    /// from `cull_params` at runtime (no `{{ max_per_froxel_capacity }}`
    /// substitution), so auto-grow never changes the generated source.
    pub fn new(cache_key: &ShaderCacheKeyLightCulling) -> Self {
        Self {
            slice_count: cache_key.slice_count,
            reverse_z: cache_key.reverse_z,
        }
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
