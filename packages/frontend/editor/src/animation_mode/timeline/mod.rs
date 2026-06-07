//! Animation-mode **timeline dock** (anim-timeline.jsx): the transport + ruler +
//! freeze-pane Dope Sheet that lives under the viewport. `dock::render()` is the
//! entry point (wired from `animation_mode::mod`).
//!
//! Geometry is shared across the dock's three views (Dope · Curves · Mixer) so
//! the ruler / playhead / column widths line up. For M-A3 only the **Dope Sheet**
//! is real; Curves + Mixer are present-but-inert segmented options that show a
//! small placeholder (they light up in M-A4/M-A5).
//!
//! Load-bearing rule (§0.2): every *animation/project* mutation is dispatched as
//! an `EditorCommand` through the one `EditorController`. Pure view chrome —
//! `px_per_sec` (zoom), the frames/seconds unit toggle, and per-track
//! expand/collapse — stays in local `Mutable`s (per §0.2, the same way the
//! timeline zoom is exempt). `anim_view` is controller state (so synced tabs
//! agree on the active view).

mod curves;
pub mod dock;
mod dope;
mod mixer;
mod ruler;
mod transport;

use crate::controller::animation::TrackTarget;
use crate::engine::scene::NodeId;

// ── shared geometry constants (mirror the JSX) ───────────────────────────────
/// Left names-column width (the freeze pane).
pub const NAMES_W: f64 = 248.0;
/// Ruler row height.
pub const RULER_H: f64 = 30.0;
/// Track (parent) row height.
pub const TRACK_H: f64 = 30.0;
/// Channel (expanded child) row height.
pub const CH_H: f64 = 23.0;

/// The display unit for the ruler + time readout (local view chrome).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TimeUnit {
    Frames,
    Seconds,
}

/// Shared timeline geometry derived from `px_per_sec` + the clip duration. Cheap
/// to copy; rebuilt whenever zoom/duration/unit/fps change.
#[derive(Clone, Copy)]
pub struct Geo {
    pub px_per_sec: f64,
    pub dur: f64,
    pub fps: u32,
    pub unit: TimeUnit,
    pub content_w: f64,
}

impl Geo {
    pub fn new(px_per_sec: f64, dur: f64, fps: u32, unit: TimeUnit) -> Self {
        let content_w = (dur * px_per_sec + 90.0).max(360.0);
        Self {
            px_per_sec,
            dur,
            fps,
            unit,
            content_w,
        }
    }
    /// Seconds → x (px from the start of the lanes content).
    pub fn time_to_x(&self, s: f64) -> f64 {
        s * self.px_per_sec
    }
    /// x (px from the start of the lanes content) → seconds.
    pub fn x_to_time(&self, x: f64) -> f64 {
        x / self.px_per_sec
    }
}

// ── time formatting ──────────────────────────────────────────────────────────

/// Format a time `t` (seconds) for the ruler / readout per the active unit.
pub fn fmt_time(t: f64, fps: u32, unit: TimeUnit) -> String {
    match unit {
        TimeUnit::Frames => (t * fps as f64).round().to_string(),
        TimeUnit::Seconds => format!("{t:.2}"),
    }
}

/// "Nice" major-tick spacing (seconds) targeting ~76px between major ticks.
pub fn nice_step_sec(px_per_sec: f64) -> f64 {
    const TARGET: f64 = 76.0; // px between major ticks
    let raw = TARGET / px_per_sec;
    const STEPS: [f64; 8] = [0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0];
    STEPS.into_iter().find(|&s| s >= raw).unwrap_or(10.0)
}

// ── track-target → label/icon/suffix (mirror inspector.rs) ───────────────────

/// The kind glyph (Lucide name) for a track's target.
pub fn target_icon(t: &TrackTarget) -> &'static str {
    match t {
        TrackTarget::Transform { .. } | TrackTarget::Morph { .. } => "cube",
        TrackTarget::Uniform { .. } | TrackTarget::BuiltinParam { .. } => "material",
        TrackTarget::Light { .. } => "light",
        TrackTarget::Camera { .. } => "camera",
    }
}

/// A short human label for the target object — the scene node's name (or the
/// custom material's name for a Uniform track), resolved live from the
/// controller. Falls back to a short id fragment if the target was deleted.
pub fn target_label(t: &TrackTarget) -> String {
    match t {
        TrackTarget::Transform { node, .. }
        | TrackTarget::Morph { node, .. }
        | TrackTarget::BuiltinParam { node, .. }
        | TrackTarget::Light { node, .. }
        | TrackTarget::Camera { node, .. } => node_label(node),
        TrackTarget::Uniform { material, .. } => {
            let ctrl = crate::controller::controller();
            let mats = ctrl.custom_materials.lock_ref();
            let name = mats
                .iter()
                .find(|m| m.id == *material)
                .map(|m| m.name.get_cloned());
            drop(mats);
            name.filter(|s| !s.is_empty())
                .unwrap_or_else(|| short_id(&material.to_string()))
        }
    }
}

/// The scene node's name, or a short id fragment if it's gone / unnamed.
fn node_label(node: &NodeId) -> String {
    crate::engine::scene::mutate::find_by_id(&crate::controller::controller().scene, *node)
        .map(|n| n.name.get_cloned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| short_id(&node.to_string()))
}

/// First segment of a UUID string (`"3d546f45-…"` → `"3d546f45"`) for a
/// readable fallback when a target has no resolvable name.
fn short_id(id: &str) -> String {
    id.split('-').next().unwrap_or(id).to_string()
}

/// The property this track drives (the second line of a track label).
pub fn prop_label(t: &TrackTarget) -> String {
    match t {
        TrackTarget::Transform { prop, .. } => format!("{prop:?}").to_lowercase(),
        TrackTarget::Morph { index, .. } => format!("morph {index}"),
        TrackTarget::Uniform { name, .. } => name.clone(),
        TrackTarget::BuiltinParam { param, .. } => format!("{param:?}").to_lowercase(),
        TrackTarget::Light { param, .. } => format!("{param:?}").to_lowercase(),
        TrackTarget::Camera { param, .. } => format!("{param:?}").to_lowercase(),
    }
}

/// The ` · uniform` / ` · morph` suffix shown after the prop on a track label.
pub fn prop_suffix(t: &TrackTarget) -> &'static str {
    match t {
        TrackTarget::Uniform { .. } | TrackTarget::BuiltinParam { .. } => " \u{00b7} uniform",
        TrackTarget::Morph { .. } => " \u{00b7} morph",
        _ => "",
    }
}

/// The components a track's keyframes carry — the label for the expanded lane /
/// the inspector's "Channels" row. `x · y · z` for a vec3 transform, `· w` added
/// for a rotation quaternion, `weight` for a morph, `value` for everything else.
pub fn channels_label(t: &TrackTarget) -> String {
    use crate::controller::animation::TransformProp;
    match t {
        TrackTarget::Transform {
            prop: TransformProp::Rotation,
            ..
        } => "x · y · z · w".into(),
        TrackTarget::Transform { .. } => "x · y · z".into(),
        TrackTarget::Morph { .. } => "weight".into(),
        _ => "value".into(),
    }
}
