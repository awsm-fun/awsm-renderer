//! Shader templates for the opaque material pass.

use askama::Template;
use awsm_materials::MaterialShaderId;

use crate::{
    render_passes::material_opaque::shader::cache_key::{
        ShaderCacheKeyMaterialOpaque, ShaderCacheKeyMaterialOpaqueEmpty,
    },
    shaders::{AwsmShaderError, Result},
};

/// Opaque material shader template components.
#[derive(Debug)]
pub struct ShaderTemplateMaterialOpaque {
    pub bind_groups: ShaderTemplateMaterialOpaqueBindGroups,
    pub compute: ShaderTemplateMaterialOpaqueCompute,
}

/// Bind group template for the opaque material pass.
#[derive(Template, Debug)]
#[template(
    path = "material_opaque_wgsl/bind_groups.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateMaterialOpaqueBindGroups {
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub debug: ShaderTemplateMaterialOpaqueDebug,
    pub mipmap: MipmapMode,
    pub multisampled_geometry: bool,
    pub msaa_sample_count: u32, // 0 if no MSAA
    /// Bind-group slot index the shadow declarations should occupy.
    /// Opaque pipelines use slot 3; the field exists so the same
    /// `shared_wgsl/shadow/bind_groups.wgsl` include can be reused by
    /// the transparent pipeline (slot 1 as of 16.B).
    pub shadow_group_index: u32,
    /// Whether `apply_sscs` should compile its real body (true on the
    /// opaque pass — it has `depth_tex` bound) or short-circuit to
    /// `return 1.0` (true on the transparent pass — sampling its own
    /// depth target would be a feedback loop, so SSCS is disabled).
    pub sscs_available: bool,
}

/// Compute shader template for the opaque material pass.
#[derive(Template, Debug)]
#[template(path = "material_opaque_wgsl/compute.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateMaterialOpaqueCompute {
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub debug: ShaderTemplateMaterialOpaqueDebug,
    pub mipmap: MipmapMode,
    pub multisampled_geometry: bool,
    pub msaa_sample_count: u32, // 0 if no MSAA
    /// Whether to wire shadow sampling into `apply_lighting`. Opaque
    /// is always `true` once Phase 2 lands; the empty / transparent
    /// templates leave it `false` until they pull in the shadow
    /// bind-group declarations themselves.
    pub shadows_enabled: bool,
    /// Cluster 2.1.c: switch the punctual-light walk to the per-mesh
    /// slice fed by `mesh_light_slices` + `mesh_light_indices`. Opaque
    /// is true; transparent stays false (plan §12 Q8 default).
    pub use_mesh_light_slices: bool,
    /// Concatenated `wgsl_fragment()` of every enabled material — see
    /// `awsm_materials::registry::build_materials_wgsl`.
    pub materials_wgsl: String,
    /// Generated `const SHADER_ID_X: u32 = N;` lines — see
    /// `awsm_materials::registry::build_shader_id_consts`.
    pub shader_id_consts: String,
    /// Which material shader_id this specialized pipeline handles.
    /// The compute.wgsl template renders only the matching material's
    /// shading code (PBR / Unlit / Toon), with a per-pixel guard
    /// early-returning on mismatch so a full-screen dispatch is
    /// correct even before classify+indirect lands.
    pub shader_id: MaterialShaderId,
}

impl ShaderTemplateMaterialOpaqueCompute {
    /// Returns true if the shader includes IBL lighting.
    pub fn has_lighting_ibl(&self) -> bool {
        match self.debug.lighting {
            ShaderTemplateMaterialOpaqueDebugLighting::None => true,
            ShaderTemplateMaterialOpaqueDebugLighting::IblOnly => true,
            ShaderTemplateMaterialOpaqueDebugLighting::PunctualOnly => false,
        }
    }

    /// Returns true if the shader includes punctual lighting.
    pub fn has_lighting_punctual(&self) -> bool {
        match self.debug.lighting {
            ShaderTemplateMaterialOpaqueDebugLighting::None => true,
            ShaderTemplateMaterialOpaqueDebugLighting::IblOnly => false,
            ShaderTemplateMaterialOpaqueDebugLighting::PunctualOnly => true,
        }
    }
}

impl TryFrom<&ShaderCacheKeyMaterialOpaque> for ShaderTemplateMaterialOpaque {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyMaterialOpaque) -> Result<Self> {
        let texture_pool_arrays_len = value.texture_pool_arrays_len;
        let texture_pool_samplers_len = value.texture_pool_samplers_len;
        let mipmap = if value.mipmaps {
            MipmapMode::Gradient
        } else {
            MipmapMode::None
        };
        let multisampled_geometry = value.msaa_sample_count.is_some();
        let msaa_sample_count = value.msaa_sample_count.unwrap_or_default();
        let debug = ShaderTemplateMaterialOpaqueDebug::new();

        let _self = Self {
            bind_groups: ShaderTemplateMaterialOpaqueBindGroups {
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                mipmap,
                multisampled_geometry,
                msaa_sample_count,
                debug,
                shadow_group_index: 3,
                sscs_available: true,
            },
            compute: ShaderTemplateMaterialOpaqueCompute {
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                mipmap,
                multisampled_geometry,
                msaa_sample_count,
                debug,
                shadows_enabled: true,
                use_mesh_light_slices: true,
                materials_wgsl: awsm_materials::registry::build_materials_wgsl(),
                shader_id_consts: awsm_materials::registry::build_shader_id_consts(),
                shader_id: value.shader_id,
            },
        };

