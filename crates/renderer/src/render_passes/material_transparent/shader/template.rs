//! Shader templates for the transparent material pass.

use askama::Template;

use crate::{
    dynamic_materials::ShadingBase,
    render_passes::material_transparent::shader::cache_key::ShaderCacheKeyMaterialTransparent,
    shaders::{AwsmShaderError, Result},
};

/// Transparent material shader template components.
#[derive(Debug)]
pub struct ShaderTemplateMaterialTransparent {
    pub includes: ShaderTemplateTransparentMaterialIncludes,
    pub bind_groups: ShaderTemplateTransparentMaterialBindGroups,
    pub vertex: ShaderTemplateTransparentMaterialVertex,
    pub fragment: ShaderTemplateTransparentMaterialFragment,
}

/// Shared include template for transparent materials.
#[derive(Template, Debug)]
#[template(
    path = "material_transparent_wgsl/includes.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateTransparentMaterialIncludes {
    pub max_morph_unroll: u32,
    pub max_skin_unroll: u32,
    pub instancing_transforms: bool,
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub color_sets: Option<u32>,
    pub uv_sets: u32,
    pub debug: ShaderTemplateMaterialTransparentDebug,
    /// Whether `lights.wgsl` should wire shadow sampling into
    /// `apply_lighting`. Enabled for the transparent pass; the shared
    /// shadow bind group sits at slot 1.
    pub shadows_enabled: bool,
    /// Transparent always uses the per-froxel punctual walk produced
    /// by the GPU light-culling pass. The shared `lights.wgsl` emits
    /// `apply_lighting_per_froxel*` only when this is `true`.
    pub use_froxel_lights: bool,
    /// Froxel slice count baked into the consumer shader's
    /// exponential z-slice math. Read from the cache key.
    pub froxel_slice_count: u32,
    /// Concatenated `wgsl_fragment()` of every enabled material — see
    /// `awsm_materials::registry::build_materials_wgsl`.
    pub materials_wgsl: String,
    /// Generated `const SHADER_ID_X: u32 = N;` lines — see
    /// `awsm_materials::registry::build_shader_id_consts`.
    pub shader_id_consts: String,
    /// PBR feature mask for the shared `brdf.wgsl` include's compile-time
    /// `{% if pbr_features.<x> %}` gating — the transparent material's exact
    /// feature-set (each transparent material compiles its own pipeline).
    pub pbr_features: awsm_materials::pbr::PbrFeatures,
}
impl ShaderTemplateTransparentMaterialIncludes {
    /// Creates include template data from the cache key.
    pub fn new(cache_key: &ShaderCacheKeyMaterialTransparent) -> Self {
        Self {
            max_morph_unroll: 2,
            max_skin_unroll: 2,
            instancing_transforms: cache_key.instancing_transforms,
            texture_pool_arrays_len: cache_key.texture_pool_arrays_len,
            texture_pool_samplers_len: cache_key.texture_pool_samplers_len,
            color_sets: cache_key.attributes.color_sets,
            uv_sets: cache_key.attributes.uv_sets.unwrap_or_default(),
            debug: ShaderTemplateMaterialTransparentDebug::new(),
            shadows_enabled: true,
            use_froxel_lights: true,
            froxel_slice_count: cache_key.froxel_slice_count,
            materials_wgsl: awsm_materials::registry::build_materials_wgsl(),
            shader_id_consts: awsm_materials::registry::build_shader_id_consts(),
            // Per-material specialization: the shared brdf /
            // material_color_calc includes gate on exactly this transparent
            // material's feature-set (no uber all()).
            pbr_features: awsm_materials::pbr::PbrFeatures::from_bits(cache_key.pbr_features),
        }
    }

    /// Returns true if the shader includes IBL lighting.
    pub fn has_lighting_ibl(&self) -> bool {
        match self.debug.lighting {
            ShaderTemplateMaterialTransparentDebugLighting::None => true,
            ShaderTemplateMaterialTransparentDebugLighting::IblOnly => true,
            ShaderTemplateMaterialTransparentDebugLighting::PunctualOnly => false,
        }
    }

    /// Returns true if the shader includes punctual lighting.
    pub fn has_lighting_punctual(&self) -> bool {
        match self.debug.lighting {
            ShaderTemplateMaterialTransparentDebugLighting::None => true,
            ShaderTemplateMaterialTransparentDebugLighting::IblOnly => false,
            ShaderTemplateMaterialTransparentDebugLighting::PunctualOnly => true,
        }
    }
}

