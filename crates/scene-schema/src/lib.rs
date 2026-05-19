//! Authored scene shape consumed by the awsm-renderer scene editor and
//! the runtime player. Pure data — no rendering deps.
//!
//! `EditorProject` is the on-disk schema the editor saves and loads
//! (`project.json`), and the same structure that gets embedded into a
//! per-game build artifact. Treating it as one type means the runtime
//! never juggles "json plus bytes" — Build produces a single bundled
//! struct, Load round-trips it identically.
//!
//! Contents are deliberately game-agnostic: the editor authors a generic
//! scene (nodes, transforms, lights, collisions, environment) plus a
//! flat asset table. Per-game packing rules live in each game's struct
//! and run at Build time, not here.
//!
//! Coordinate convention: right-handed, Y-up, meters. Rotations are unit
//! quaternions stored as `[x, y, z, w]`.

pub mod assets;
pub mod camera;
pub mod collider;
pub mod curve;
pub mod environment;
pub mod instances;
pub mod light;
pub mod line;
pub mod material;
pub mod model;
pub mod particle;
pub mod primitive;
pub mod project;
pub mod shadows;
pub mod sprite;
pub mod transform;
pub mod tree;

pub use assets::*;
pub use camera::*;
pub use collider::*;
pub use curve::*;
pub use environment::*;
pub use instances::*;
pub use light::*;
pub use line::*;
pub use material::*;
pub use model::*;
pub use particle::*;
pub use primitive::*;
pub use project::*;
pub use shadows::*;
pub use sprite::*;
pub use transform::*;
pub use tree::*;

#[cfg(test)]
mod tests {
    //! Sanity checks: `EditorProject` round-trips through both serde-JSON
    //! (the on-disk `project.json` format) and bitcode (the per-game
    //! build artifact). If either of these regresses, the editor's
    //! Save/Load or the runtime's bin-loading will silently break.

    use super::*;

    fn sample() -> EditorProject {
        let asset = AssetId::new();
        let mut assets = AssetTable::new();
        // Content-hash addressing post-`feat(schema): content-hash …`:
        // a stable test hash keeps this round-trip deterministic across
        // serializers; real callers compute the SHA-256 from upload bytes.
        assets.insert_file_with_hash(
            "robot.glb".to_string(),
            "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        );
        let _unused = asset; // keep the fresh-id helper exercised
        EditorProject {
            name: String::new(),
            environment: EnvironmentConfig::default(),
            assets,
            nodes: vec![EditorNode {
                id: NodeId::new(),
                name: "root".to_string(),
                transform: Trs::IDENTITY,
                kind: NodeKind::Group,
                locked: false,
                visible: true,
                prefab: false,
                children: vec![],
            }],
        }
    }

    #[test]
    fn json_roundtrip() {
        let project = sample();
        let json = serde_json::to_string(&project).unwrap();
        let back: EditorProject = serde_json::from_str(&json).unwrap();
        assert_eq!(project, back);
    }

    #[test]
    fn bitcode_roundtrip() {
        let project = sample();
        let bytes = bitcode::serialize(&project).unwrap();
        let back: EditorProject = bitcode::deserialize(&bytes).unwrap();
        assert_eq!(project, back);
    }
}