        Ok(_self)
    }
}

/// Mipmap sampling mode for the material opaque pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MipmapMode {
    None,
    Gradient,
}

impl MipmapMode {
    /// Returns the function name suffix for this mipmap mode
    pub fn suffix(&self) -> &'static str {
        match self {
            MipmapMode::Gradient => "_grad",
            MipmapMode::None => "_no_mips",
        }
    }

    /// Returns the texture sampling function name for this mode
    pub fn sample_fn(&self) -> &'static str {
        match self {
            MipmapMode::Gradient => "texture_pool_sample_grad",
            MipmapMode::None => "texture_pool_sample_no_mips",
        }
    }

    /// Returns true if this is gradient mode (for conditional template logic)
    pub fn is_gradient(&self) -> bool {
        matches!(self, MipmapMode::Gradient)
    }
}

/// Debug flags for the opaque material pass.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShaderTemplateMaterialOpaqueDebug {
    mips: bool,
    n_dot_v: bool,
    normals: bool,
    base_color: bool,
    view_direction: bool,
    irradiance_sample: bool,
    msaa_detect_edges: bool,
    lighting: ShaderTemplateMaterialOpaqueDebugLighting,
}

impl ShaderTemplateMaterialOpaqueDebug {
    /// Creates a default debug configuration.
    pub fn new() -> Self {
        Self { ..Self::default() }
    }
    /// Returns true if any debug mode is enabled.
    pub fn any(&self) -> bool {
        self.mips
            || self.n_dot_v
            || self.normals
            || self.base_color
            || self.view_direction
            || self.irradiance_sample
            || self.msaa_detect_edges
            || !matches!(
                self.lighting,
                ShaderTemplateMaterialOpaqueDebugLighting::None
            )
    }
}

/// Lighting debug override for opaque materials.
#[derive(Clone, Copy, Debug, Default)]
pub enum ShaderTemplateMaterialOpaqueDebugLighting {
    #[default]
    None,
    IblOnly,
    PunctualOnly,
}

impl ShaderTemplateMaterialOpaque {
    /// Renders the opaque material shader into WGSL.
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = self.bind_groups.render()?;
        let compute_source = self.compute.render()?;

        let source = format!("{}\n{}", bind_groups_source, compute_source);
        // print_shader_source(&source, true);

        //debug_unique_string(1, &source, || print_shader_source(&source, false));

        Ok(source)
    }

    #[cfg(debug_assertions)]
    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Material Opaque")
    }
}

impl TryFrom<&ShaderCacheKeyMaterialOpaqueEmpty> for ShaderTemplateMaterialOpaqueEmpty {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyMaterialOpaqueEmpty) -> Result<Self> {
        Ok(Self {
            texture_pool_arrays_len: value.texture_pool_arrays_len,
            texture_pool_samplers_len: value.texture_pool_samplers_len,
            multisampled_geometry: value.msaa_sample_count.is_some(),
            unlit: true,
            shadow_group_index: 3,
            shadows_enabled: false,
            sscs_available: false,
            use_mesh_light_slices: false,
            materials_wgsl: awsm_materials::registry::build_materials_wgsl(),
            shader_id_consts: awsm_materials::registry::build_shader_id_consts(),
        })
    }
}

/// Empty shader template used when no opaque geometry is present.
#[derive(Template, Debug)]
#[template(path = "material_opaque_wgsl/empty.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateMaterialOpaqueEmpty {
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub multisampled_geometry: bool,
    pub unlit: bool,
    /// Bind-group slot index the shadow declarations should occupy.
    pub shadow_group_index: u32,
    /// Mirror of the opaque-compute flag. The empty template has no
    /// real geometry so shadow sampling is irrelevant; left `false`
    /// to keep the WGSL minimal.
    pub shadows_enabled: bool,
    /// Mirror of the opaque-compute flag. The empty template never
    /// runs SSCS, but the shared shadow include needs the symbol.
    pub sscs_available: bool,
    /// Mirror of the opaque-compute flag. The empty template never
    /// touches the per-mesh slice path but the shared lights include
    /// needs the symbol in scope.
    pub use_mesh_light_slices: bool,
    /// Concatenated `wgsl_fragment()` of every enabled material — see
    /// `awsm_materials::registry::build_materials_wgsl`.
    pub materials_wgsl: String,
    /// Generated `const SHADER_ID_X: u32 = N;` lines — see
    /// `awsm_materials::registry::build_shader_id_consts`.
    pub shader_id_consts: String,
}

impl ShaderTemplateMaterialOpaqueEmpty {
    /// Renders the empty opaque shader into WGSL.
    pub fn into_source(self) -> Result<String> {
        let source = self.render()?;
        // print_shader_source(&source, true);

        //debug_unique_string(1, &source, || print_shader_source(&source, false));

        Ok(source)
    }

    #[cfg(debug_assertions)]
    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Material Opaque Empty")
    }

    /// Returns true if the shader includes IBL lighting.
    pub fn has_lighting_ibl(&self) -> bool {
        false
    }

    /// Returns true if the shader includes punctual lighting.
    pub fn has_lighting_punctual(&self) -> bool {
        false
    }
}
