//! Kind-specific inspector. For single-selection only.
//!
//! Variant switching (Point ↔ Spot, Box ↔ Sphere) is deferred — it's
//! editable polish. Today you delete + re-insert with the desired variant.

pub mod camera;
pub mod curve;
pub mod decal;
pub mod instances;
pub mod light_shadow;
pub mod line;
pub mod material;
pub mod mesh;
pub mod mesh_shadow;
pub mod particle;
pub mod primitive;
pub mod sprite;
pub mod sweep;

mod collider;
mod dispatch;
mod helpers;
mod light;
mod model;
mod pickers;

pub use dispatch::render;

// Re-exports so sibling submodules can keep their `use super::{...}`
// imports (and `super::xxx(...)` call paths) unchanged. Helpers + pickers
// are also reached from neighbouring `properties::*` modules
// (e.g. `asset_editor::texture` calls `kind_editor::field_row`), hence
// the broader `pub(crate)` visibility on the items themselves.
pub(crate) use helpers::{capture_as_mesh_button, field_row, section_header};
pub(crate) use pickers::{
    collect_nodes_matching, material_ref_select, node_id_select, texture_ref_select,
};
