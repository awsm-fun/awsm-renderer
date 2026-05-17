//! Project-level state: the live directory handle (if any) and the
//! "dirty" flag (unsaved changes). The rest of the save/load flow lives
//! in `actions/project.rs`; this module just holds the state.

use crate::fs::ProjectDir;

pub const PROJECT_JSON_FILENAME: &str = "project.json";
pub const ASSETS_SUBDIR: &str = "assets";

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

/// Build the on-disk path for an asset filename. Internal-only — never
/// serialized into `project.json` (the asset table stores the filename
/// alone; the `assets/` prefix is implied by convention).
pub fn asset_disk_path(filename: &str) -> String {
    format!("{ASSETS_SUBDIR}/{filename}")
}