/// Bind group template for transparent materials.
#[derive(Template, Debug)]
#[template(
    path = "material_transparent_wgsl/bind_groups.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateTransparentMaterialBindGroups {
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub multisampled_geometry: bool,
    /// Bind-group slot used by the shared shadow `bind_groups.wgsl`
    /// include. 16.B folded `lights` into `main` (group 0) so the
    /// shadow group lives at slot 1 on the transparent pipeline.
    pub shadow_group_index: u32,
    /// SSCS is opaque-only — the transparent pass doesn't have access
    /// to a depth texture it can sample on the same frame without a
    /// feedback loop. `false` here makes `apply_sscs` short-circuit.
    pub sscs_available: bool,
}

impl ShaderTemplateTransparentMaterialBindGroups {
    /// Creates a bind group template from the cache key.
    pub fn new(cache_key: &ShaderCacheKeyMaterialTransparent) -> Self {
        Self {
            texture_pool_arrays_len: cache_key.texture_pool_arrays_len,
            texture_pool_samplers_len: cache_key.texture_pool_samplers_len,
            multisampled_geometry: cache_key.msaa_sample_count.is_some(),
            shadow_group_index: 1,
            sscs_available: false,
        }
    }
}

/// Vertex shader template for transparent materials.
#[derive(Template, Debug)]
#[template(
    path = "material_transparent_wgsl/vertex.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateTransparentMaterialVertex {
    pub instancing_transforms: bool,
    pub uv_sets: u32,
    pub color_sets: u32,
    pub in_uv_set_start: u32,
    pub in_color_set_start: u32,
    pub out_uv_set_start: u32,
    pub out_color_set_start: u32,
}

impl ShaderTemplateTransparentMaterialVertex {
    /// Creates a vertex shader template from the cache key.
    pub fn new(cache_key: &ShaderCacheKeyMaterialTransparent) -> Self {
        let uv_sets = cache_key.attributes.uv_sets.unwrap_or_default();
        let color_sets = cache_key.attributes.color_sets.unwrap_or_default();

        // after instancing or tangent
        let in_color_set_start = if cache_key.instancing_transforms {
            7
        } else {
            3
        };

        let in_uv_set_start = in_color_set_start + color_sets;

        // after world_tangent (loc 2) + instance_id (loc 3)
        let out_color_set_start = 4;
        let out_uv_set_start = out_color_set_start + color_sets;

        Self {
            instancing_transforms: cache_key.instancing_transforms,
            uv_sets,
            color_sets,
            in_uv_set_start,
            in_color_set_start,
            out_uv_set_start,
            out_color_set_start,
        }
    }
}

/// Fragment shader template for transparent materials.
#[derive(Template, Debug)]
#[template(
    path = "material_transparent_wgsl/fragment.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateTransparentMaterialFragment {
    pub uv_sets: u32,
    pub color_sets: u32,
    pub in_uv_set_start: u32,
    pub in_color_set_start: u32,
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub debug: ShaderTemplateMaterialTransparentDebug,
    /// Which built-in shading family this transparent pipeline emits — the
    /// fragment selects its body at compile time on `base == ShadingBase::X`
    /// (specialize-only; no runtime `shader_id ==` uber branch).
    pub base: ShadingBase,
    /// Per-mesh dynamic-material shader_id (`u32`), `0` when not a custom
    /// material. Only meaningful when `base == Custom`; retained for the
    /// wrapper's debug labelling.
    pub shader_id_dynamic: u32,
    /// For dynamic transparent shaders: the auto-generated
    /// `struct MaterialData { ... }` declaration. Empty when not in use.
    pub dynamic_struct_decl: String,
    /// For dynamic transparent shaders: the auto-generated
    /// `fn material_data_load(byte_offset: u32) -> MaterialData`
    /// accessor. Empty when not in use.
    pub dynamic_loader_decl: String,
    /// For dynamic transparent shaders: the author's WGSL fragment
    /// (wrapped at template render time into
    /// `fn custom_shade_transparent_dynamic(input) -> TransparentShadingOutput`).
    pub dynamic_wgsl_fragment: String,
}

