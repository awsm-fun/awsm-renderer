//! Engine layer — the UI-agnostic renderer/scene plumbing, adapted from the
//! archived editor. The `EditorController` calls into these; they are command
//! *implementations*, not driven directly by the UI. The scene model + renderer
//! bridge + actions land here as the panels that need them arrive (M4+).

pub mod bridge;
pub mod canvas;
pub mod config;
pub mod context;
pub mod environment;
pub mod gizmo;
pub mod grid;
pub mod preview;
pub mod render_loop;
pub mod scene;
pub mod selection_box;
