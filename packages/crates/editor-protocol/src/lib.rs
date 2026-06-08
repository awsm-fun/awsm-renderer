//! Shared, serializable command/query wire types for driving the awsm-renderer
//! editor remotely (the MCP / WebTransport transport) and from headless tests.
//!
//! Pure data — no rendering, DOM, async, or reactive deps — so it compiles for
//! both the editor's wasm target and the native MCP server. The heavy payloads
//! (scene + animation data) already live in [`awsm_scene_schema`]; this crate
//! owns only the thin editor-control layer (`EditorCommand` / `EditorQuery` /
//! `EditorSnapshot` + a few UI enums) and re-exports the schema payloads so
//! callers have a single import path.
//!
//! The reactive materialization that the editor-control commands imply (turning
//! an `InsertSpec`/`NodeSpec` into a live `Node`, applying a command to the
//! controller) lives in the editor — this crate is the vocabulary, not the
//! interpreter.

mod anim_ui;
mod command;
mod node_spec;
mod query;
mod transport;

pub use anim_ui::{AnimSel, AnimView, StepKind};
pub use command::{CameraAxis, EditorCommand, EditorMode, ProceduralKind};
pub use node_spec::{kind_tag, InsertSpec, NodeQuery, NodeSpec};
pub use query::{
    AnimationSnapshot, ClipSnapshot, EditorQuery, EditorSnapshot, MaterialSnapshot, PixelsResult,
    ProjectSnapshot, QueryResult, ReadbackTarget, StatsResult, TimeseriesFrame, TimeseriesResult,
    TrackSnapshot,
};
pub use transport::{Request, Response};
