use super::{assets::AssetId, dynamic_material::CustomMaterialInstance, tree::MeshShadowConfig};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ModelRef {
    /// Lookup into `EditorProject::assets` for the gltf/glb source. The
    /// table maps the id to either a project-relative filename or a
    /// runtime URL.
    pub asset_id: AssetId,
    /// Which node inside the referenced gltf/glb file.
    pub node_index: u32,
    /// Optional primitive index within that gltf node. `None` (the
    /// default) means "render every mesh primitive on this node". `Some(i)`
    /// is produced by the editor's `Split` action to peel one primitive
    /// onto its own editor node.
    #[serde(default)]
    pub primitive_index: Option<u32>,
    /// Optional library-material override for this model node. `None` (the
    /// default) renders every primitive with the material extracted from the
    /// glTF; `Some` reassigns the whole node to one chosen library material
    /// (the editor's "change the material on this mesh" for imported models).
    #[serde(default)]
    pub material: Option<CustomMaterialInstance>,
    /// Per-mesh shadow cast / receive flags.
    #[serde(default)]
    pub shadow: MeshShadowConfig,
}
