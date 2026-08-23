//! The runtime scene document — the lean, canonical thing the player loads
//! (`scene.toml` + an `assets/` directory). The editor authors a richer
//! `EditorProject` (in `awsm-renderer-editor-protocol`); its **bake** step lowers that to
//! a [`Scene`]: modifier stacks evaluate + collapse to mesh blobs (cheap
//! primitives stay procedural), per-vertex overrides apply, and the editor-only
//! library snapshots (material/animation authoring state) are dropped — only
//! what the player needs survives.

use serde::{Deserialize, Serialize};

use crate::{
    animation::{MixerDoc, StoredAnimation},
    assets::AssetTable,
    dynamic_material::CustomMaterialRef,
    environment::EnvironmentConfig,
    post_process::PostProcessConfig,
    shadows::ShadowsConfig,
    tree::EditorNode,
};

/// A baked runtime scene. References every asset by id into [`assets`](Scene::assets),
/// whose `Mesh` entries are runtime meshes ([`crate::mesh::RuntimeMesh`] —
/// primitive params or a baked blob), never authoring modifier stacks.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Scene {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub environment: EnvironmentConfig,
    /// Renderer-wide shadow settings, pushed into the renderer at load.
    #[serde(default)]
    pub shadows: ShadowsConfig,
    /// Renderer-wide post-processing (tonemapping / bloom / DoF / exposure),
    /// pushed into the renderer at load.
    #[serde(default)]
    pub post_process: PostProcessConfig,
    /// The by-id asset table (meshes / materials / textures / file refs).
    #[serde(default)]
    pub assets: AssetTable,
    /// Custom (runtime-registered WGSL) materials — refs to material folders
    /// under `assets/materials/<name>/`, loaded + registered at scene load.
    #[serde(default)]
    pub custom_materials: Vec<CustomMaterialRef>,
    /// Animation clips, in our own full-fidelity format (TRS + material-uniform /
    /// light / camera / morph tracks). The player reads these directly — no glTF,
    /// no `KHR_animation_pointer`.
    #[serde(default)]
    pub animations: Vec<StoredAnimation>,
    /// The NLA mixer document (layers / strips / masks, by clip id).
    #[serde(default)]
    pub mixer: MixerDoc,
    /// The node hierarchy.
    #[serde(default)]
    pub nodes: Vec<EditorNode>,
    /// The Camera node this scene is authored to be VIEWED through, carried
    /// from the editor's active camera so a framing composed in the editor is
    /// the framing the player renders — without the framing having to be
    /// duplicated as constants in player code.
    ///
    /// `None` = the player picks its own camera; that is what every
    /// pre-feature bundle deserializes to, so this is additive. A hint rather
    /// than a mandate: a player driving its own camera (a chase cam, a replay
    /// free-cam) is free to ignore it.
    #[serde(default)]
    pub active_camera: Option<crate::tree::NodeId>,
    /// The bundle's asset-pack index, when the bake packed its `assets/` files
    /// into a few concatenated `assets/pack-<n>.bin` blobs (see
    /// [`crate::project_dir::ScenePack`]). `None` = a loose-file bundle: every
    /// asset ships as its own file and the loader fetches per path — what every
    /// pre-pack bundle (and the editor's dev route) deserializes to, so this is
    /// additive in both directions (old parsers ignore the unknown key; the
    /// loader treats `None` as "fetch loose").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<crate::project_dir::ScenePack>,
}
