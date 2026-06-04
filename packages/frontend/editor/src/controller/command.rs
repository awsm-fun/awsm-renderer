//! `EditorCommand` — the single serializable enum covering every editor
//! mutation (decision 8 / §5.5). The UI never mutates editor state directly; it
//! builds a command and dispatches it through the [`super::EditorController`].
//! Commands are **data** (no closures) so they serialize, and non-transient
//! ones are invertible — the inverse is captured at apply-time and pushed onto
//! the undo log (command-sourcing, replacing the old snapshot history).
//!
//! M3 establishes the seam + the project/mode commands. The per-node mutation
//! commands (insert/delete/reparent/transform/material/env/…) are added as the
//! panels that dispatch them land in M4–M12.

use serde::{Deserialize, Serialize};

use super::node_spec::{InsertSpec, NodeSpec};
use crate::engine::scene::types::Trs;
use crate::engine::scene::NodeId;

/// Top-level workspace mode (the Scene/Material switch in the top bar).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorMode {
    #[default]
    Scene,
    Material,
}

/// Every editor mutation, as serializable data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum EditorCommand {
    /// Switch the workspace mode. **Transient** — dispatched but not recorded in
    /// the undo log.
    SwitchMode { mode: EditorMode },

    /// Set the current selection (ordered; last = primary/anchor). **Transient**
    /// — the UI computes single/ctrl-toggle/shift-range and dispatches the
    /// resulting set.
    SetSelection { ids: Vec<NodeId> },

    /// Set a node's local transform (TRS). Inverse: restore the prior transform.
    /// Consecutive `SetTransform`s on the same node coalesce into one undo step
    /// (so a drag-scrub is a single undo).
    SetTransform { id: NodeId, transform: Trs },

    /// Rename a node. Inverse: rename back to the prior name.
    Rename { id: NodeId, name: String },

    /// Set a node's visibility (eye toggle). Inverse: restore prior value.
    SetVisible { id: NodeId, visible: bool },

    /// Set a node's locked flag. Inverse: restore prior value.
    SetLocked { id: NodeId, locked: bool },

    /// Set a node's prefab-root flag. Inverse: restore prior value.
    SetPrefab { id: NodeId, prefab: bool },

    /// Duplicate a node (deep clone, fresh ids) as a following sibling. Inverse:
    /// delete the clone.
    Duplicate { id: NodeId },

    /// Reparent a node under `new_parent` at `index` (root when `None`).
    /// Inverse: reparent back to its prior parent + index.
    Reparent {
        id: NodeId,
        new_parent: Option<NodeId>,
        index: Option<usize>,
    },

    /// Start a fresh, empty project.
    NewProject,

    /// Insert a fresh node (from a ribbon Insert action) under `parent` (root
    /// when `None`). Inverse: `Delete` of the new node.
    Insert {
        spec: InsertSpec,
        parent: Option<NodeId>,
    },

    /// Re-insert a captured node subtree at `index` under `parent` (preserving
    /// ids). This is the inverse of `Delete` — undoing a delete restores the
    /// exact subtree. `node` is boxed (it's the largest variant payload).
    InsertTree {
        node: Box<NodeSpec>,
        parent: Option<NodeId>,
        index: Option<usize>,
    },

    /// Remove the node with `id` (and its subtree). Inverse: `InsertTree` of the
    /// captured subtree at its original position.
    Delete { id: NodeId },

    /// Load a project from a base URL (gesture-free; fetches `<base>/project.toml`
    /// and the referenced material/asset files). The external/MCP + headless-test
    /// entry point (§5.5). Full implementation lands in M11; the seam exists now.
    LoadProjectFromUrl { base_url: String },

    /// Import a glTF model from a URL (gesture-free). Pairs with the file-picker
    /// variant `ImportModelFromFile` (added with the ribbon in M4).
    ImportModelFromUrl { url: String },

    /// Import a texture from a URL (gesture-free).
    ImportTextureFromUrl { url: String },
}

impl EditorCommand {
    /// Transient commands are applied but never recorded in the undo log
    /// (mode switches, selection, camera orbit, panel toggles). Everything else
    /// records its inverse and participates in undo/redo.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            EditorCommand::SwitchMode { .. } | EditorCommand::SetSelection { .. }
        )
    }

    /// A short human-readable label (used in toasts / telemetry / the eventual
    /// undo-history UI). Consumed as the mutation commands land in M4+.
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            EditorCommand::SwitchMode { .. } => "Switch mode",
            EditorCommand::SetSelection { .. } => "Select",
            EditorCommand::NewProject => "New project",
            EditorCommand::Insert { .. } | EditorCommand::InsertTree { .. } => "Insert node",
            EditorCommand::Delete { .. } => "Delete node",
            EditorCommand::SetTransform { .. } => "Transform",
            EditorCommand::Rename { .. } => "Rename",
            EditorCommand::SetVisible { .. } => "Toggle visibility",
            EditorCommand::SetLocked { .. } => "Toggle lock",
            EditorCommand::SetPrefab { .. } => "Toggle prefab",
            EditorCommand::Duplicate { .. } => "Duplicate",
            EditorCommand::Reparent { .. } => "Reparent",
            EditorCommand::LoadProjectFromUrl { .. } => "Load project",
            EditorCommand::ImportModelFromUrl { .. } => "Import model",
            EditorCommand::ImportTextureFromUrl { .. } => "Import texture",
        }
    }
}
