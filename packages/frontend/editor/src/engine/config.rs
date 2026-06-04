//! Engine tuning constants. (The legacy media-base / material-editor URL config
//! from the archived editor is dropped — the unified editor authors materials
//! in-app, and the debug external-scene loader isn't ported.)

use std::sync::LazyLock;

#[derive(Debug, Clone)]
pub struct Config {
    pub camera_focus_distance: f32,
    pub camera_aperture: f32,
}

impl Config {
    /// URL for the editor's bundled gltf gizmo asset (Trunk copies the crate's
    /// `assets/` dir into the dist). Relative — resolves against the page base.
    /// Consumed by the gizmo loader in M6.
    #[allow(dead_code)]
    pub fn gizmo_url(&self) -> &'static str {
        "assets/gizmo.glb"
    }
}

pub static CONFIG: LazyLock<Config> = LazyLock::new(|| Config {
    camera_aperture: 5.6,
    camera_focus_distance: 10.0,
});

/// Keyboard shortcut bindings, matched against `KeyboardEvent::key()`. Consumed
/// by the key handler as the editing commands land (M4+).
#[allow(dead_code)]
pub mod keys {
    pub const DELETE: &[&str] = &["Delete", "Backspace"];
    pub const ESCAPE: &str = "Escape";
    pub const ARROW_UP: &str = "ArrowUp";
    pub const ARROW_DOWN: &str = "ArrowDown";
    pub const DUPLICATE_KEY: &str = "d";
    pub const SAVE_KEY: &str = "s";
}

/// Drag threshold before a tree-row pointer-down is treated as a drag.
#[allow(dead_code)]
pub const TREE_DRAG_THRESHOLD_PX: f64 = 4.0;
/// Per-depth indentation inside the tree view.
#[allow(dead_code)]
pub const TREE_INDENT_PX: f64 = 16.0;
/// Height of a single tree row.
#[allow(dead_code)]
pub const TREE_ROW_HEIGHT_PX: f64 = 24.0;
