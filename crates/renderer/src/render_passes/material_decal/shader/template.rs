//! Material decal compute shader template.

use askama::Template;

use crate::{
    render_passes::material_decal::shader::cache_key::ShaderCacheKeyMaterialDecal,
    shaders::{AwsmShaderError, Result},
};

/// Decal pass shader template — bind groups + compute.
pub struct ShaderTemplateMaterialDecal {
    pub bind_groups: ShaderTemplateMaterialDecalBindGroups,
    pub compute: ShaderTemplateMaterialDecalCompute,
}

#[derive(Template, Debug)]
#[template(path = "material_decal_wgsl/bind_groups.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateMaterialDecalBindGroups {
    pub multisampled_geometry: bool,
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
}

#[derive(Template, Debug)]
#[template(path = "material_decal_wgsl/compute.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateMaterialDecalCompute {
    pub multisampled_geometry: bool,
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
}

impl TryFrom<&ShaderCacheKeyMaterialDecal> for ShaderTemplateMaterialDecal {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyMaterialDecal) -> Result<Self> {
        let multisampled_geometry = value.msaa_sample_count.is_some();
        Ok(Self {
            bind_groups: ShaderTemplateMaterialDecalBindGroups {
                multisampled_geometry,
                texture_pool_arrays_len: value.texture_pool_arrays_len,
                texture_pool_samplers_len: value.texture_pool_samplers_len,
            },
            compute: ShaderTemplateMaterialDecalCompute {
                multisampled_geometry,
                texture_pool_arrays_len: value.texture_pool_arrays_len,
                texture_pool_samplers_len: value.texture_pool_samplers_len,
            },
        })
    }
}

impl ShaderTemplateMaterialDecal {
    pub fn into_source(self) -> Result<String> {
        let bg = self.bind_groups.render()?;
        let cs = self.compute.render()?;
        Ok(format!("{}\n{}", bg, cs))
    }

    #[cfg(debug_assertions)]
    pub fn debug_label(&self) -> Option<&str> {
        Some("Material Decal")
    }
}