impl ShaderTemplateTransparentMaterialFragment {
    /// Creates a fragment shader template from the cache key.
    pub fn new(cache_key: &ShaderCacheKeyMaterialTransparent) -> Self {
        let uv_sets = cache_key.attributes.uv_sets.unwrap_or_default();
        let color_sets = cache_key.attributes.color_sets.unwrap_or_default();
        // after world_tangent (loc 2) + instance_id (loc 3)
        let in_color_set_start = 4;
        let in_uv_set_start = in_color_set_start + color_sets;

        Self {
            uv_sets,
            color_sets,
            in_uv_set_start,
            in_color_set_start,
            texture_pool_arrays_len: cache_key.texture_pool_arrays_len,
            texture_pool_samplers_len: cache_key.texture_pool_samplers_len,
            debug: ShaderTemplateMaterialTransparentDebug::new(),
            base: cache_key.base,
            shader_id_dynamic: cache_key
                .dynamic_shader_id
                .map(|id| id.as_u32())
                .unwrap_or(0),
            dynamic_struct_decl: cache_key
                .dynamic_shader
                .as_ref()
                .map(|d| d.struct_decl.clone())
                .unwrap_or_default(),
            dynamic_loader_decl: cache_key
                .dynamic_shader
                .as_ref()
                .map(|d| d.loader_decl.clone())
                .unwrap_or_default(),
            dynamic_wgsl_fragment: cache_key
                .dynamic_shader
                .as_ref()
                .map(|d| d.wgsl_fragment.clone())
                .unwrap_or_default(),
        }
    }

    /// Returns true if the shader includes IBL lighting.
    pub fn has_lighting_ibl(&self) -> bool {
        match self.debug.lighting {
            ShaderTemplateMaterialTransparentDebugLighting::None => true,
            ShaderTemplateMaterialTransparentDebugLighting::IblOnly => true,
            ShaderTemplateMaterialTransparentDebugLighting::PunctualOnly => false,
        }
    }

    /// Returns true if the shader includes punctual lighting.
    pub fn has_lighting_punctual(&self) -> bool {
        match self.debug.lighting {
            ShaderTemplateMaterialTransparentDebugLighting::None => true,
            ShaderTemplateMaterialTransparentDebugLighting::IblOnly => false,
            ShaderTemplateMaterialTransparentDebugLighting::PunctualOnly => true,
        }
    }
}

impl TryFrom<&ShaderCacheKeyMaterialTransparent> for ShaderTemplateMaterialTransparent {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyMaterialTransparent) -> Result<Self> {
        Ok(Self {
            includes: ShaderTemplateTransparentMaterialIncludes::new(value),
            bind_groups: ShaderTemplateTransparentMaterialBindGroups::new(value),
            vertex: ShaderTemplateTransparentMaterialVertex::new(value),
            fragment: ShaderTemplateTransparentMaterialFragment::new(value),
        })
    }
}

/// Debug flags for transparent materials.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShaderTemplateMaterialTransparentDebug {
    lighting: ShaderTemplateMaterialTransparentDebugLighting,
}

impl ShaderTemplateMaterialTransparentDebug {
    /// Creates a default debug configuration.
    pub fn new() -> Self {
        Self {
            ..Default::default()
        }
    }
    /// Returns true if any debug mode is enabled.
    pub fn any(&self) -> bool {
        !matches!(
            self.lighting,
            ShaderTemplateMaterialTransparentDebugLighting::None
        )
    }
}

/// Lighting debug override for transparent materials.
#[derive(Clone, Copy, Debug, Default)]
pub enum ShaderTemplateMaterialTransparentDebugLighting {
    #[default]
    None,
    IblOnly,
    PunctualOnly,
}

impl ShaderTemplateMaterialTransparent {
    /// Renders the transparent material shader into WGSL.
    pub fn into_source(self) -> Result<String> {
        let includes_source = self.includes.render()?;
        let bind_groups_source = self.bind_groups.render()?;
        let vertex_source = self.vertex.render()?;
        let fragment_source = self.fragment.render()?;

        // print_shader_source(&includes_source, true);

        // debug_unique_string(1, &vertex_source, || {
        //     print_shader_source(&vertex_source, false)
        // });

        Ok(format!(
            "{}\n{}\n{}\n{}",
            includes_source, bind_groups_source, vertex_source, fragment_source
        ))
    }

    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Material Transparent")
    }
}
