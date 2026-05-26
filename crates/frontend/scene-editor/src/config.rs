//! UI-tuning constants for the editor. Anything that would otherwise
//! show up as a magic number (drag thresholds, indents) lives here so it
//! can be tweaked in one place.
//!
//! The gizmo asset is shipped with the editor and copied into the dist
//! by Trunk (`<link data-trunk rel="copy-dir" href="assets" …>` in
//! `index.html`), so the runtime fetch is a path relative to the
//! editor's own deploy — no environment variable, no separate media
//! server required for the editor to come up cleanly.

use std::sync::LazyLock;

#[derive(Debug, Clone)]
pub struct Config {
    pub camera_focus_distance: f32,
    pub camera_aperture: f32,
    /// Only read in debug builds via `load_external_test_scene`
    /// (which is itself `#[cfg(debug_assertions)]`). Kept on the
    /// struct unconditionally so the `LazyLock` initializer below
    /// stays a single literal regardless of build profile —
    /// `dead_code` is silenced for the release case.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub media_base_url_additional_assets: String,
    /// Base URL of the sibling `material-editor` frontend. Wired by
    /// the same env-var pattern as the additional-assets media base —
    /// `URL_MATERIAL_EDITOR` from `taskfiles/frontend/scene-editor.yml`.
    /// In dev it's `http://localhost:9084`; in prod it's
    /// `https://dakom.github.io/awsm-renderer/material-editor/`. The
    /// "Open in material-editor" link in the Custom Materials pane
    /// builds against this base.
    pub url_material_editor: String,
}

impl Config {
    /// URL for the editor's bundled gltf gizmo asset. Relative — resolves
    /// against the page's base URL whether dev (`localhost:9081/`) or
    /// prod (`/awsm-renderer/scene-editor/`).
    pub fn gizmo_url(&self) -> &'static str {
        "assets/gizmo.glb"
    }

    /// Base URL for the `awsm-renderer-assets` repo. Used by the
    /// debug-only `load_external_test_scene` wasm export so test
    /// fixtures live in one canonical place (the sibling assets repo)
    /// instead of being duplicated into the editor's dist. Set at
    /// build time via the `MEDIA_BASE_URL_ADDITIONAL_ASSETS` env var
    /// in `taskfiles/frontend/scene-editor.yml`. Dev points at
    /// `http://localhost:9083` (the media-additional-assets server);
    /// prod points at `https://dakom.github.io/awsm-renderer-assets`.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub fn media_base_url_additional_assets(&self) -> &str {
        &self.media_base_url_additional_assets
    }

    /// Returns the material-editor frontend base URL.
    pub fn url_material_editor(&self) -> &str {
        &self.url_material_editor
    }
}

#[allow(clippy::option_env_unwrap)]
pub static CONFIG: LazyLock<Config> = LazyLock::new(|| Config {
    camera_aperture: 5.6,
    camera_focus_distance: 10.0,
    media_base_url_additional_assets: option_env!("MEDIA_BASE_URL_ADDITIONAL_ASSETS")
        .expect(
            "MEDIA_BASE_URL_ADDITIONAL_ASSETS must be set — see \
             `taskfiles/frontend/scene-editor.yml`",
        )
        .to_string(),
    url_material_editor: option_env!("URL_MATERIAL_EDITOR")
        .expect(
            "URL_MATERIAL_EDITOR must be set — see \
             `taskfiles/frontend/scene-editor.yml`",
        )
        .to_string(),
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
