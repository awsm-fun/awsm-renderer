//! `EditorController` — the single command/query authority.
//!
//! All editor/project state is governed here. The UI is just one driver: event
//! handlers translate gestures → [`EditorCommand`]s → [`EditorController::dispatch`];
//! they never mutate editor state directly. Non-transient commands record an
//! inverse and form the undo/redo log (command-sourcing). A serializable
//! [`EditorSnapshot`] read API exists for external inspection + headless tests.
//!
//! A future MCP/websocket transport is a thin adapter over `dispatch`/`snapshot`
//! — designed for now (the URL load/import command variants + source seam), not
//! built now.

pub mod animation;
mod command;
pub mod custom_material;
pub mod export;
pub mod mesh_eval;
mod node_spec;
pub mod persistence;
pub mod query;
mod source;

// The animation model + transport/mixer doc types. Several are consumed only by
// the Animation-mode UI panels; re-exported now so the contract is
// reachable + the command/query/persistence layers use them.
#[allow(unused_imports)]
pub use animation::{
    AnimSel, AnimView, ClipDirection, ClipLoop, CustomAnimation, Interp, MixerDoc, SamplerKind,
    StepKind, Track, TrackTarget, TrackValue,
};
pub use command::{CameraAxis, EditorCommand, EditorMode, ProceduralKind};
pub use custom_material::{compile_wgsl, AlphaMode, CustomMaterial, Slot};
// InsertSpec is dispatched by the ribbon; NodeQuery is the snapshot
// projection. `build_insert` / `spec_from_node` / `node_from_spec` are the
// editor-side reactive materialization (data types live in
// `awsm_renderer_editor_protocol`).
#[allow(unused_imports)]
pub use node_spec::{
    build_insert, node_from_spec, spec_from_node, InsertSpec, NodeQuery, NodeSpec,
};
pub use query::{EditorSnapshot, ProjectSnapshot};
// The query read-surface — consumed by the `editor_query_json` wasm seam
// + the future MCP transport.
#[allow(unused_imports)]
pub use query::{EditorQuery, QueryResult, ReadbackTarget};
// The source/sink seam is wired into the loader/saver; re-export now so
// the contract is reachable + documented.
#[allow(unused_imports)]
pub use source::{AssetSource, ProjectSink, ProjectSource};

mod state;

pub use state::*;
