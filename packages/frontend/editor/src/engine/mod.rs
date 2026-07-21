//! Engine layer — the UI-agnostic renderer/scene plumbing, adapted from the
//! archived editor. The `EditorController` calls into these; they are command
//! *implementations*, not driven directly by the UI. The scene model + renderer
//! bridge + actions land here as the panels that need them arrive (M4+).

pub mod activity;
pub mod activity_feed;
pub mod bridge;
pub mod camera_gizmos;
pub mod canvas;
pub mod canvas_host;
pub mod config;
pub mod context;
pub mod curve_handles;
pub mod gizmo;
pub mod grid;
pub mod light_icons;
pub mod preview;
pub mod query;
pub mod render_loop;
pub mod scene;
pub mod selection_box;
pub mod settings_sync;
pub mod skeleton_viz;
pub mod thumbnail;
