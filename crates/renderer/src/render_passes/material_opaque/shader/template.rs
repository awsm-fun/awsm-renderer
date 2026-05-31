//! Shader templates for the opaque material pass.

use askama::Template;
use awsm_materials::MaterialShaderId;

use crate::dynamic_materials::ShadingBase;
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
    /// Registry bucket list — drives the templated `ClassifyBuckets`
    /// struct emit, must match the classify-pass writer's struct
    /// byte-for-byte.
    pub bucket_entries: Vec<crate::dynamic_materials::BucketEntry>,
    /// Trailing alignment-pad u32 indices for `ClassifyBuckets`,
    /// mirrors the classify-pass template's `pad_words_iter`.
    pub pad_words_iter: Vec<u32>,
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
    /// is always `true`; the empty / transparent templates leave it
    /// `false` until they pull in the shadow bind-group declarations
    /// themselves.
    pub shadows_enabled: bool,
    /// Switch the punctual-light walk to the per-mesh slice fed by
    /// `mesh_light_slices` + `mesh_light_indices`. Opaque is true;
    /// transparent stays false.
    /// When `true`, the shared `lights.wgsl` emits the
    /// `apply_lighting_per_froxel*` helpers. Opaque sets this `true` —
    /// all opaque shading reads the per-pixel froxel light list produced
    /// by the GPU cull pass (see `compute.wgsl` and docs/PERFORMANCE.md
    /// §5h).
    pub use_froxel_lights: bool,
    /// Number of view-space Z slices in the cull grid. Constant-
    /// folded into the per-pixel froxel-index calc that the gated
    /// `apply_lighting_per_froxel*` helpers contain.
    pub froxel_slice_count: u32,
    /// Concatenated `wgsl_fragment()` of every enabled material — see
    /// `awsm_materials::registry::build_materials_wgsl`.
    pub materials_wgsl: String,
    /// Generated `const SHADER_ID_X: u32 = N;` lines — see
    /// `awsm_materials::registry::build_shader_id_consts`.
    pub shader_id_consts: String,
    /// Which material shader_id this specialized pipeline handles.
    /// The compute.wgsl template renders only the matching material's
    /// shading code (PBR / Unlit / Toon / FlipBook / a registered
    /// dynamic material), with a per-pixel guard early-returning on
    /// mismatch so a full-screen dispatch is correct even before
    /// classify+indirect lands.
    pub shader_id: MaterialShaderId,
    /// Which built-in shading family's body this pipeline emits
    /// (`{% if base == ShadingBase::Pbr %}` etc.). Decoupled from
    /// `shader_id` so a per-feature-set PBR variant whose id is in the
    /// dynamic range still emits the PBR path. The per-pixel guard uses
    /// the numeric `shader_id` regardless of `base`.
    pub base: crate::dynamic_materials::ShadingBase,
    /// Whether this pipeline owns the skybox write (only the canonical
    /// PBR bucket; see [`ShaderCacheKeyMaterialOpaque::owns_skybox`]).
    pub owns_skybox: bool,
    /// PBR feature set this specialized pipeline is compiled for. The
    /// compute template + `material_color_calc.wgsl` gate per-feature code
    /// behind `{% if pbr_features.<x> %}`, so an unused feature (no
    /// clearcoat in the scene, etc.) emits no code.
    /// The empty set for non-PBR ids and the canonical PBR (skybox-owner)
    /// bucket — inert for the former (their body doesn't read it) and the
    /// minimal shader for the latter. Never the full "uber" set.
    pub pbr_features: awsm_materials::pbr::PbrFeatures,
    /// For dynamic shader ids: the auto-generated `struct
    /// MaterialData { ... }` declaration emitted above the author's
    /// WGSL fragment. Empty string for first-party ids.
    pub dynamic_struct_decl: String,
    /// For dynamic shader ids: the auto-generated
    /// `fn material_data_load(byte_offset: u32) -> MaterialData`
    /// accessor. Empty string for first-party ids.
    pub dynamic_loader_decl: String,
    /// For dynamic shader ids: the author's WGSL fragment (wrapped at
    /// template render time into
    /// `fn custom_shade_<id>(...) -> OpaqueShadingOutput { <body> }`).
    /// Empty string for first-party ids.
    pub dynamic_wgsl_fragment: String,
    /// Registry bucket list — used by the per-shader-id
    /// `bucket_offset` lookup chain so dynamic shader_ids can resolve
    /// their `classify_buckets.<name>_offset` field. Mirrors the
    /// classify-pass template's `bucket_entries`.
    pub bucket_entries: Vec<crate::dynamic_materials::BucketEntry>,
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

        let bucket_entries = value.bucket_entries.clone();
        let pad_words_iter: Vec<u32> = (0
            ..crate::render_passes::material_classify::shader::template::pad_words_count(
                bucket_entries.len() as u32,
            ))
            .collect();
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
                bucket_entries: bucket_entries.clone(),
                pad_words_iter,
            },
            compute: ShaderTemplateMaterialOpaqueCompute {
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                mipmap,
                multisampled_geometry,
                msaa_sample_count,
                debug,
                shadows_enabled: true,
                // All opaque shading reads the per-pixel froxel light
                // list from the GPU cull pass; `lights.wgsl` emits the
                // `apply_lighting_per_froxel*` helpers when this is `true`.
                use_froxel_lights: true,
                froxel_slice_count: crate::render_passes::light_culling::DEFAULT_SLICE_COUNT,
                materials_wgsl: awsm_materials::registry::build_materials_wgsl(),
                shader_id_consts: awsm_materials::registry::build_shader_id_consts(),
                shader_id: value.shader_id,
                base: value.base,
                owns_skybox: value.owns_skybox,
                pbr_features: awsm_materials::pbr::PbrFeatures::from_bits(value.pbr_features),
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
                bucket_entries,
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

    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Material Opaque")
    }
}

