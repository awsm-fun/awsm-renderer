use super::{
    assets::AssetId, dynamic_material::CustomMaterialInstance, material::MaterialDef,
    tree::MeshShadowConfig,
};

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
    /// The node's assigned library material — **one material per node**, the
    /// same model as every other mesh in the editor. Set at import (the glTF
    /// material is destructured into a shared library material and assigned
    /// here); `None` means *unassigned* and renders flat magenta — the
    /// missing-material sentinel — exactly like an unassigned primitive. A glTF
    /// node whose primitives use *different* materials is split at import into
    /// one child node per primitive (each with its own `primitive_index` +
    /// `material`), so this single slot is always sufficient. The instance also
    /// carries per-node uniform/texture/buffer overrides, so one library
    /// material can be shared across many nodes and customized per node.
    #[serde(default)]
    pub material: Option<CustomMaterialInstance>,
    /// Per-mesh **uniform** values for a built-in assigned material (base color /
    /// opacity / metallic / roughness / emissive) — the same per-mesh store a
    /// Primitive/Mesh node carries. Seeded at import from the glTF material's
    /// factors; merged over the assigned material's shared variant by
    /// `builtin_merged`. Non-recompiling overrides (textures, etc.) live in
    /// `material`'s override maps.
    #[serde(default)]
    pub inline_material: MaterialDef,
    /// Per-mesh shadow cast / receive flags.
    #[serde(default)]
    pub shadow: MeshShadowConfig,
}
