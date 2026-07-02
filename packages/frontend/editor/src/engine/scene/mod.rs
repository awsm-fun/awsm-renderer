//! Editor scene model — a live, reactive tree (`Mutable`/`MutableVec`) held by
//! the `EditorController`. UI-agnostic; adapted from the archived editor. The
//! old snapshot-history module is dropped — undo/redo is command-sourcing in the
//! controller now. Coordinate convention: right-handed, Y-up, meters.

// The scene model is the foundation consumed incrementally by the panels +
// renderer bridge + persistence; allow the not-yet-wired surface.
#![allow(dead_code)]

pub mod assets;
mod model;
pub mod mutate;
pub mod node;
pub mod types;

// The full scene-leaf surface is re-exported for the panels + persistence;
// allow the not-yet-consumed names now.
#[allow(unused_imports)]
pub use assets::{AssetId, AssetSource, AssetTable};
pub use awsm_renderer_editor_protocol::ShadowsConfig;
#[allow(unused_imports)]
pub use awsm_renderer_editor_protocol::{PostProcessConfig, ToneMappingConfig};
#[allow(unused_imports)]
pub use model::{Scene, SceneStats};
pub use node::{Node, NodeId};
#[allow(unused_imports)]
pub use types::{
    AssetStatus, CameraConfig, CameraProjection, ColliderShape, EnvironmentConfig, IblConfig,
    LightConfig, LightKind, NodeKind, SkyboxConfig, Trs,
};
