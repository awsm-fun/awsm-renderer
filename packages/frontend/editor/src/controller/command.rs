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

    /// Start a fresh, empty project.
    NewProject,

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
        matches!(self, EditorCommand::SwitchMode { .. })
    }

    /// A short human-readable label (used in toasts / telemetry / the eventual
    /// undo-history UI). Consumed as the mutation commands land in M4+.
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            EditorCommand::SwitchMode { .. } => "Switch mode",
            EditorCommand::NewProject => "New project",
            EditorCommand::LoadProjectFromUrl { .. } => "Load project",
            EditorCommand::ImportModelFromUrl { .. } => "Import model",
            EditorCommand::ImportTextureFromUrl { .. } => "Import texture",
        }
    }
}
