//! Occlusion compute shader templates.

use askama::Template;

use crate::{
    render_passes::occlusion::shader::cache_key::{
        ShaderCacheKeyOcclusionCompaction, ShaderCacheKeyOcclusionCull,
    },
    shaders::{AwsmShaderError, Result},
};

#[derive(Template, Debug, Default)]
#[template(path = "occlusion_wgsl/cull.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateOcclusionCull {
    pub reverse_z: bool,
}

impl TryFrom<&ShaderCacheKeyOcclusionCull> for ShaderTemplateOcclusionCull {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyOcclusionCull) -> Result<Self> {
        Ok(Self {
            reverse_z: value.reverse_z,
        })
    }
}

impl ShaderTemplateOcclusionCull {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("Occlusion Cull")
    }
}

#[derive(Template, Debug, Default)]
#[template(path = "occlusion_wgsl/compaction.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateOcclusionCompaction {
    /// Mirrors `ShaderCacheKeyOcclusionCompaction::write_first_instance`.
    /// Gates the `indirect_args[mesh_slot].first_instance = mesh_slot`
    /// write in the WGSL — only emitted when the device supports the
    /// `indirect-first-instance` feature.
    write_first_instance: bool,
}

impl TryFrom<&ShaderCacheKeyOcclusionCompaction> for ShaderTemplateOcclusionCompaction {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyOcclusionCompaction) -> Result<Self> {
        Ok(Self {
            write_first_instance: value.write_first_instance,
        })
    }
}

impl ShaderTemplateOcclusionCompaction {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("Occlusion Compaction")
    }
}