impl TryFrom<&ShaderCacheKeyMaterialOpaqueEmpty> for ShaderTemplateMaterialOpaqueEmpty {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyMaterialOpaqueEmpty) -> Result<Self> {
        // The empty variant is built at builder time only — no dynamic
        // materials exist yet, so the first-party bucket list is the
        // right value. If dynamic registrations ever needed to feed
        // the empty path, ShaderCacheKeyMaterialOpaqueEmpty would grow
        // the same bucket_entries field as ShaderCacheKeyMaterialOpaque.
        let bucket_entries = crate::dynamic_materials::first_party_bucket_entries();
        let pad_words_iter: Vec<u32> = (0
            ..crate::render_passes::material_classify::shader::template::pad_words_count(
                bucket_entries.len() as u32,
            ))
            .collect();
        Ok(Self {
            texture_pool_arrays_len: value.texture_pool_arrays_len,
            texture_pool_samplers_len: value.texture_pool_samplers_len,
            multisampled_geometry: value.msaa_sample_count.is_some(),
            unlit: true,
            shadow_group_index: 3,
            shadows_enabled: false,
            sscs_available: false,
            use_froxel_lights: false,
            froxel_slice_count: crate::render_passes::light_culling::DEFAULT_SLICE_COUNT,
            materials_wgsl: awsm_materials::registry::build_materials_wgsl(),
            shader_id_consts: awsm_materials::registry::build_shader_id_consts(),
            bucket_entries,
            pad_words_iter,
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
    /// Mirror of the opaque-compute flag. The empty template doesn't
    /// emit the per-froxel walk either, but the shared `lights.wgsl`
    /// references the symbol so it must be declared.
    pub use_froxel_lights: bool,
    /// Mirror of the opaque-compute field. Unused in the empty path
    /// (the `{% if use_froxel_lights %}` gate is closed) but askama
    /// type-checks every `{{ var }}` reference even inside a closed
    /// gate, so the field has to exist.
    pub froxel_slice_count: u32,
    /// Concatenated `wgsl_fragment()` of every enabled material — see
    /// `awsm_materials::registry::build_materials_wgsl`.
    pub materials_wgsl: String,
    /// Generated `const SHADER_ID_X: u32 = N;` lines — see
    /// `awsm_materials::registry::build_shader_id_consts`.
    pub shader_id_consts: String,
    /// Mirror of the opaque-compute field. The templated
    /// `ClassifyBuckets` struct in bind_groups.wgsl walks this list to
    /// keep its layout aligned with the classify-pass writer's struct.
    pub bucket_entries: Vec<crate::dynamic_materials::BucketEntry>,
    /// Mirror of the opaque-compute field — trailing alignment-pad
    /// u32 indices for the templated `ClassifyBuckets` struct.
    pub pad_words_iter: Vec<u32>,
}

impl ShaderTemplateMaterialOpaqueEmpty {
    /// Renders the empty opaque shader into WGSL.
    pub fn into_source(self) -> Result<String> {
        let source = self.render()?;
        // print_shader_source(&source, true);

        //debug_unique_string(1, &source, || print_shader_source(&source, false));

        Ok(source)
    }

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

// ─────────────────────────────────────────────────────────────────────
// Empty-registry guarantee tests
// ─────────────────────────────────────────────────────────────────────
//
// The dynamic-materials plan promises that when no dynamic materials
// are registered, first-party pipelines' compiled WGSL is
// bit-identical to the pre-feature build. The strict cross-branch
// hash diff requires checking out main and re-running the template;
// the structural guarantees below are what we can verify in-tree:
//
//  1. dispatch_hash is 0 when the registry is empty
//  2. bucket_entries collapses to the first-party list
//  3. The templated WGSL emits no `custom_shade_dynamic` wrapper
//     (the {% if shader_id.is_dynamic() %} block evaluates to empty)
//  4. The templated `ClassifyBuckets` struct has the same field
//     names as the hand-rolled version (args_pbr / args_unlit /
//     args_toon / args_flipbook + their _offset siblings).

#[cfg(test)]
mod empty_registry_tests {
    use super::*;
    use crate::render_passes::material_opaque::shader::cache_key::ShaderCacheKeyMaterialOpaque;
    use awsm_materials::MaterialShaderId;

    /// Build a first-party-only opaque cache key, render the WGSL,
    /// and return the source.
    fn render_first_party_wgsl(shader_id: MaterialShaderId, msaa: Option<u32>) -> String {
        let key = ShaderCacheKeyMaterialOpaque {
            texture_pool_arrays_len: 1,
            texture_pool_samplers_len: 1,
            msaa_sample_count: msaa,
            mipmaps: true,
            shader_id,
            base: crate::dynamic_materials::ShadingBase::for_shader_id(shader_id),
            owns_skybox: shader_id == MaterialShaderId::PBR,
            // Canonical first-party buckets carry the empty feature-set
            // (the minimal shader, never the uber `all()`).
            pbr_features: awsm_materials::pbr::PbrFeatures::default().bits(),
            dispatch_hash: 0,
            dynamic_shader: None,
            bucket_entries: crate::dynamic_materials::first_party_bucket_entries(),
        };
        let template = ShaderTemplateMaterialOpaque::try_from(&key).expect("template");
        template.into_source().expect("render")
    }

    #[test]
    fn empty_registry_emits_no_dynamic_wrapper() {
        // The `{% if shader_id.is_dynamic() %}` block in compute.wgsl
        // must evaluate to empty for every first-party shader_id, so
        // the emitted WGSL contains none of the dynamic-wrapper
        // marker tokens.
        for shader_id in [
            MaterialShaderId::PBR,
            MaterialShaderId::UNLIT,
            MaterialShaderId::TOON,
            MaterialShaderId::FLIPBOOK,
        ] {
            let wgsl = render_first_party_wgsl(shader_id, None);
            assert!(
                !wgsl.contains("custom_shade_dynamic"),
                "first-party {shader_id:?} pipeline accidentally emits `custom_shade_dynamic`"
            );
            assert!(
                !wgsl.contains("dynamic-material wrapper"),
                "first-party {shader_id:?} pipeline emits the dynamic-material wrapper block"
            );
            assert!(
                !wgsl.contains("material_data_load"),
                "first-party {shader_id:?} pipeline emits the dynamic-material loader"
            );
        }
    }

    #[test]
    fn empty_registry_preserves_classify_buckets_field_names() {
        // The templated ClassifyBuckets struct walks bucket_entries.
        // For the empty registry these are the four first-party
        // materials in registration order, so the struct emits
        // args_pbr / args_unlit / args_toon / args_flipbook plus
        // their <name>_offset siblings — same as the hand-rolled
        // pre-feature version.
        let wgsl = render_first_party_wgsl(MaterialShaderId::PBR, None);
        for expected in [
            "args_pbr",
            "args_unlit",
            "args_toon",
            "args_flipbook",
            "pbr_offset",
            "unlit_offset",
            "toon_offset",
            "flipbook_offset",
        ] {
            assert!(
                wgsl.contains(expected),
                "empty-registry ClassifyBuckets missing `{expected}`"
            );
        }
    }

    #[test]
    fn empty_registry_bucket_offset_resolves() {
        // The Phase-3 bug we fixed was `let bucket_offset =;` when
        // the lookup chain had no match. Verify the resolved
        // expression isn't empty for every first-party shader_id.
        for (shader_id, expected) in [
            (MaterialShaderId::PBR, "classify_buckets.pbr_offset"),
            (MaterialShaderId::UNLIT, "classify_buckets.unlit_offset"),
            (MaterialShaderId::TOON, "classify_buckets.toon_offset"),
            (
                MaterialShaderId::FLIPBOOK,
                "classify_buckets.flipbook_offset",
            ),
        ] {
            let wgsl = render_first_party_wgsl(shader_id, None);
            assert!(
                wgsl.contains(expected),
                "first-party {shader_id:?} pipeline's bucket_offset doesn't resolve to {expected}"
            );
            assert!(
                !wgsl.contains("let bucket_offset =;"),
                "first-party {shader_id:?} pipeline emits empty bucket_offset assignment"
            );
        }
    }

    #[test]
    fn dispatch_hash_is_zero_on_empty_registry() {
        // The dispatch_hash field on the cache key is what triggers
        // pipeline cache invalidation when registrations change.
        // For the empty registry it must be a stable 0, otherwise
        // the first-party-only build doesn't get cache hits across
        // session restarts.
        let registry = crate::dynamic_materials::DynamicMaterials::new();
        assert_eq!(registry.dispatch_hash(), 0);
    }
}

// ─────────────────────────────────────────────────────────────────────
// Transparent dynamic-shader compile verification (lives next to the
// opaque tests for convenience — both walk the dynamic-substitution
// path with different cache keys).
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod transparent_dynamic_tests {
    use crate::render_passes::material_opaque::shader::cache_key::DynamicShaderInfo;
    use crate::render_passes::material_transparent::shader::cache_key::ShaderCacheKeyMaterialTransparent;
    use crate::render_passes::material_transparent::shader::template::ShaderTemplateMaterialTransparent;
    use crate::render_passes::shared::material::cache_key::ShaderMaterialVertexAttributes;
    use awsm_materials::dynamic_layout::{
        FieldType, MaterialLayout, TextureSlotRuntime, UniformFieldRuntime,
    };
    use awsm_materials::MaterialShaderId;

    /// Verifies the transparent fragment template + the dynamic-
    /// material wrapper compose into valid WGSL. Uses a soft-glass-
    /// style author fragment that references the TransparentShadingInput
    /// contract fields (no opaque `input.coords` / `input.screen_dims`
    /// — those are opaque-only).
    #[test]
    fn transparent_dynamic_template_renders_valid_wgsl() {
        let layout = MaterialLayout {
            uniforms: vec![
                UniformFieldRuntime {
                    name: "tint".into(),
                    ty: FieldType::Color3,
                },
                UniformFieldRuntime {
                    name: "refraction_strength".into(),
                    ty: FieldType::F32,
                },
            ],
            textures: vec![TextureSlotRuntime { name: "bg".into() }],
            buffers: Vec::new(),
        };
        let struct_decl =
            awsm_materials::dynamic_layout::generate_wgsl_struct("MaterialData", &layout);
        let loader_decl = awsm_materials::dynamic_layout::generate_wgsl_loader(
            "MaterialData",
            "material_data_load",
            &layout,
        );

        // Soft-glass-style fragment — references TransparentShadingInput's
        // actual fields + input.material.<field>.
        let wgsl_fragment = r#"
let cos_theta = clamp(dot(input.world_normal, input.surface_to_camera), 0.0, 1.0);
let alpha = mix(0.85, 0.25, cos_theta);
let color = input.material.tint * (1.0 - input.material.refraction_strength);
return TransparentShadingOutput(vec4<f32>(color, alpha));
"#
        .to_string();

        let dyn_info = DynamicShaderInfo {
            struct_decl,
            loader_decl,
            wgsl_fragment,
        };
        let dyn_id = MaterialShaderId::from_dynamic_raw(MaterialShaderId::DYNAMIC_START);

        let key = ShaderCacheKeyMaterialTransparent {
            instancing_transforms: false,
            attributes: ShaderMaterialVertexAttributes::default(),
            texture_pool_arrays_len: 1,
            texture_pool_samplers_len: 1,
            msaa_sample_count: None,
            mipmaps: true,
            base: crate::dynamic_materials::ShadingBase::Custom,
            pbr_features: awsm_materials::pbr::PbrFeatures::default().bits(),
            dispatch_hash: 0,
            dynamic_shader_id: Some(dyn_id),
            dynamic_shader: Some(dyn_info),
            froxel_slice_count: crate::render_passes::light_culling::DEFAULT_SLICE_COUNT,
        };

        let template = ShaderTemplateMaterialTransparent::try_from(&key)
            .expect("transparent template must build from a valid cache key");
        let source = template
            .into_source()
            .expect("transparent template must render without errors");

        // Structural assertions on the emitted source. The specialize-only
        // transparent fragment selects the body at COMPILE time on
        // `base == Custom` (no runtime `shader_id ==` dispatch arm), so the
        // wrapper + its call must be present.
        assert!(
            source.contains("fn custom_shade_transparent_dynamic"),
            "transparent template missing custom_shade_transparent_dynamic wrapper"
        );
        assert!(
            source.contains("material_data_load"),
            "transparent template missing material_data_load accessor"
        );
        assert!(
            source.contains("struct MaterialData"),
            "transparent template missing auto-generated MaterialData struct"
        );
        let _ = dyn_id;
    }

    #[test]
    fn transparent_empty_dynamic_path_collapses() {
        // When no dynamic transparent material is registered
        // (dynamic_shader_id = None), the {% if shader_id_dynamic != 0 %}
        // block must emit nothing — no wrapper, no extra dispatch arm,
        // no MaterialData decl that doesn't belong on this pipeline.
        let key = ShaderCacheKeyMaterialTransparent {
            instancing_transforms: false,
            attributes: ShaderMaterialVertexAttributes::default(),
            texture_pool_arrays_len: 1,
            texture_pool_samplers_len: 1,
            msaa_sample_count: None,
            mipmaps: true,
            base: crate::dynamic_materials::ShadingBase::Pbr,
            pbr_features: awsm_materials::pbr::PbrFeatures::default().bits(),
            dispatch_hash: 0,
            dynamic_shader_id: None,
            dynamic_shader: None,
            froxel_slice_count: crate::render_passes::light_culling::DEFAULT_SLICE_COUNT,
        };

        let template = ShaderTemplateMaterialTransparent::try_from(&key)
            .expect("transparent template must build");
        let source = template.into_source().expect("transparent must render");

        assert!(
            !source.contains("custom_shade_transparent_dynamic"),
            "first-party transparent pipeline accidentally emits the dynamic wrapper"
        );
        assert!(
            !source.contains("material_data_load"),
            "first-party transparent pipeline accidentally emits the dynamic loader"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// brdf.wgsl PBR feature-gating tests. Render the shared lobe template
// standalone (it only references `pbr_features`) and assert the
// specialize-only contract: feature presence is purely compile-time, and
// NO feature runtime guards (`if (color.x > 0)`) survive in any
// configuration.
// Call-site-unique tokens (cc_fresnel / sheen_scaling / iri_f0) are used
// because the lobe *functions* are defined in brdf.wgsl and would always
// match by name.
#[cfg(test)]
mod brdf_gate_tests {
    use askama::Template;
    use awsm_materials::pbr::PbrFeatures;

    #[derive(Template)]
    #[template(path = "shared_wgsl/lighting/brdf.wgsl")]
    struct BrdfGateTest {
        pbr_features: PbrFeatures,
    }

    /// Rendered brdf with `//` line comments removed (so marker tokens
    /// that appear in comments outside the gates don't pollute matches)
    /// and all whitespace stripped (spacing-robust token matching).
    fn render_nows(pbr_features: PbrFeatures) -> String {
        let raw = BrdfGateTest { pbr_features }
            .render()
            .expect("brdf.wgsl renders");
        raw.lines()
            .map(|l| match l.find("//") {
                Some(i) => &l[..i],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    #[test]
    fn no_feature_runtime_guards_anywhere() {
        // Even the all-features shader must have NO per-feature runtime
        // guards — the specialize-only design moved all feature branching
        // to compile time. (Lighting-geometry guards like n_dot_l_back
        // remain; those aren't feature guards.)
        let w = render_nows(PbrFeatures::all());
        assert!(
            !w.contains("color.clearcoat>0"),
            "no clearcoat runtime guard"
        );
        assert!(
            !w.contains("color.iridescence>0"),
            "no iridescence runtime guard"
        );
        assert!(
            !w.contains("color.anisotropy_strength!=0"),
            "no anisotropy runtime guard"
        );
        assert!(
            !w.contains("color.diffuse_transmission>0"),
            "no diffuse-transmission runtime guard"
        );
        // ...but every lobe is still present at all().
        assert!(w.contains("cc_fresnel") && w.contains("sheen_scaling") && w.contains("iri_f0"));
    }

    #[test]
    fn specialized_strips_absent_lobes() {
        let mut f = PbrFeatures::all();
        f.clearcoat = false;
        f.sheen = false;
        let w = render_nows(f);
        assert!(
            !w.contains("cc_fresnel"),
            "absent clearcoat is compile-time stripped"
        );
        assert!(
            !w.contains("sheen_scaling"),
            "absent sheen is compile-time stripped"
        );
        assert!(w.contains("iri_f0"), "present iridescence is kept");
    }

    #[test]
    fn specialized_keeps_only_present_lobes() {
        let f = PbrFeatures {
            clearcoat: true,
            iridescence: true,
            ..PbrFeatures::default()
        };
        let w = render_nows(f);
        assert!(w.contains("cc_fresnel"), "present clearcoat emitted");
        assert!(w.contains("iri_f0"), "present iridescence emitted");
        assert!(!w.contains("sheen_scaling"), "absent sheen stripped");
    }
}
