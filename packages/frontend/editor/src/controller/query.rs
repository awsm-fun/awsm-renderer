//! `EditorQuery` / `snapshot()` — a serializable read of editor state for
//! external inspection + headless tests (§5.5). A future MCP/websocket transport
//! `serde`-encodes this back to the caller. It is a flat, view-agnostic
//! projection of the controller's state, not the live model.
//!
//! M3 carries the project/mode surface; the scene-tree / selection / material
//! projections are filled in as those models land (M4+).

use serde::{Deserialize, Serialize};

use super::command::EditorMode;
use super::node_spec::NodeQuery;

/// A serializable snapshot of editor state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorSnapshot {
    pub mode: EditorMode,
    pub project: ProjectSnapshot,
    /// The scene tree (id / name / kind / children), top-level first.
    pub scene_tree: Vec<NodeQuery>,
    /// Selected node ids (ordered; last = primary).
    pub selection: Vec<String>,
    pub undo_depth: usize,
    pub redo_depth: usize,
    // materials / compile_errors land as those models arrive.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSnapshot {
    pub name: String,
    pub dirty: bool,
    pub missing_assets: Vec<String>,
}
