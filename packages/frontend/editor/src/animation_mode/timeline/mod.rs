//! Animation-mode **timeline dock**: the transport + ruler + freeze-pane views
//! that live under the viewport. `dock::render()` is the entry point (wired from
//! the animation-mode workspace).
//!
//! Geometry + track-target labelling are shared across the dock's three (all
//! live) views — Dope · Curves · Mixer — so the ruler / playhead / column widths
//! line up; see [`shared`].
//!
//! Load-bearing rule: every animation/project mutation is dispatched as an
//! `EditorCommand` through the one `EditorController`. Pure view chrome —
//! `px_per_sec` (zoom), the frames/seconds unit toggle, and per-track
//! expand/collapse — stays in local `Mutable`s; `anim_view` is controller state
//! (so synced tabs agree on the active view).

mod curves;
pub mod dock;
mod dope;
mod mixer;
mod ruler;
mod shared;
mod transport;

pub use shared::{
    channels_label, fmt_time, nice_step_sec, prop_label, prop_suffix, target_icon, target_label,
    Geo, TimeUnit, CH_H, NAMES_W, RULER_H, TRACK_H,
};
