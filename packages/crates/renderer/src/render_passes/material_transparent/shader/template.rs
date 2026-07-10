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
    /// `awsm_renderer_materials::registry::build_materials_wgsl`.
    pub materials_wgsl: String,
    /// Generated `const SHADER_ID_X: u32 = N;` lines — see
    /// `awsm_renderer_materials::registry::build_shader_id_consts`.
    pub shader_id_consts: String,
    /// PBR feature mask for the shared `brdf.wgsl` include's compile-time
    /// `{% if pbr_features.<x> %}` gating — the transparent material's exact
    /// feature-set (each transparent material compiles its own pipeline).
    pub pbr_features: awsm_renderer_materials::pbr::PbrFeatures,
    /// Skinny-materials include gating (brdf / apply_lighting) — see the opaque
    /// compute template.
    pub inc: crate::dynamic_materials::ShaderIncludeFlags,
    /// Shading family this transparent pipeline handles — gates the PBR vs unlit
    /// material-color builders in `material_color_calc.wgsl` so a thin non-PBR
    /// transparent shader (whose `materials_wgsl` only carries its own fragment)
    /// doesn't reference the other family's material struct.
    pub base: ShadingBase,
    /// Plan B (stage 5a): always `false` on the transparent pass — the forward
    /// transparent fragment samples shadow maps inline (it shades its own
    /// pixels back-to-front, with no prep buffer / no `g_prep_ctx`). Present
    /// because the shared `apply_lighting.wgsl` gates the runtime PrepReadContext
    /// select on it; `false` keeps the legacy inline-only path (byte-identical).
    pub prep_present: bool,
    /// Plan B: emit the inline `sample_shadow_*` path. Transparent always lights
    /// inline (`inc.apply_lighting`), so the cascade-selection / inline-sample
    /// arms in apply_lighting are gated on it (mirrors the opaque template).
    pub needs_shadow_sampling: bool,
    /// Inert on the transparent pass (`prep_present` is false), but the
    /// shared `apply_lighting.wgsl`'s `prep_shadow_read` references it.
    pub max_shadow_casters: u32,
    /// Always `false` on the transparent pass (forward, no edge buffer), but the
    /// shared `apply_lighting.wgsl`'s `prep_shadow_read` gates the EDGE-mode
    /// `prep_edge_shadow` read on it; the field must exist for askama type-check
    /// even though the enclosing `{% if prep_present %}` is always false here.
    pub multisampled_geometry: bool,
    /// `true` when this transparent mesh's material declared a
    /// `custom_displace_vertex` body — emits the gated hook wrapper +
    /// `apply_vertex` call. Driven by the cache key's `dynamic_vertex_shader`;
    /// `false` (every transparent mesh without a vertex body) keeps the output
    /// byte-identical to the pre-feature build.
    pub has_custom_vertex: bool,
    /// The author's WGSL displacement body, wrapped into
    /// `custom_displace_vertex` at render time. Empty unless `has_custom_vertex`.
    pub dynamic_wgsl_vertex: String,
    /// Auto-generated `struct MaterialData { ... }` decl for the hook — emitted
    /// only for a non-`Custom` base (a `Custom` transparent material's FRAGMENT
    /// template already declares it; re-emitting would redefine the type). Empty
    /// unless `has_custom_vertex`.
    pub dynamic_vertex_struct_decl: String,
    /// Auto-generated `material_data_load` accessor for the hook (same non-Custom
    /// gating as the struct decl). Empty unless `has_custom_vertex`.
    pub dynamic_vertex_loader_decl: String,
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
            materials_wgsl: awsm_renderer_materials::registry::build_materials_wgsl_filtered(
                cache_key.base.canonical_shader_id(),
            ),
            shader_id_consts: awsm_renderer_materials::registry::build_shader_id_consts(),
            // Per-material specialization: the shared brdf /
            // material_color_calc includes gate on exactly this transparent
            // material's feature-set (no uber all()).
            pbr_features: awsm_renderer_materials::pbr::PbrFeatures::from_bits(
                cache_key.pbr_features,
            ),
            // `for_custom` forces the Tier-B PBR-internal flags off — a custom
            // material can never enable brdf/apply_lighting/material_color_calc
            // on the transparent path either (Phase 3 item 2; parity with opaque).
            inc: if let Some(d) = cache_key.dynamic_shader.as_ref() {
                crate::dynamic_materials::ShaderIncludeFlags::for_custom(d.shader_includes)
            } else {
                crate::dynamic_materials::ShaderIncludeFlags::for_base(cache_key.base)
            },
            base: cache_key.base,
            // Transparent keeps inline shadow sampling (forward pass, own
            // pixels, no prep buffer) — never reads the prep shadow buffer.
            prep_present: false,
            // Inline shadow sampling is emitted whenever this material runs
            // first-party lighting (the only `sample_shadow_*` caller).
            needs_shadow_sampling: if let Some(d) = cache_key.dynamic_shader.as_ref() {
                crate::dynamic_materials::ShaderIncludeFlags::for_custom(d.shader_includes)
                    .apply_lighting
            } else {
                crate::dynamic_materials::ShaderIncludeFlags::for_base(cache_key.base)
                    .apply_lighting
            },
            max_shadow_casters: 4,
            // Transparent is a forward pass with no edge buffer; inert (the EDGE
            // branch lives under the always-false `prep_present` gate).
            multisampled_geometry: false,
            // Custom-vertex hook (opt-in): only emitted when the mesh's material
            // declared a `wgsl_vertex` body. `None` keeps the gated WGSL out, so
            // a transparent mesh without a vertex body is byte-identical to today.
            has_custom_vertex: cache_key.dynamic_vertex_shader.is_some(),
            dynamic_wgsl_vertex: cache_key
                .dynamic_vertex_shader
                .as_ref()
                .map(|d| d.wgsl_vertex.clone())
                .unwrap_or_default(),
            dynamic_vertex_struct_decl: cache_key
                .dynamic_vertex_shader
                .as_ref()
                .map(|d| d.struct_decl.clone())
                .unwrap_or_default(),
            dynamic_vertex_loader_decl: cache_key
                .dynamic_vertex_shader
                .as_ref()
                .map(|d| d.loader_decl.clone())
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
    /// include. `lights` is folded into `main` (group 0) so the
    /// shadow group lives at slot 1 on the transparent pipeline.
    pub shadow_group_index: u32,
    /// SSCS is opaque-only — the transparent pass doesn't have access
    /// to a depth texture it can sample on the same frame without a
    /// feedback loop. `false` here makes `apply_sscs` short-circuit.
    pub sscs_available: bool,
    /// SSCS loop bound — inert on the transparent pass (`sscs_available` is
    /// always `false` so `apply_sscs`'s body is never emitted), but the shared
    /// shadow template references the field, so it must exist. Dummy `1`.
    pub sscs_step_count: u32,
    /// Emit the shadow SAMPLING block only when this material runs
    /// first-party lighting (`inc.apply_lighting`) — the only caller of
    /// `sample_shadow_*`. Custom materials force it off. The shadow bind
    /// group + structs stay (ABI). Parity with the opaque path.
    pub needs_shadow_sampling: bool,
    /// Emit the cascade-debug overlay (`debug_cascade_tint`). Same
    /// `inc.apply_lighting` gate as `needs_shadow_sampling` here (transparent
    /// always inline-samples when it lights); a separate flag because the
    /// opaque path drops the sampler block but keeps the overlay.
    pub needs_cascade_debug: bool,
    /// Depth convention (003) — read by the shared SSCS body in
    /// `shared_wgsl/shadow/bind_groups.wgsl` (inert here: `sscs_available`
    /// is always `false` on the transparent pass, but the shared template
    /// references the field, so it must exist).
    pub reverse_z: bool,
}

