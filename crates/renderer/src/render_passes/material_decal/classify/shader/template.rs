use askama::Template;

use crate::{
    render_passes::material_decal::classify::shader::cache_key::ShaderCacheKeyDecalClassify,
    shaders::{AwsmShaderError, Result},
};

#[derive(Template, Debug, Default)]
#[template(path = "decal_classify_wgsl/compute.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateDecalClassify {
    pub hzb_enabled: bool,
}

impl TryFrom<&ShaderCacheKeyDecalClassify> for ShaderTemplateDecalClassify {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyDecalClassify) -> Result<Self> {
        Ok(Self {
            hzb_enabled: value.hzb_enabled,
        })
    }
}

impl ShaderTemplateDecalClassify {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    #[cfg(debug_assertions)]
    pub fn debug_label(&self) -> Option<&str> {
        Some("Decal Classify")
    }
}
