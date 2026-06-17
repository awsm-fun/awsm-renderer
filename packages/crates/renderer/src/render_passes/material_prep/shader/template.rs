//! Shader template for the material prep compute pass (Plan B). Renders bind
//! groups + compute into one WGSL string. Mirrors the other render-pass templates.

use askama::Template;

use crate::{
    render_passes::material_prep::shader::cache_key::ShaderCacheKeyMaterialPrep,
    shaders::{AwsmShaderError, Result},
};

pub struct ShaderTemplateMaterialPrep {
    pub bind_groups: ShaderTemplateMaterialPrepBindGroups,
    pub compute: ShaderTemplateMaterialPrepCompute,
}

/// Bind group declarations — must stay in lockstep with
/// `material_prep/bind_group.rs` (added in the buffer-wiring sub-stage).
#[derive(Template, Debug)]
#[template(path = "material_prep_wgsl/bind_groups.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateMaterialPrepBindGroups {
    /// Visibility texture sample count (true = multisampled binding type).
    pub multisampled_geometry: bool,
    /// Stage 3b: emit the shadow-feature bindings + lighting/shadow includes.
    /// Always `true` for the prep variant (prep is only built when enabled, and
    /// it always computes shadow visibility); the gate keeps the prep-disabled /
    /// no-shadow source byte-identical to the pre-3b scaffold.
    pub shadows: bool,
    /// Compile the shadow-SAMPLING bodies (`{% if needs_shadow_sampling %}` in
    /// `shadow/bind_groups.wgsl`). `true` — prep is a shadow sampler.
    pub needs_shadow_sampling: bool,
    /// SSCS availability (`apply_sscs` reads `depth_tex` + `camera_raw`). Prep
    /// binds both, so `true` (matches the opaque pass for parity).
    pub sscs_available: bool,
    /// Bind-group slot the shadow bindings live at (group 2 for prep).
    pub shadow_group_index: u32,
    /// Z-slice count for `froxel_walk.wgsl` (`FROXEL_SLICE_COUNT`).
    pub froxel_slice_count: u32,
}

/// Compute body (`cs_prep`).
#[derive(Template, Debug)]
#[template(path = "material_prep_wgsl/compute.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateMaterialPrepCompute {
    pub multisampled_geometry: bool,
    /// UV / color set layer caps — kept in lockstep with the array-texture
    /// allocation (`render_textures.rs`) so the write loop never exceeds the
    /// texture's layer count.
    pub max_prep_uv_sets: u32,
    pub max_prep_color_sets: u32,
    /// Stage 3b: emit the shadow-loop body in `cs_prep`. See the BindGroups
    /// `shadows` field — always `true` for the prep variant.
    pub shadows: bool,
    /// `K` — the clamped per-pixel shadow-caster cap. The loop stops storing
    /// once `slot >= K` (and the packed-layer count is `ceil(K/4)`).
    pub max_shadow_casters: u32,
    /// `ceil(K/4)` — the packed shadow-visibility layer count; the shared
    /// `compute_shadow_visibility_packed` returns this many vec4 layers.
    pub shadow_visibility_layers: u32,
    /// MSAA sample count (4 under MSAA, 0 otherwise). cs_prep_edge loops
    /// `0..msaa_sample_count` (only emitted on the MSAA module).
    pub msaa_sample_count: u32,
    /// Fixed width of the compact edge-shadow texture (Stage 5b-shadow). The
    /// flat edge-sample index maps to `(idx % W, idx / W)`. Must match the
    /// Rust-side allocation (`material_prep::buffers::EDGE_SHADOW_TEX_WIDTH`).
    pub edge_shadow_tex_width: u32,
}

impl TryFrom<&ShaderCacheKeyMaterialPrep> for ShaderTemplateMaterialPrep {
    type Error = AwsmShaderError;
    fn try_from(key: &ShaderCacheKeyMaterialPrep) -> Result<Self> {
        let multisampled_geometry = key.msaa_sample_count.is_some();
        let froxel_slice_count = crate::render_passes::light_culling::DEFAULT_SLICE_COUNT;
        Ok(ShaderTemplateMaterialPrep {
            bind_groups: ShaderTemplateMaterialPrepBindGroups {
                multisampled_geometry,
                shadows: true,
                needs_shadow_sampling: true,
                sscs_available: true,
                shadow_group_index: 2,
                froxel_slice_count,
            },
            compute: ShaderTemplateMaterialPrepCompute {
                multisampled_geometry,
                max_prep_uv_sets: crate::render_passes::material_prep::MAX_PREP_UV_SETS,
                max_prep_color_sets: crate::render_passes::material_prep::MAX_PREP_COLOR_SETS,
                shadows: true,
                max_shadow_casters: key.max_shadow_casters,
                shadow_visibility_layers: key.max_shadow_casters.div_ceil(4).max(1),
                msaa_sample_count: key.msaa_sample_count.unwrap_or(0),
                edge_shadow_tex_width:
                    crate::render_passes::material_prep::buffers::EDGE_SHADOW_TEX_WIDTH,
            },
        })
    }
}

impl ShaderTemplateMaterialPrep {
    /// Renders the prep shader into a WGSL source string.
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = self.bind_groups.render()?;
        let compute_source = self.compute.render()?;
        Ok(format!("{}\n{}", bind_groups_source, compute_source))
    }

    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Material Prep")
    }
}
