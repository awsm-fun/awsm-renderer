//! `EditorController` — the single command/query authority (decision 8 / §5.5).
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

mod command;
mod query;
mod source;

pub use command::{EditorCommand, EditorMode};
pub use query::{EditorSnapshot, ProjectSnapshot};
// The source/sink seam is wired into the loader/saver in M11; re-export now so
// the contract is reachable + documented.
#[allow(unused_imports)]
pub use source::{AssetSource, ProjectSink, ProjectSource};

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

use awsm_web_shared::prelude::{Mutable, Toast};

use crate::error::EditorResult;

thread_local! {
    static CONTROLLER: OnceCell<EditorController> = const { OnceCell::new() };
}

/// Install the controller singleton. Call once at boot, before mounting the UI.
pub fn init() {
    CONTROLLER.with(|c| {
        let _ = c.set(EditorController::new());
    });
}

/// A cheap clone of the controller singleton (all fields are `Mutable`/`Rc`).
pub fn controller() -> EditorController {
    CONTROLLER.with(|c| c.get().expect("controller not initialized").clone())
}

/// The command/query authority. Clone is cheap — every field is a shared handle.
#[derive(Clone)]
pub struct EditorController {
    pub mode: Mutable<EditorMode>,
    pub project_name: Mutable<String>,
    pub dirty: Mutable<bool>,
    pub missing_assets: Mutable<Vec<String>>,
    pub can_undo: Mutable<bool>,
    pub can_redo: Mutable<bool>,
    /// Inverses of applied commands, newest last (the undo log).
    undo: Rc<RefCell<Vec<EditorCommand>>>,
    /// Inverses popped by undo, re-appliable by redo.
    redo: Rc<RefCell<Vec<EditorCommand>>>,
}

impl EditorController {
    fn new() -> Self {
        Self {
            mode: Mutable::new(EditorMode::default()),
            project_name: Mutable::new("untitled.awsm".to_string()),
            dirty: Mutable::new(false),
            missing_assets: Mutable::new(Vec::new()),
            can_undo: Mutable::new(false),
            can_redo: Mutable::new(false),
            undo: Rc::new(RefCell::new(Vec::new())),
            redo: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// The single entry point. UI handlers build a command and dispatch it here;
    /// async because some commands await the renderer / FS / network.
    pub async fn dispatch(&self, cmd: EditorCommand) -> EditorResult<()> {
        let transient = cmd.is_transient();
        let inverse = self.apply(cmd).await?;
        if !transient {
            if let Some(inv) = inverse {
                self.undo.borrow_mut().push(inv);
                self.redo.borrow_mut().clear();
                self.refresh_history_signals();
            }
            self.dirty.set_neq(true);
        }
        Ok(())
    }

    /// Apply a command's effect and return its inverse (for the undo log), or
    /// `None` if the command is not undoable. The undoable per-node mutation
    /// commands return `Some(inverse)` here as they land in M4+.
    async fn apply(&self, cmd: EditorCommand) -> EditorResult<Option<EditorCommand>> {
        match cmd {
            EditorCommand::SwitchMode { mode } => {
                self.mode.set_neq(mode);
                Ok(None)
            }
            EditorCommand::NewProject => {
                // Project-level reset. Full scene-clear + undo-restore lands with
                // the project model (M11); for now reset the label/flags.
                self.project_name.set("untitled.awsm".to_string());
                self.missing_assets.set(Vec::new());
                self.dirty.set_neq(false);
                self.undo.borrow_mut().clear();
                self.redo.borrow_mut().clear();
                self.refresh_history_signals();
                Toast::info("New project");
                Ok(None)
            }
            EditorCommand::LoadProjectFromUrl { base_url } => {
                // Seam present; the fetch + TOML deserialize lands in M11.
                Toast::info(format!("Load project from {base_url} — lands in M11"));
                Ok(None)
            }
            EditorCommand::ImportModelFromUrl { url } => {
                Toast::info(format!("Import model from {url} — lands in M4/M11"));
                Ok(None)
            }
            EditorCommand::ImportTextureFromUrl { url } => {
                Toast::info(format!("Import texture from {url} — lands in M11"));
                Ok(None)
            }
        }
    }

    /// Pop the newest inverse and apply it; its own inverse becomes a redo entry.
    pub async fn undo(&self) {
        let cmd = self.undo.borrow_mut().pop();
        if let Some(cmd) = cmd {
            if let Ok(Some(inv)) = self.apply(cmd).await {
                self.redo.borrow_mut().push(inv);
            }
            self.refresh_history_signals();
        }
    }

    /// Re-apply the newest redo entry.
    pub async fn redo(&self) {
        let cmd = self.redo.borrow_mut().pop();
        if let Some(cmd) = cmd {
            if let Ok(Some(inv)) = self.apply(cmd).await {
                self.undo.borrow_mut().push(inv);
            }
            self.refresh_history_signals();
        }
    }

    fn refresh_history_signals(&self) {
        self.can_undo.set_neq(!self.undo.borrow().is_empty());
        self.can_redo.set_neq(!self.redo.borrow().is_empty());
    }

    /// A serializable read of editor state (§5.5) for external inspection.
    pub fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            mode: self.mode.get(),
            project: ProjectSnapshot {
                name: self.project_name.get_cloned(),
                dirty: self.dirty.get(),
                missing_assets: self.missing_assets.get_cloned(),
            },
            undo_depth: self.undo.borrow().len(),
            redo_depth: self.redo.borrow().len(),
        }
    }

    /// `snapshot()` as a JSON string (the shape an MCP/websocket transport would
    /// return). Used by headless tests + the future external transport.
    pub fn snapshot_json(&self) -> String {
        serde_json::to_string(&self.snapshot()).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}
