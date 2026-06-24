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
}
