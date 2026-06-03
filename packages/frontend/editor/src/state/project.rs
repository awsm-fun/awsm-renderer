//! Project-level state: the live directory handle (if any) and the
//! "dirty" flag (unsaved changes). The rest of the save/load flow lives
//! in `actions/project.rs`; this module just holds the state.

use crate::fs::ProjectDir;

pub const PROJECT_JSON_FILENAME: &str = "project.json";

pub struct ProjectState {
    pub directory: Option<ProjectDir>,
    pub dirty: bool,
}

impl ProjectState {
    pub fn new() -> Self {
        Self {
            directory: None,
            dirty: false,
        }
    }
}
