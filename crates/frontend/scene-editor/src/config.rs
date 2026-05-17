//! Build-time + UI-tuning constants for the editor. Anything that would
//! otherwise show up as a magic number (drag thresholds, indents) lives
//! here so it can be tweaked in one place.
//!
//! The CDN/media URLs are pulled from build-time env vars (see
//! `required_build_env!`) — same pattern as the rest of the frontend.

#![allow(dead_code)]
use std::sync::LazyLock;

use awsm_web_shared::required_build_env;

#[derive(Debug, Clone)]
pub struct Config {
    pub cdn_url: &'static str,
    pub media_url: &'static str,
    pub camera_focus_distance: f32,
    pub camera_aperture: f32,
}

impl Config {
    pub fn media_link(&self, path: &str) -> String {
        format!("{}/{}", self.media_url, path)
    }

    /// URL for shared editor assets on the CDN (e.g. `gizmo.glb`).
    pub fn editor_common_url(&self, path: &str) -> String {
        format!("{}/games/_editor-common/{}", self.cdn_url, path)
    }
}

pub static CONFIG: LazyLock<Config> = LazyLock::new(|| Config {
    cdn_url: required_build_env!("URL_CDN"),
    media_url: required_build_env!("URL_MEDIA"),
    camera_aperture: 5.6,
    camera_focus_distance: 10.0,
});

/// Keyboard shortcut bindings for the editor. Matched against
/// `web_sys::KeyboardEvent::key()` (case-sensitive on `key`; modifier
/// flags are boolean properties of the event).
pub mod keys {
    /// `Delete` / `Backspace` → delete the current selection.
    pub const DELETE: &[&str] = &["Delete", "Backspace"];
    /// `Escape` → clear selection or close the open popup/menu.
    pub const ESCAPE: &str = "Escape";
    /// `ArrowUp` / `ArrowDown` → move selection in the tree.
    pub const ARROW_UP: &str = "ArrowUp";
    pub const ARROW_DOWN: &str = "ArrowDown";
    /// `D` (with Ctrl or Meta) → duplicate the current selection.
    pub const DUPLICATE_KEY: &str = "d";
    /// `S` (with Ctrl or Meta) → save the current project.
    pub const SAVE_KEY: &str = "s";
}

/// Drag threshold in pixels before a pointer-down on a tree row is
/// treated as a drag rather than a click.
pub const TREE_DRAG_THRESHOLD_PX: f64 = 4.0;
/// Per-depth indentation inside the tree view.
pub const TREE_INDENT_PX: f64 = 16.0;
/// Height (in px) of a single tree row.
pub const TREE_ROW_HEIGHT_PX: f64 = 24.0;
