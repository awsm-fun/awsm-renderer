//! `awsm-scene` — the lean, canonical **runtime** scene schema for the
//! awsm-renderer player: `scene.toml` + an `assets/` directory, all by-id. The
//! player and renderer touch only this crate.
//!
//! Authoring lives elsewhere: the modifier stack, per-vertex overrides, the
//! editor's `Mesh = base + edits`, and `EditorCommand`/`EditorQuery` are in
//! `awsm-editor-protocol` (which depends on this crate and reuses its core
//! types). The editor's bake step lowers authoring → runtime
//! (`MeshDef` → [`mesh::MeshBlob`]).
//!
//! Carve in progress — see `docs/plans/unified-mesh-model.md` "Execution
//! blueprint". This first increment establishes the runtime mesh attribute
//! table; CORE modules (transform / assets / material / tree / animation / …)
//! and the runtime `Scene` document + `scene.toml` writer follow.
//!
//! Coordinate convention: right-handed, Y-up, meters. Rotations are unit
//! quaternions stored as `[x, y, z, w]`.

pub mod mesh;

pub use mesh::*;
