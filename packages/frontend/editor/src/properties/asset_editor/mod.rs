//! Right-sidebar inspector for the *currently-selected asset(s)*.
//!
//! When `AppState::selected_assets` is non-empty, the right sidebar
//! swaps from "edit the selected node" to "edit the selected asset(s)".
//! Single-select routes to the per-source editor; multi-select routes
//! to a batch summary + Delete-selected action.
//!
//! Per-source editors (each in its own submodule):
//! - Material — `kind_editor::material::render_asset_material`. Lives
//!   alongside the inline-material editor in `kind_editor` because the
//!   same controls drive both the asset and inline paths.
//! - Mesh — [`mesh`]. Editable label, vertex/triangle stats from
//!   `mesh_cache`, plus the source-picker + Re-capture action.
//! - Texture — [`texture`]. Procedural Checker / Gradient / Noise
//!   editor with live cascade through `texture_cache::update_existing`.
//!   File-backed `Raster` falls through to a placeholder (raster
//!   textures are imported via the Environment tab).

mod batch;
mod dispatch;
mod mesh;
mod texture;

pub use batch::render_batch;
pub use dispatch::render;
