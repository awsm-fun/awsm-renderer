//! The editor's **authoring** layer + the serializable command/query wire types
//! for driving the awsm-renderer editor remotely (the MCP WebSocket transport)
//! and from headless tests.
//!
//! Pure data — no rendering, DOM, async, or reactive deps — so it compiles for
//! both the editor's wasm target and the native MCP server. It depends on
//! [`awsm_renderer_scene`] (the lean runtime schema — reused for transforms, materials,
//! lights, cameras, clips, the node hierarchy) and [`awsm_renderer_meshgen`]'s recipe
//! types (the modifier stack the agent sends over the wire), and adds the
//! authoring types the runtime crate deliberately omits:
//! - [`MeshDef`] / [`VertexOverrides`] / [`CapturedSource`] — the editable
//!   `Mesh = base + edits` (lowered to `awsm_renderer_scene::RuntimeMesh` at bake);
//! - the authoring asset table ([`AssetSource`] carries `Mesh(MeshDef)`);
//! - [`EditorProject`] — the on-disk authoring document + its library snapshots.
//!
//! The reactive materialization these commands imply (turning an `InsertSpec` /
//! `NodeSpec` into a live `Node`, applying a command to the controller) lives in
//! the editor — this crate is the vocabulary, not the interpreter.

// The umbrella re-export (`pub use awsm_renderer_scene::*`) intentionally has its runtime
// `AssetSource`/`AssetEntry`/`AssetTable` shadowed by this crate's authoring
// versions (Rust resolves the explicit re-export over the glob). That shadowing
// is the whole point — silence the advisory lint.
#![allow(hidden_glob_reexports)]

mod anim_ui;
mod assets;
mod bake;
mod command;
mod history;
mod merge_patch;
mod mesh_def;
mod node_spec;
mod project;
mod query;
mod transport;

pub use anim_ui::{AnimSel, AnimView, StepKind};
pub use assets::{asset_disk_path, asset_filename, AssetEntry, AssetSource, AssetTable, BufferDef};
pub use bake::{lower_mesh, project_to_scene};
pub use command::{
    BuiltinTextureSlot, CameraAxis, CustomAlphaMode, EditorCommand, EditorMode, ProceduralKind,
    SkinWeightEntry, SlotSpec,
};
pub use history::{estimate_command_bytes, BoundedHistory, DEFAULT_HISTORY_BUDGET_BYTES};
pub use merge_patch::{coerce_patch, json_merge_patch};
pub use mesh_def::{CapturedMesh, CapturedSource, MeshDef, VertexOverrides};
pub use node_spec::{kind_tag, InsertSpec, NodeQuery, NodeSpec};
pub use project::{EditorProject, StoredMaterial, StoredSlot};
pub use query::{
    AnimationSnapshot, ClipSnapshot, CompileDiagnostics, CompileError, EditorQuery, EditorSnapshot,
    EnvSlotSnapshot, EnvironmentSnapshot, MapResult, MaterialSnapshot, PixelsResult,
    ProjectSnapshot, QueryResult, ReadbackTarget, SettledResult, StatsResult, TextureSnapshot,
    TimeseriesFrame, TimeseriesResult, TrackSnapshot, VertexPredicate,
};
pub use transport::{
    BundleFileMeta, BundleHandle, EditorEvent, GlbHandle, PngHandle, Request, Response,
    WsClientMsg, WsServerMsg,
};

// Re-export the meshgen recipe types so editor + mcp callers that build/send a
// modifier stack have a single import path alongside the commands that carry it.
pub use awsm_renderer_meshgen::recipe::{
    Axis, MeshBase, Modifier, ModifierStack, SdfNode, SdfPrimitive, SweepAlongCurveDef,
};

// Umbrella: re-export the runtime CORE schema so the editor has a single import
// path for both authoring + CORE types. The authoring `AssetSource`/`AssetEntry`/
// `AssetTable` re-exported above deliberately shadow awsm-renderer-scene's runtime ones
// (glob re-exports yield to explicit ones), so `awsm_renderer_editor_protocol::AssetSource`
// is the authoring table (carries `Mesh(MeshDef)`).
pub use awsm_renderer_scene::*;
