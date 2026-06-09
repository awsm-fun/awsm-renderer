//! The editor's **authoring** layer + the serializable command/query wire types
//! for driving the awsm-renderer editor remotely (the MCP / WebTransport
//! transport) and from headless tests.
//!
//! Pure data — no rendering, DOM, async, or reactive deps — so it compiles for
//! both the editor's wasm target and the native MCP server. It depends on
//! [`awsm_scene`] (the lean runtime schema — reused for transforms, materials,
//! lights, cameras, clips, the node hierarchy) and [`awsm_meshgen`]'s recipe
//! types (the modifier stack the agent sends over the wire), and adds the
//! authoring types the runtime crate deliberately omits:
//! - [`MeshDef`] / [`VertexOverrides`] / [`CapturedSource`] — the editable
//!   `Mesh = base + edits` (lowered to `awsm_scene::RuntimeMesh` at bake);
//! - the authoring asset table ([`AssetSource`] carries `Mesh(MeshDef)`);
//! - [`EditorProject`] — the on-disk authoring document + its library snapshots.
//!
//! The reactive materialization these commands imply (turning an `InsertSpec` /
//! `NodeSpec` into a live `Node`, applying a command to the controller) lives in
//! the editor — this crate is the vocabulary, not the interpreter.

mod anim_ui;
mod assets;
mod command;
mod mesh_def;
mod node_spec;
mod project;
mod query;
mod transport;

pub use anim_ui::{AnimSel, AnimView, StepKind};
pub use assets::{asset_disk_path, asset_filename, AssetEntry, AssetSource, AssetTable};
pub use command::{
    BuiltinTextureSlot, CameraAxis, CustomAlphaMode, EditorCommand, EditorMode, ProceduralKind,
    SlotSpec,
};
pub use mesh_def::{CapturedMesh, CapturedSource, MeshDef, VertexOverrides};
pub use node_spec::{kind_tag, InsertSpec, NodeQuery, NodeSpec};
pub use project::{EditorProject, StoredMaterial, StoredSlot};
pub use query::{
    AnimationSnapshot, ClipSnapshot, CompileDiagnostics, CompileError, EditorQuery, EditorSnapshot,
    MapResult, MaterialSnapshot, PixelsResult, ProjectSnapshot, QueryResult, ReadbackTarget,
    SettledResult, StatsResult, TextureSnapshot, TimeseriesFrame, TimeseriesResult, TrackSnapshot,
    VertexPredicate,
};
pub use transport::{EditorEvent, Request, Response};

// Re-export the meshgen recipe types so editor + mcp callers that build/send a
// modifier stack have a single import path alongside the commands that carry it.
pub use awsm_meshgen::recipe::{
    Axis, MeshBase, Modifier, ModifierStack, SdfNode, SdfPrimitive, SweepAlongCurveDef,
};
