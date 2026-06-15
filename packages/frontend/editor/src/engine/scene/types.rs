//! Re-exports of the serializable scene leaf types from
//! `lockstep-game-data`. The editor wraps these in `Mutable<T>` /
//! `MutableVec<T>` (see `node.rs`) for reactive editing; conversion
//! happens at Save / Load / Build boundaries via `SceneSnapshot`.
//!
//! `AssetStatus` lives here (not in game-data) because it's UI-only state
//! describing the renderer-side load progress for a `Model` node — it's
//! never serialized.

pub use awsm_editor_protocol::{
    CameraConfig, CameraProjection, ColliderShape, EnvironmentConfig, IblConfig, LightConfig,
    LightKind, NodeKind, SkyboxConfig, Trs,
};

/// Load state of the renderer-side asset (glb/gltf) for a `Model` node.
///
/// This is UI state, not scene data — it's never serialized. It lives on
/// `Node` rather than in the bridge so the tree view can observe it
/// without racing the async bridge-entry creation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AssetStatus {
    /// No load started (e.g. the node is a Group, Light, or Collision).
    #[default]
    Idle,
    Loading,
    Ready,
    Failed(String),
}
