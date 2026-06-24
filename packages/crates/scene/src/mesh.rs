//! The runtime mesh — what `AssetSource::Mesh` resolves to in a baked
//! [`Scene`](crate::scene).
//!
//! We do NOT hand-roll a runtime mesh-bytes format: reinventing vertex packing /
//! quantization / Draco / skins / morph targets (all of which glTF expresses
//! natively + maturely) is wasted effort. So **baked geometry is glb**
//! (`assets/<id>.glb`) — geometry + skin-rig + morph-targets only (materials +
//! animations are ours, in the scene + clips). Only cheap **primitives** stay
//! procedural (the player regenerates them from params).
//!
//! Authoring meshes (`MeshDef` = modifier stack + per-vertex overrides) live in
//! `awsm-renderer-editor-protocol`; the editor's bake lowers them — and imported
//! skinned/morph models — to a `RuntimeMesh` (`Primitive` or a baked `Glb`).

use serde::{Deserialize, Serialize};

use crate::primitive::PrimitiveShape;

/// The runtime mesh asset. `Primitive` regenerates from params at load (the
/// player runs meshgen primitive-gen — no side file); `Glb` loads the baked
/// `assets/<id>.glb` (geometry + skin binding + morph targets; NO materials/
/// animations — those are ours). The editor's bake produces one of these from
/// every mesh.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMesh {
    /// A primitive regenerated from params at load (no side file).
    Primitive(PrimitiveShape),
    /// Baked geometry + skin-rig + morph-targets — the `assets/<id>.glb` side
    /// file (one mesh asset = one glb). Re-exported through our glb writer at
    /// bake (uniform encoding, no source cruft).
    Glb,
}

/// File extension for a baked mesh glb side file — `<asset-id>.glb`.
pub const MESH_GLB_EXTENSION: &str = "glb";

/// On-disk filename for the baked glb bytes of an `AssetSource::Mesh(Glb)` entry,
/// addressed by the mesh asset's id. Returns just the leaf; callers prepend
/// `assets/`.
pub fn mesh_glb_filename(asset_id: crate::AssetId) -> String {
    format!("{}.{}", asset_id.0, MESH_GLB_EXTENSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AssetId;

    #[test]
    fn runtime_mesh_round_trips() {
        let prim = RuntimeMesh::Primitive(PrimitiveShape::Sphere {
            radius: 0.5,
            segments_long: 16,
            segments_lat: 12,
        });
        for rm in [prim, RuntimeMesh::Glb] {
            let json = serde_json::to_string(&rm).unwrap();
            assert_eq!(serde_json::from_str::<RuntimeMesh>(&json).unwrap(), rm);
        }
    }

    #[test]
    fn glb_filename_uses_id() {
        let id = AssetId::new();
        assert_eq!(mesh_glb_filename(id), format!("{}.glb", id.0));
    }
}
