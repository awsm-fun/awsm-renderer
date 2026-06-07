use super::{
    animation::{CustomAnimationRef, MixerDoc, StoredAnimation},
    assets::{AssetId, AssetTable},
    dynamic_material::CustomMaterialRef,
    environment::EnvironmentConfig,
    material::MaterialDef,
    shadows::ShadowsConfig,
    tree::EditorNode,
};

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

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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
    #[serde(default)]
    pub nodes: Vec<EditorNode>,
}
