//! Askama templates for the per-shader-id edge_resolve + skybox /
//! final_blend shaders (Priority 3 in
//! docs/plans/more-optimizations.md).

use askama::Template;
use awsm_materials::MaterialShaderId;

use crate::{
    dynamic_materials::BucketEntry,
    render_passes::material_opaque::shader::{
        cache_key::DynamicShaderInfo,
        edge_cache_key::{
            ShaderCacheKeyMaterialEdgeResolve, ShaderCacheKeyMaterialFinalBlend,
            ShaderCacheKeyMaterialSkyboxEdgeResolve,
        },
        template::MipmapMode,
    },
    shaders::{AwsmShaderError, Result},
};

/// Bind-group + compute shader pair for the per-shader-id edge_resolve.
pub struct ShaderTemplateMaterialEdgeResolve {
    pub bind_groups: ShaderTemplateMaterialEdgeResolveBindGroups,
    pub compute: ShaderTemplateMaterialEdgeResolveCompute,
}

#[derive(Template, Debug)]
#[template(
    path = "material_opaque_wgsl/edge_resolve_bind_groups.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateMaterialEdgeResolveBindGroups {
    /// Forwarded to the included primary opaque bind_groups.wgsl —
    /// edge_resolve shares its group(0) shape.
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub debug:
        crate::render_passes::material_opaque::shader::template::ShaderTemplateMaterialOpaqueDebug,
    pub mipmap: MipmapMode,
    pub multisampled_geometry: bool,
    pub msaa_sample_count: u32,
    pub shadow_group_index: u32,
    pub bucket_entries: Vec<BucketEntry>,
    pub pad_words_iter: Vec<u32>,
    pub sscs_available: bool,
}

#[derive(Template, Debug)]
#[template(
    path = "material_opaque_wgsl/edge_resolve.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateMaterialEdgeResolveCompute {
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub multisampled_geometry: bool,
    pub msaa_sample_count: u32,
    pub mipmap: MipmapMode,
    pub use_mesh_light_slices: bool,
    pub shadows_enabled: bool,
    pub materials_wgsl: String,
    pub shader_id_consts: String,
    pub shader_id: MaterialShaderId,
    pub dynamic_struct_decl: String,
    pub dynamic_loader_decl: String,
    pub dynamic_wgsl_fragment: String,
    /// Hard-coded bucket index for this shader_id (used in the slot_map
    /// scan to find this thread's accumulator slot).
    pub bucket_index: u32,
    /// `args_<name>_edge` field name — used to read the per-bucket
    /// entry count from `edge_buffers`.
    pub bucket_args_field: String,
    /// `args_<name>_sample_list_base` field name — used to index into
    /// `edge_layout` for this shader_id's sample-entry list.
    pub bucket_sample_list_base: String,
    /// Used by the templated `apply_lighting` include for IBL gating.
    pub debug:
        crate::render_passes::material_opaque::shader::template::ShaderTemplateMaterialOpaqueDebug,
    pub bucket_entries: Vec<BucketEntry>,
}

impl ShaderTemplateMaterialEdgeResolveCompute {
    pub fn has_lighting_ibl(&self) -> bool {
        true
    }

    pub fn has_lighting_punctual(&self) -> bool {
        true
    }
}

impl TryFrom<&ShaderCacheKeyMaterialEdgeResolve> for ShaderTemplateMaterialEdgeResolve {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyMaterialEdgeResolve) -> Result<Self> {
        let mipmap = if value.mipmaps {
            MipmapMode::Gradient
        } else {
            MipmapMode::None
        };
        let bucket_entries = value.bucket_entries.clone();
        let pad_words_iter: Vec<u32> = (0
            ..crate::render_passes::material_classify::shader::template::pad_words_count(
                bucket_entries.len() as u32,
            ))
            .collect();
        let entry = bucket_entries
            .get(value.bucket_index as usize)
            .ok_or_else(|| {
                AwsmShaderError::DuplicateAttribute(format!(
                    "edge_resolve: bucket_index {} out of range for {} entries",
                    value.bucket_index,
                    bucket_entries.len()
                ))
            })?;
        let bucket_args_field = entry.args_field();
        let bucket_sample_list_base = format!("{}_sample_list_base", entry.args_field());

        // DynamicShaderInfo is intentionally `!Default` — its fields
        // are mandatory when the shader_id is dynamic. Avoid the
        // accidental Default reach.
        let _unused_dynamic_info: Option<&DynamicShaderInfo> = value.dynamic_shader.as_ref();

