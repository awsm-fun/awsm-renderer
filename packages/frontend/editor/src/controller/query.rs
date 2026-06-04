//! `EditorQuery` / `snapshot()` — a serializable read of editor state for
//! external inspection + headless tests (§5.5). A future MCP/websocket transport
//! `serde`-encodes this back to the caller. It is a flat, view-agnostic
//! projection of the controller's state, not the live model.
//!
//! M3 carries the project/mode surface; the scene-tree / selection / material
//! projections are filled in as those models land (M4+).

use serde::{Deserialize, Serialize};

use super::command::EditorMode;

/// A serializable snapshot of editor state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorSnapshot {
    pub mode: EditorMode,
    pub project: ProjectSnapshot,
    pub undo_depth: usize,
    pub redo_depth: usize,
    // scene_tree / selection / materials / compile_errors land in M4+.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSnapshot {
    pub name: String,
    pub dirty: bool,
    pub missing_assets: Vec<String>,
}
