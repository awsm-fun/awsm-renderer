//! The runtime mesh: what `AssetSource::Mesh` resolves to in a baked
//! [`Scene`](crate::scene). Authoring meshes (`MeshDef` = modifier stack +
//! per-vertex overrides) live in `awsm-editor-protocol`; the editor's bake step
//! lowers one to the other (`MeshDef` → evaluate → [`MeshBlob`]).
//!
//! We are pre-1.0 and not shipping persisted player content yet, so this format
//! is designed *clean* rather than wire-compatible with the old
//! `awsm-scene-schema::CapturedMesh` bitcode blob — existing `.mesh.bin` files
//! re-bake. That frees `uvs`/`colors` to be multi-set and adds an open
//! [`NamedAttribute`] table (the runtime is no longer bound to the glTF
//! `COLOR_n`/`TEXCOORD_n` vocabulary — arbitrary per-vertex streams ride here:
//! splat weights, tangents, custom data).

use serde::{Deserialize, Serialize};

// NOTE: `RuntimeMesh = Primitive(PrimitiveShape) | Editable(MeshBlob)` lands in
// the next carve increment, once `primitive.rs` + its `AssetId` closure move
// into this crate (see docs/plans/unified-mesh-model.md "Execution blueprint").
// This increment establishes the keystone named-attribute table standalone.

/// Baked triangle geometry as a named-attribute table. `positions` + `indices`
/// are mandatory; everything else is optional / multi-set. `uvs[0]` is
/// `TEXCOORD_0`, `colors[0]` is `COLOR_0`; further sets are extra entries.
/// Anything outside that vocabulary goes in [`attributes`](MeshBlob::attributes).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct MeshBlob {
    pub positions: Vec<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normals: Option<Vec<[f32; 3]>>,
    /// UV sets, set 0 = `TEXCOORD_0`. Empty = untextured.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uvs: Vec<Vec<[f32; 2]>>,
    /// Color sets, set 0 = `COLOR_0`. Empty = unpainted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub colors: Vec<Vec<[f32; 4]>>,
    pub indices: Vec<u32>,
    /// Arbitrary named per-vertex streams beyond the glTF vocabulary — splat
    /// weights, tangents, custom data. One entry per stream.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<NamedAttribute>,
}

/// One named per-vertex stream. `data` length must equal `positions.len()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct NamedAttribute {
    pub name: String,
    pub data: AttributeData,
}

/// Per-vertex stream payload — fixed float widths (the renderer packs them into
/// the merged attribute buffer alongside uvs/colors).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum AttributeData {
    Vec2(Vec<[f32; 2]>),
    Vec3(Vec<[f32; 3]>),
    Vec4(Vec<[f32; 4]>),
}

impl AttributeData {
    /// Number of vertices this stream covers.
    pub fn len(&self) -> usize {
        match self {
            AttributeData::Vec2(v) => v.len(),
            AttributeData::Vec3(v) => v.len(),
            AttributeData::Vec4(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Float components per vertex (2 / 3 / 4).
    pub fn component_len(&self) -> usize {
        match self {
            AttributeData::Vec2(_) => 2,
            AttributeData::Vec3(_) => 3,
            AttributeData::Vec4(_) => 4,
        }
    }
}

impl MeshBlob {
    /// Vertex count (driven by `positions`).
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    /// Triangle count.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MeshBlob {
        MeshBlob {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: Some(vec![[0.0, 0.0, 1.0]; 3]),
            uvs: vec![vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]],
            colors: vec![vec![
                [1.0, 0.0, 0.0, 1.0],
                [0.0, 1.0, 0.0, 1.0],
                [0.0, 0.0, 1.0, 1.0],
            ]],
            indices: vec![0, 1, 2],
            attributes: vec![NamedAttribute {
                name: "_splat_weight".into(),
                data: AttributeData::Vec4(vec![[1.0, 0.0, 0.0, 0.0]; 3]),
            }],
        }
    }

    #[test]
    fn bitcode_round_trip() {
        let blob = sample();
        let bytes = bitcode::serialize(&blob).expect("serialize");
        let back: MeshBlob = bitcode::deserialize(&bytes).expect("deserialize");
        assert_eq!(blob, back);
    }

    #[test]
    fn json_round_trip_and_skips_empty() {
        // A bare untextured/unpainted blob: optional/multi-set fields skip.
        let bare = MeshBlob {
            positions: vec![[0.0; 3]],
            indices: vec![0],
            ..Default::default()
        };
        let json = serde_json::to_string(&bare).unwrap();
        assert!(!json.contains("uvs"), "empty uvs should skip: {json}");
        assert!(!json.contains("colors"), "empty colors should skip: {json}");
        assert!(!json.contains("attributes"), "empty attributes should skip");
        let back: MeshBlob = serde_json::from_str(&json).unwrap();
        assert_eq!(bare, back);
    }

    #[test]
    fn counts_and_attr_widths() {
        let blob = sample();
        assert_eq!(blob.vertex_count(), 3);
        assert_eq!(blob.triangle_count(), 1);
        assert_eq!(blob.attributes[0].data.len(), 3);
        assert_eq!(blob.attributes[0].data.component_len(), 4);
    }
}
