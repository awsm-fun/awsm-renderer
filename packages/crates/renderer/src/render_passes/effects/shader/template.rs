//! Shader templates for the effects pass.

use askama::Template;

use crate::{
    render_passes::effects::shader::cache_key::{BloomPhase, ShaderCacheKeyEffects},
    shaders::{AwsmShaderError, Result},
};

/// Effects pass shader template components.
#[derive(Debug)]
pub struct ShaderTemplateEffects {
    pub bind_groups: ShaderTemplateEffectsBindGroups,
    pub compute: ShaderTemplateEffectsCompute,
}

/// Bind group template for the effects pass.
#[derive(Template, Debug)]
#[template(path = "effects_wgsl/bind_groups.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateEffectsBindGroups {
    pub smaa_anti_alias: bool,
    pub multisampled_geometry: bool,
    pub dof: bool,
    pub debug: ShaderTemplateEffectsDebug,
}

impl ShaderTemplateEffectsBindGroups {
    /// Creates a bind group template from the cache key.
    pub fn new(cache_key: &ShaderCacheKeyEffects) -> Self {
        Self {
            smaa_anti_alias: cache_key.smaa_anti_alias,
            multisampled_geometry: cache_key.multisampled_geometry,
            dof: cache_key.dof,
            debug: ShaderTemplateEffectsDebug::new(),
        }
    }
}

/// Compute shader template for the effects pass.
#[derive(Template, Debug)]
#[template(path = "effects_wgsl/compute.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateEffectsCompute {
    pub smaa_anti_alias: bool,
    pub multisampled_geometry: bool,
    /// Bloom blend is enabled (`BloomPhase::Blend` — the pre-built pyramid
    /// glow is added over the composite)
    pub bloom: bool,
    pub dof: bool,
    /// Depth convention (003) — read by the DoF include (`helpers/dof.wgsl`).
    pub reverse_z: bool,
    pub debug: ShaderTemplateEffectsDebug,
}

impl ShaderTemplateEffectsCompute {
    /// Creates a compute shader template from the cache key.
    pub fn new(cache_key: &ShaderCacheKeyEffects) -> Self {
        let bloom = cache_key.bloom_phase == BloomPhase::Blend;

        Self {
            smaa_anti_alias: cache_key.smaa_anti_alias,
            multisampled_geometry: cache_key.multisampled_geometry,
            bloom,
            dof: cache_key.dof,
            reverse_z: cache_key.reverse_z,
            debug: ShaderTemplateEffectsDebug::new(),
        }
    }
}

impl TryFrom<&ShaderCacheKeyEffects> for ShaderTemplateEffects {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyEffects) -> Result<Self> {
        Ok(Self {
            bind_groups: ShaderTemplateEffectsBindGroups::new(value),
            compute: ShaderTemplateEffectsCompute::new(value),
        })
    }
}

impl ShaderTemplateEffects {
    /// Renders the effects shader template into WGSL.
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = self.bind_groups.render()?;
        let compute_source = self.compute.render()?;
        Ok(format!("{}\n{}", bind_groups_source, compute_source))
    }

    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Effects")
    }
}

/// Debug toggles for effects shaders.
#[derive(Default, Debug, Clone)]
pub struct ShaderTemplateEffectsDebug {
    pub smaa_edges: bool,
}

impl ShaderTemplateEffectsDebug {
    /// Creates a default debug config.
    pub fn new() -> Self {
        Self { smaa_edges: false }
    }
}
