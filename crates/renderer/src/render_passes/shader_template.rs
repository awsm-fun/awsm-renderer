//! Shader template variants for render passes.

use crate::{
    render_passes::{
        display::shader::template::ShaderTemplateDisplay,
        effects::shader::template::ShaderTemplateEffects,
        geometry::shader::template::ShaderTemplateGeometry,
        light_culling::shader::template::ShaderTemplateLightCulling,
        material_classify::shader::template::ShaderTemplateMaterialClassify,
        material_opaque::shader::template::{
            ShaderTemplateMaterialOpaque, ShaderTemplateMaterialOpaqueEmpty,
        },
        material_transparent::shader::template::ShaderTemplateMaterialTransparent,
        shader_cache_key::ShaderCacheKeyRenderPass,
    },
    shaders::AwsmShaderError,
};

/// Render-pass shader template variants.
pub enum ShaderTemplateRenderPass {
    Geometry(ShaderTemplateGeometry),
    LightCulling(ShaderTemplateLightCulling),
    MaterialClassify(ShaderTemplateMaterialClassify),
    MaterialOpaque(ShaderTemplateMaterialOpaque),
    MaterialOpaqueEmpty(ShaderTemplateMaterialOpaqueEmpty),
    MaterialTransparent(ShaderTemplateMaterialTransparent),
    Effects(ShaderTemplateEffects),
    Display(ShaderTemplateDisplay),
}

impl TryFrom<&ShaderCacheKeyRenderPass> for ShaderTemplateRenderPass {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyRenderPass) -> std::result::Result<Self, Self::Error> {
        match value {
            ShaderCacheKeyRenderPass::Geometry(cache_key) => {
                Ok(ShaderTemplateRenderPass::Geometry(cache_key.try_into()?))
            }
            ShaderCacheKeyRenderPass::LightCulling(cache_key) => Ok(
                ShaderTemplateRenderPass::LightCulling(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::MaterialClassify(cache_key) => Ok(
                ShaderTemplateRenderPass::MaterialClassify(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::MaterialOpaque(cache_key) => Ok(
                ShaderTemplateRenderPass::MaterialOpaque(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::MaterialOpaqueEmpty(cache_key) => Ok(
                ShaderTemplateRenderPass::MaterialOpaqueEmpty(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::MaterialTransparent(cache_key) => Ok(
                ShaderTemplateRenderPass::MaterialTransparent(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::Effects(cache_key) => {
                Ok(ShaderTemplateRenderPass::Effects(cache_key.try_into()?))
            }
            ShaderCacheKeyRenderPass::Display(cache_key) => {
                Ok(ShaderTemplateRenderPass::Display(cache_key.try_into()?))
            }
        }
    }
}

impl ShaderTemplateRenderPass {
    /// Renders the template into WGSL source.
    pub fn into_source(self) -> std::result::Result<String, AwsmShaderError> {
        match self {
            ShaderTemplateRenderPass::Geometry(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::LightCulling(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::MaterialClassify(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::MaterialOpaque(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::MaterialOpaqueEmpty(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::MaterialTransparent(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::Effects(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::Display(tmpl) => tmpl.into_source(),
        }
    }

    #[cfg(debug_assertions)]
    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        match self {
            ShaderTemplateRenderPass::Geometry(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::LightCulling(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::MaterialClassify(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::MaterialOpaque(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::MaterialOpaqueEmpty(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::MaterialTransparent(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::Effects(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::Display(tmpl) => tmpl.debug_label(),
        }
    }
}
