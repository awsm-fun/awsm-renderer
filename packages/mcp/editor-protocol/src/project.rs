use awsm_renderer_scene::animation::{CustomAnimationRef, MixerDoc, StoredAnimation};
use awsm_renderer_scene::{
    AssetId, CustomMaterialRef, EditorNode, EnvironmentConfig, MaterialDef, PostProcessConfig,
    ShadowsConfig,
};

use crate::assets::AssetTable;

/// Player-bundle export options — persisted in project.toml, edited in the
/// pre-export modal and via MCP `set_bundle_options` (with optional per-call
/// overrides on `export_player_bundle`). Applied to base mesh glbs, bundle
/// rig copies, and the coarse LOD chain glbs. The knobs are independent:
/// quantization without meshopt is valid (`KHR_mesh_quantization` alone).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct BundleOptions {
    #[serde(default)]
    pub mesh_compression: MeshCompression,
    #[serde(default)]
    pub mesh_quantization: MeshQuantization,
    /// `Smart` quantizes a mesh only when its position grid step
    /// (max half-extent / 32767) stays at or under this many millimetres.
    #[serde(default = "default_smart_threshold_mm")]
    pub smart_threshold_mm: f32,
    #[serde(default)]
    pub texture_compression: TextureCompression,
}

fn default_smart_threshold_mm() -> f32 {
    0.1
}

impl Default for BundleOptions {
    fn default() -> Self {
        Self {
            mesh_compression: MeshCompression::default(),
            mesh_quantization: MeshQuantization::default(),
            smart_threshold_mm: default_smart_threshold_mm(),
            texture_compression: TextureCompression::default(),
        }
    }
}

/// Mesh stream encoding (`EXT_meshopt_compression`).
#[derive(Clone, Copy, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum MeshCompression {
    Off,
    #[default]
    Meshopt,
}

/// Mesh quantization policy (`KHR_mesh_quantization`). Structural guards
/// (morph targets, multi-skin / mixed-use meshes, IBM-less skins,
/// out-of-[0,1] UVs) are correctness, not policy — they apply even under
/// `Always`.
#[derive(Clone, Copy, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum MeshQuantization {
    Off,
    Always,
    /// Quantize only when structurally possible AND the grid step stays under
    /// [`BundleOptions::smart_threshold_mm`].
    #[default]
    Smart,
}

/// Bundle texture default. `Off` = lossless WebP (pixel-exact), never raw
/// source dumps. Per-texture prefs override the global either way.
#[derive(Clone, Copy, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum TextureCompression {
    Off,
    #[default]
    Ktx2,
}

/// A partial update of [`BundleOptions`] (mirrors [`crate::ShadowsPatch`]):
/// `None` fields preserve the current value. Built by the MCP
/// `set_bundle_options` tool, the pre-export modal, and the per-call
/// overrides on `export_player_bundle` (where it merges onto the persisted
/// options WITHOUT modifying them).
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct BundleOptionsPatch {
    #[serde(default)]
    pub mesh_compression: Option<MeshCompression>,
    #[serde(default)]
    pub mesh_quantization: Option<MeshQuantization>,
    /// Clamped non-negative on apply (a negative step threshold would just
    /// mean "never quantize" — normalize it to 0 which means the same).
    #[serde(default)]
    pub smart_threshold_mm: Option<f32>,
    #[serde(default)]
    pub texture_compression: Option<TextureCompression>,
}

impl BundleOptionsPatch {
    /// Merge onto `base`: `None` preserves, `Some` sets.
    pub fn apply(&self, base: BundleOptions) -> BundleOptions {
        BundleOptions {
            mesh_compression: self.mesh_compression.unwrap_or(base.mesh_compression),
            mesh_quantization: self.mesh_quantization.unwrap_or(base.mesh_quantization),
            smart_threshold_mm: self
                .smart_threshold_mm
                .unwrap_or(base.smart_threshold_mm)
                .max(0.0),
            texture_compression: self.texture_compression.unwrap_or(base.texture_compression),
        }
    }