impl ShaderTemplateTransparentMaterialBindGroups {
    /// Creates a bind group template from the cache key.
    pub fn new(cache_key: &ShaderCacheKeyMaterialTransparent) -> Self {
        // Same include resolution as the main transparent template, so the
        // shadow-sampling gate matches `apply_lighting`'s presence.
        let inc = if let Some(d) = cache_key.dynamic_shader.as_ref() {
            crate::dynamic_materials::ShaderIncludeFlags::for_custom(d.shader_includes)
        } else {
            crate::dynamic_materials::ShaderIncludeFlags::for_base(cache_key.base)
        };
        Self {
            texture_pool_arrays_len: cache_key.texture_pool_arrays_len,
            texture_pool_samplers_len: cache_key.texture_pool_samplers_len,
            multisampled_geometry: cache_key.msaa_sample_count.is_some(),
            shadow_group_index: 1,
            sscs_available: false,
            sscs_step_count: 1,
            needs_shadow_sampling: inc.apply_lighting,
            needs_cascade_debug: inc.apply_lighting,
            reverse_z: cache_key.reverse_z,
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
    /// Gates the extra `apply_vertex` hook args (real per-mesh uv0 /
    /// instance_id / frame_globals) in the vertex entry. `true` only when this
    /// mesh's material declared a `custom_displace_vertex` body; `false` keeps
    /// the vertex entry byte-identical to the pre-feature build.
    pub has_custom_vertex: bool,
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
            // Gates the extra `apply_vertex` hook args (uv0 / instance_id /
            // frame_globals) in the vertex entry; on only when this mesh's
            // material declared a `custom_displace_vertex` body.
            has_custom_vertex: cache_key.dynamic_vertex_shader.is_some(),
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
    /// Gate for the runtime debug VIEW overlays in the shared
    /// `apply_lighting.wgsl` (unlit/flat view mode + froxel light-count
    /// heatmap). Follows the `debug-views` cargo feature; `false` for game
    /// builds collapses the `{% if debug.views %}` gates. `pub` to mirror the
    /// opaque struct (read by the shared include).
    pub views: bool,
}

impl ShaderTemplateMaterialTransparentDebug {
    /// Creates a default debug configuration. The view-overlay gate follows the
    /// `debug-views` cargo feature (off for game builds, on for the editor).
    pub fn new() -> Self {
        Self {
            views: cfg!(feature = "debug-views"),
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