        Ok(Self {
            bind_groups: ShaderTemplateMaterialEdgeResolveBindGroups {
                texture_pool_arrays_len: value.texture_pool_arrays_len,
                texture_pool_samplers_len: value.texture_pool_samplers_len,
                debug: crate::render_passes::material_opaque::shader::template::ShaderTemplateMaterialOpaqueDebug::new(),
                mipmap,
                multisampled_geometry: true,
                msaa_sample_count: 4,
                shadow_group_index: 3,
                bucket_entries: bucket_entries.clone(),
                pad_words_iter,
                sscs_available: true,
            },
            compute: ShaderTemplateMaterialEdgeResolveCompute {
                texture_pool_arrays_len: value.texture_pool_arrays_len,
                texture_pool_samplers_len: value.texture_pool_samplers_len,
                multisampled_geometry: true,
                msaa_sample_count: 4,
                mipmap,
                use_mesh_light_slices: true,
                shadows_enabled: true,
                materials_wgsl: awsm_materials::registry::build_materials_wgsl(),
                shader_id_consts: awsm_materials::registry::build_shader_id_consts(),
                shader_id: value.shader_id,
                dynamic_struct_decl: value
                    .dynamic_shader
                    .as_ref()
                    .map(|d| d.struct_decl.clone())
                    .unwrap_or_default(),
                dynamic_loader_decl: value
                    .dynamic_shader
                    .as_ref()
                    .map(|d| d.loader_decl.clone())
                    .unwrap_or_default(),
                dynamic_wgsl_fragment: value
                    .dynamic_shader
                    .as_ref()
                    .map(|d| d.wgsl_fragment.clone())
                    .unwrap_or_default(),
                bucket_index: value.bucket_index,
                bucket_args_field,
                bucket_sample_list_base,
                debug: crate::render_passes::material_opaque::shader::template::ShaderTemplateMaterialOpaqueDebug::new(),
                bucket_entries: bucket_entries.clone(),
            },
        })
    }
}

impl ShaderTemplateMaterialEdgeResolve {
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = self.bind_groups.render()?;
        let compute_source = self.compute.render()?;
        Ok(format!("{}\n{}", bind_groups_source, compute_source))
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("Material Edge Resolve")
    }
}

// ─────────────────────────────────────────────────────────────────────
// Skybox edge resolve template.
// ─────────────────────────────────────────────────────────────────────

pub struct ShaderTemplateMaterialSkyboxEdgeResolve {
    pub bind_groups: ShaderTemplateMaterialSkyboxEdgeBindGroups,
    pub compute: ShaderTemplateMaterialSkyboxEdgeCompute,
}

#[derive(Template, Debug)]
#[template(
    path = "material_opaque_wgsl/skybox_edge_bind_groups.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateMaterialSkyboxEdgeBindGroups {
    pub bucket_entries: Vec<BucketEntry>,
}

#[derive(Template, Debug)]
#[template(
    path = "material_opaque_wgsl/skybox_edge_resolve.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateMaterialSkyboxEdgeCompute {}

impl TryFrom<&ShaderCacheKeyMaterialSkyboxEdgeResolve> for ShaderTemplateMaterialSkyboxEdgeResolve {
    type Error = AwsmShaderError;
    fn try_from(value: &ShaderCacheKeyMaterialSkyboxEdgeResolve) -> Result<Self> {
        Ok(Self {
            bind_groups: ShaderTemplateMaterialSkyboxEdgeBindGroups {
                bucket_entries: value.bucket_entries.clone(),
            },
            compute: ShaderTemplateMaterialSkyboxEdgeCompute {},
        })
    }
}

impl ShaderTemplateMaterialSkyboxEdgeResolve {
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = self.bind_groups.render()?;
        let compute_source = self.compute.render()?;
        Ok(format!("{}\n{}", bind_groups_source, compute_source))
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("Material Skybox Edge Resolve")
    }
}

// ─────────────────────────────────────────────────────────────────────
// Final blend template.
// ─────────────────────────────────────────────────────────────────────

pub struct ShaderTemplateMaterialFinalBlend {
    pub bind_groups: ShaderTemplateMaterialFinalBlendBindGroups,
    pub compute: ShaderTemplateMaterialFinalBlendCompute,
}

#[derive(Template, Debug)]
#[template(
    path = "material_opaque_wgsl/final_blend_bind_groups.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateMaterialFinalBlendBindGroups {
    pub bucket_entries: Vec<BucketEntry>,
    pub color_format: String,
}

#[derive(Template, Debug)]
#[template(
    path = "material_opaque_wgsl/final_blend.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateMaterialFinalBlendCompute {}

impl TryFrom<&ShaderCacheKeyMaterialFinalBlend> for ShaderTemplateMaterialFinalBlend {
    type Error = AwsmShaderError;
    fn try_from(value: &ShaderCacheKeyMaterialFinalBlend) -> Result<Self> {
        Ok(Self {
            bind_groups: ShaderTemplateMaterialFinalBlendBindGroups {
                bucket_entries: value.bucket_entries.clone(),
                color_format: value.color_format.clone(),
            },
            compute: ShaderTemplateMaterialFinalBlendCompute {},
        })
    }
}

impl ShaderTemplateMaterialFinalBlend {
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = self.bind_groups.render()?;
        let compute_source = self.compute.render()?;
        Ok(format!("{}\n{}", bind_groups_source, compute_source))
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("Material Final Blend")
    }
}