    /// A patch that sets every field — the undo inverse of any applied patch.
    pub fn replace(options: &BundleOptions) -> Self {
        Self {
            mesh_compression: Some(options.mesh_compression),
            mesh_quantization: Some(options.mesh_quantization),
            smart_threshold_mm: Some(options.smart_threshold_mm),
            texture_compression: Some(options.texture_compression),
        }
    }
}

/// One declared slot of a stored custom material (uniform / texture / buffer).
/// Mirrors the editor's `Slot`. Editor-only — the player ignores `editor_materials`.
#[derive(Clone, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StoredSlot {
    pub name: String,
    pub ty: String,
    #[serde(default)]
    pub val: String,
    #[serde(default)]
    pub debug: String,
    /// Texture slots only: the slot's semantic role (see `SlotSpec::color_kind`).
    #[serde(default)]
    pub color_kind: awsm_renderer_scene::TextureColorKind,
}

/// A persisted editor custom material — the full library entry, so that built-in
/// (PBR/Unlit/Toon variant) **and** dynamic WGSL materials survive save/load and
/// reappear in the Material library, keyed by their stable `AssetId` (which the
/// scene nodes reference). Editor-only; `#[serde(default)]` everywhere so a
/// project without this section still loads, and the player ignores it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StoredMaterial {
    pub id: AssetId,
    pub name: String,
    /// `Some` ⇒ built-in (carries the shared variant `MaterialDef`); `None` ⇒ dynamic.
    #[serde(default)]
    pub builtin: Option<MaterialDef>,
    #[serde(default)]
    pub wgsl: String,
    /// The second, alpha-only WGSL window (returns `f32`). Only meaningful for
    /// `alpha == "mask"`; compiled into the masked visibility-raster variant.
    /// Empty / absent → no masked cutout. `#[serde(default)]` so older projects
    /// round-trip.
    #[serde(default)]
    pub alpha_wgsl: String,
    /// The third, vertex-displacement WGSL window — wrapped into
    /// `custom_displace_vertex` and compiled into the geometry/shadow raster so
    /// the material moves its own vertices. Empty / absent → no custom vertex
    /// (shared fast pipeline). `#[serde(default)]` so older projects round-trip.
    #[serde(default)]
    pub vertex_wgsl: String,
    /// "opaque" / "mask" / "blend".
    #[serde(default)]
    pub alpha: String,
    #[serde(default)]
    pub cutoff: f32,
    #[serde(default)]
    pub double_sided: bool,
    /// Debug preview color (`#rrggbb`).
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub uniforms: Vec<StoredSlot>,
    #[serde(default)]
    pub textures: Vec<StoredSlot>,
    #[serde(default)]
    pub buffers: Vec<StoredSlot>,
    #[serde(default)]
    pub registered: bool,
    #[serde(default)]
    pub shader_includes: Vec<String>,
    #[serde(default)]
    pub fragment_inputs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EditorProject {
    /// Human-readable project name. When empty (the default), the
    /// editor falls back to the on-disk directory name for display +
    /// build-artifact filename. Set by the in-header rename input;
    /// preserved across save/load so rename is durable. Existing
    /// project.json files without a `name` round-trip via
    /// `#[serde(default)]`.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub environment: EnvironmentConfig,
    /// Renderer-wide shadow settings. Read at startup by the
    /// editor / player and pushed into the renderer via
    /// `AwsmRenderer::set_shadows_config`; subsequent edits push the
    /// updated config the same way and take effect on the next
    /// rendered frame.
    #[serde(default)]
    pub shadows: ShadowsConfig,
    /// Renderer-wide post-processing (tonemapping / bloom / DoF / exposure).
    /// Same lifecycle as `shadows`: read at startup, pushed via
    /// `AwsmRenderer::set_post_processing`, live-synced on edit.
    #[serde(default)]
    pub post_process: PostProcessConfig,
    #[serde(default)]
    pub assets: AssetTable,
    /// Custom (runtime-registered) materials imported into the project.
    /// Each entry points at a material folder under
    /// `<project>/assets/materials/<name>/`. The renderer-bridge walks
    /// this list on project load, calls
    /// `load_material_folder` for each, and registers the result via
    /// `AwsmRenderer::register_material`.
    ///
    /// Old `project.json` files without this field round-trip via
    /// `#[serde(default)]`.
    #[serde(default)]
    pub custom_materials: Vec<CustomMaterialRef>,
    /// Full editor custom-material library (built-in variant defs + dynamic WGSL),
    /// so materials survive save/load and reappear in the Material pane. Editor-only
    /// (the runtime player uses `custom_materials` refs); `#[serde(default)]` so
    /// pre-existing projects round-trip.
    #[serde(default)]
    pub editor_materials: Vec<StoredMaterial>,
    /// Animation clips imported/authored in the project (refs to
    /// `animation-<slug>.toml` side files). Mirrors `custom_materials`.
    /// Editor-only; `#[serde(default)]` so projects without animation round-trip.
    #[serde(default)]
    pub custom_animations: Vec<CustomAnimationRef>,
    /// Full editor animation-clip library (the authored model), so clips survive
    /// save/load and reappear in the Animation library. Editor-only.
    #[serde(default)]
    pub editor_animations: Vec<StoredAnimation>,
    /// The NLA mixer document (layers / strips / masks, by clip id). Editor-only.
    #[serde(default)]
    pub anim_mixer: MixerDoc,
    /// Player-bundle export options (mesh compression / quantization /
    /// texture default). `#[serde(default)]` so older projects round-trip
    /// onto the locked defaults (Meshopt + Smart 0.1mm + KTX2).
    #[serde(default)]
    pub bundle_options: BundleOptions,
    #[serde(default)]
    pub nodes: Vec<EditorNode>,
}

