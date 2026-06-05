//! Top header bar. Two stacked rows:
//!
//! 1. **Top strip** — section tabs (Insert / Object / Assets /
//!    Environment / Camera / Editor) on the left, project actions
//!    (`New` / `Save` / `Load` / `Build` / `Undo` / `Redo` / `⋯`) on
//!    the right.
//! 2. **Action row** — buttons for the currently-active tab.
//!
//! The header is *layout only* — every button delegates to a function in
//! `crate::actions::*`. Reactive signals (`has_selection`, `dirty`,
//! `can_undo`, `can_redo`) come from `state::app_state()` and drive
//! enabled / disabled / accent styling.
//!
//! Per-section layout lives in sibling submodules so each section
//! file stays scannable.

pub(crate) mod assets;
mod camera;
mod environment;
#[allow(clippy::module_inception)]
mod header;
mod insert;
mod menu;
mod object;
mod project_label;
pub(crate) mod settings_drawer;
pub(crate) mod shadows_config;
mod stats;
mod top;

pub use header::Header;
