//! Shader template for the material prep compute pass (Plan B). Renders bind
//! groups + compute into one WGSL string. Mirrors the other render-pass templates.

use askama::Template;

use crate::{
    render_passes::material_prep::shader::cache_key::{
        ShaderCacheKeyMaterialPrep, ShaderCacheKeyShadowBlur,
    },
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
    /// SSCS effective gate = pass-capability (prep binds `depth_tex` +
    /// `camera_raw`) AND the global `ShadowsConfig::sscs_enabled`. When `false`
    /// the shared `apply_sscs` body is compiled out to `return 1.0` (zero cost).
    pub sscs_available: bool,
    /// SSCS ray-march step count baked as the `apply_sscs` loop bound
    /// (compile-time constant). Only read when `sscs_available`.
    pub sscs_step_count: u32,
    /// Bind-group slot the shadow bindings live at (group 2 for prep).
    pub shadow_group_index: u32,
    /// Z-slice count for `froxel_walk.wgsl` (`FROXEL_SLICE_COUNT`).
    pub froxel_slice_count: u32,
    /// Depth convention (003) — read by the shared SSCS body in
    /// `shared_wgsl/shadow/bind_groups.wgsl`.
    pub reverse_z: bool,
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
                // Prep is SSCS-capable (binds depth_tex + camera_raw); the
                // effective gate is the global enable. `sscs_step_count` is
                // clamped ≥1 so the loop bound and `f32(steps)` divisor are safe.
                sscs_available: key.sscs_enabled,
                sscs_step_count: key.sscs_step_count.max(1),
                shadow_group_index: 2,
                froxel_slice_count,
                reverse_z: key.reverse_z,
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

// ── Optional shadow-visibility denoise blur (`cs_blur_h` / `cs_blur_v`) ───────

pub struct ShaderTemplateShadowBlur {
    pub bind_groups: ShaderTemplateShadowBlurBindGroups,
    pub compute: ShaderTemplateShadowBlurCompute,
}

/// Blur bind-group declarations — lockstep with the blur layout in
/// `material_prep/bind_group.rs` (`create_blur_bind_group_layout_key`).
#[derive(Template, Debug)]
#[template(path = "shadow_blur_wgsl/bind_groups.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateShadowBlurBindGroups {
    /// Depth binding type (true = `texture_depth_multisampled_2d`).
    pub multisampled_geometry: bool,
}

/// Blur compute body (`cs_blur_h` / `cs_blur_v`).
#[derive(Template, Debug)]
#[template(path = "shadow_blur_wgsl/compute.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateShadowBlurCompute {
    /// `ceil(K/4)` — packed shadow-visibility layers to blur (matches the prep
    /// output this pass reads + writes).
    pub shadow_visibility_layers: u32,
}

impl TryFrom<&ShaderCacheKeyShadowBlur> for ShaderTemplateShadowBlur {
    type Error = AwsmShaderError;
    fn try_from(key: &ShaderCacheKeyShadowBlur) -> Result<Self> {
        Ok(ShaderTemplateShadowBlur {
            bind_groups: ShaderTemplateShadowBlurBindGroups {
                multisampled_geometry: key.msaa_sample_count.is_some(),
            },
            compute: ShaderTemplateShadowBlurCompute {
                shadow_visibility_layers: key.max_shadow_casters.div_ceil(4).max(1),
            },
        })
    }
}

impl ShaderTemplateShadowBlur {
    /// Renders the blur shader into a WGSL source string.
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = self.bind_groups.render()?;
        let compute_source = self.compute.render()?;
        Ok(format!("{}\n{}", bind_groups_source, compute_source))
    }

    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Shadow Denoise Blur")
    }
}