#[cfg(test)]
mod bundle_options_tests {
    use super::*;

    /// A project saved before `bundle_options` existed loads onto the locked
    /// defaults: Meshopt + Smart 0.1mm + KTX2.
    #[test]
    fn missing_section_yields_locked_defaults() {
        let project: EditorProject = toml::from_str("name = \"old\"").unwrap();
        assert_eq!(project.bundle_options, BundleOptions::default());
        let o = project.bundle_options;
        assert_eq!(o.mesh_compression, MeshCompression::Meshopt);
        assert_eq!(o.mesh_quantization, MeshQuantization::Smart);
        assert_eq!(o.smart_threshold_mm, 0.1);
        assert_eq!(o.texture_compression, TextureCompression::Ktx2);
    }

    /// Patch semantics: `None` preserves, `Some` sets, threshold clamps
    /// non-negative, and `replace` round-trips any options wholesale.
    #[test]
    fn patch_apply_and_replace() {
        let base = BundleOptions::default();
        let patched = BundleOptionsPatch {
            texture_compression: Some(TextureCompression::Off),
            smart_threshold_mm: Some(-1.0),
            ..Default::default()
        }
        .apply(base);
        assert_eq!(patched.mesh_compression, base.mesh_compression);
        assert_eq!(patched.mesh_quantization, base.mesh_quantization);
        assert_eq!(patched.smart_threshold_mm, 0.0, "clamped non-negative");
        assert_eq!(patched.texture_compression, TextureCompression::Off);

        assert_eq!(BundleOptionsPatch::replace(&patched).apply(base), patched);
        assert_eq!(BundleOptionsPatch::default().apply(patched), patched);
    }

    #[test]
    fn non_default_options_roundtrip_through_toml() {
        let project = EditorProject {
            bundle_options: BundleOptions {
                mesh_compression: MeshCompression::Off,
                mesh_quantization: MeshQuantization::Always,
                smart_threshold_mm: 0.5,
                texture_compression: TextureCompression::Off,
            },
            ..Default::default()
        };
        let toml_str = toml::to_string_pretty(&project).unwrap();
        let back: EditorProject = toml::from_str(&toml_str).unwrap();
        assert_eq!(back.bundle_options, project.bundle_options);
    }
}
