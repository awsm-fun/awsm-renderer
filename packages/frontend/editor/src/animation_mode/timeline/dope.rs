//! Timeline **Dope Sheet** body (anim-timeline.jsx `NameCell` + `DopeLane` +
//! `EmptyTimeline`): the freeze-pane rows — a sticky-left names column + a
//! scrollable lanes area sharing one geometry. A track row (h=30) carries the
//! expand chevron · kind icon · two-line label · mute eye; expanded tracks add
//! channel rows (h=23). Lanes draw keyframe diamonds at `time_to_x(t)`; dragging
//! a diamond horizontally updates its keyframe time. A vertical playhead line
//! spans the body.
//!
//! Load-bearing rule (§0.2): mute → `SetTrackMute`, selection → `SetAnimSelection`,
//! diamond drag → `SetKeyframe { t }`. Per-track **expanded** is pure view chrome
//! (no command exists; set the live `Mutable` directly — §0.2 carve-out).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use crate::controller::animation::{AnimSel, CustomAnimation, Track};
use crate::controller::EditorCommand;
use crate::engine::scene::AssetId;
use crate::prelude::*;

use super::{
    channels_label, prop_label, prop_suffix, target_icon, target_label, Geo, CH_H, NAMES_W, TRACK_H,
};

/// The full Dope Sheet body (rows + playhead), or the empty-state when no tracks.
pub fn render(clip: Arc<CustomAnimation>, geo: Geo) -> Dom {
    html!("div", {
        .child_signal(clip.tracks.signal_vec_cloned().len().map(clone!(clip => move |n| {
            Some(if n == 0 {
                empty_state()
            } else {
                rows_body(clip.clone(), geo)
            })
        })))
    })
}

/// The track + channel rows over the shared geometry, with the body playhead.
fn rows_body(clip: Arc<CustomAnimation>, geo: Geo) -> Dom {
    html!("div", {
        .style("position", "relative")
        .children_signal_vec(
            clip.tracks.signal_vec_cloned()
                .enumerate()
                .map(clone!(clip => move |(idx, track)| {
                    let i = idx.get().unwrap_or(0);
                    track_block(clip.clone(), i, track, geo)
                }))
        )
        // vertical body playhead (at NAMES_W + t*pxPerSec) over the whole body.
        .child(body_playhead(geo))
    })
}

/// One track: its track row, then (when expanded) its channel rows.
fn track_block(clip: Arc<CustomAnimation>, idx: usize, track: Arc<Track>, geo: Geo) -> Dom {
    html!("div", {
        .child(row(clip.clone(), idx, track.clone(), geo, true))
        // expanded → channel rows. M-A3 shows a single "value" channel per track
        // (per-component channels arrive with the value editors in M-A4).
        .child_signal(track.expanded.signal().map(clone!(clip, track => move |open| {
            if open {
                Some(row(clip.clone(), idx, track.clone(), geo, false))
            } else {
                None
            }
        })))
    })
}

/// One freeze-pane row: a sticky name cell + a lane, sharing `geo`. `is_track`
/// selects the track-row chrome vs. the channel-row chrome.
fn row(clip: Arc<CustomAnimation>, idx: usize, track: Arc<Track>, geo: Geo, is_track: bool) -> Dom {
    html!("div", {
        .style("display", "flex")
        .child(html!("div", {
            .style("position", "sticky").style("left", "0").style("z-index", "5")
            .style("width", &format!("{NAMES_W}px")).style("flex", "0 0 auto")
            .style("background", "var(--bg-1)").style("border-right", "1px solid var(--line)")
            .child(if is_track {
                track_name_cell(clip.id, idx, &track)
            } else {
                channel_name_cell(&track)
            })
        }))
        .child(lane(clip.id, idx, track, geo, is_track))
    })
}

// ── name cells ───────────────────────────────────────────────────────────────

/// The track name cell (h=30): chevron · kind icon · two-line label · mute eye.
fn track_name_cell(clip: AssetId, idx: usize, track: &Arc<Track>) -> Dom {
    let target = track.target.clone();
    let icon = target_icon(&target);
    let label = target_label(&target);
    let prop = format!("{}{}", prop_label(&target), prop_suffix(&target));
    let mute = track.mute.clone();
    let expanded = track.expanded.clone();
    let selected = sel_signal(idx, None);

    html!("div", {
        .class("t")
        .style("display", "flex").style("align-items", "center").style("gap", "6px")
        .style("height", &format!("{TRACK_H}px")).style("padding", "0 8px")
        .style("cursor", "pointer").style("border-bottom", "1px solid var(--line-soft)")
        .style_signal("background", selected.map(|on| if on { "var(--accent-ghost)" } else { "transparent" }))
        .event(move |_: events::Click| {
            dispatch(EditorCommand::SetAnimSelection { sel: Some(AnimSel { track: idx, keyframe: None }) });
        })
        // expand chevron (rotates 90° when open) — pure view chrome.
        .child(html!("span", {
            .style("display", "flex").style("padding", "2px").style("cursor", "pointer")
            .event(clone!(expanded => move |e: events::Click| {
                e.stop_propagation();
                expanded.set(!expanded.get());
            }))
            .child_signal(expanded.signal().map(|open| {
                Some(Icon::new("chevron").size(12.0).color("var(--text-3)")
                    .style("transform", if open { "rotate(90deg)" } else { "none" })
                    .style("transition", "transform .14s")
                    .render())
            }))
        }))
        // kind icon
        .child_signal(mute.signal().map(move |m| {
            Some(Icon::new(icon).size(13.0).color(if m { "var(--text-3)" } else { "var(--text-2)" }).render())
        }))
        // two-line label (target name / prop + suffix)
        .child(html!("span", {
            .style("flex", "1").style("min-width", "0")
            .style("display", "flex").style("flex-direction", "column").style("line-height", "1.15")
            .child(html!("span", {
                .style("font-size", "12px").style("font-weight", "540")
                .style("white-space", "nowrap").style("overflow", "hidden").style("text-overflow", "ellipsis")
                .style_signal("color", mute.signal().map(|m| if m { "var(--text-3)" } else { "var(--text-0)" }))
                .text(&label)
            }))
            .child(html!("span", {
                .class("mono").style("font-size", "9.5px").style("color", "var(--text-3)")
                .text(&prop)
            }))
        }))
        // mute eye → SetTrackMute
        .child(html!("button", {
            .class("t")
            .attr("title", "Mute track")
            .style("width", "20px").style("height", "20px")
            .style("display", "flex").style("align-items", "center").style("justify-content", "center")
            .style("border", "1px solid transparent").style("background", "transparent").style("cursor", "pointer")
            .style("border-radius", "4px")
            .event(clone!(mute => move |e: events::Click| {
                e.stop_propagation();
                dispatch(EditorCommand::SetTrackMute { clip, track: idx, mute: !mute.get() });
            }))
            .child_signal(mute.signal().map(|m| {
                Some(Icon::new(if m { "eyeoff" } else { "eye" }).size(13.0)
                    .color(if m { "var(--text-3)" } else { "var(--text-2)" }).render())
            }))
        }))
    })
}

/// A channel name cell (h=23): color dot · channel name · key count. The label
/// names the components the track's keyframes carry (`x · y · z` for a vec3
/// transform, `x · y · z · w` for a rotation, `weight` for a morph, `value` for
/// a scalar) so the expanded lane isn't a mysterious "value" row.
fn channel_name_cell(track: &Arc<Track>) -> Dom {
    let keys = track.keys.clone();
    let channels = channels_label(&track.target);
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("gap", "8px")
        .style("height", &format!("{CH_H}px")).style("padding", "0 8px 0 30px")
        .style("border-bottom", "1px solid var(--line-soft)")
        .child(html!("span", {
            .style("width", "8px").style("height", "8px").style("border-radius", "2px")
            .style("flex", "0 0 auto").style("background", "var(--accent)")
        }))
        .child(html!("span", {
            .class("mono").style("flex", "1").style("font-size", "11px").style("color", "var(--text-1)")
            .text(&channels)
        }))
        .child(html!("span", {
            .class("mono").style("font-size", "9.5px").style("color", "var(--text-3)")
            .text_signal(keys.signal_ref(|k| k.len().to_string()))
        }))
    })
}

// ── lanes ────────────────────────────────────────────────────────────────────

/// A lane (width = content_w) drawing keyframe diamonds at `time_to_x(t)`.
fn lane(clip: AssetId, idx: usize, track: Arc<Track>, geo: Geo, is_track: bool) -> Dom {
    let h = if is_track { TRACK_H } else { CH_H };
    let mute = track.mute.clone();
    // Track rows dim when muted; channel rows are a solid recessed surface.
    let bg_track = is_track.then(clone!(mute => move || mute));
    html!("div", {
        .style("position", "relative")
        .style("width", &format!("{}px", geo.content_w)).style("height", &format!("{h}px"))
        .style("border-bottom", "1px solid var(--line-soft)")
        .apply(move |b| match bg_track {
            Some(mute) => b.style_signal("background", mute.signal().map(|m| {
                if m { "color-mix(in oklch, var(--bg-1) 70%, transparent)" } else { "transparent" }
            })),
            None => b.style("background", "var(--bg-3)"),
        })
        // clip-extent shading
        .child(html!("div", {
            .style("position", "absolute").style("left", "0").style("top", "0").style("bottom", "0")
            .style("width", &format!("{}px", geo.time_to_x(geo.dur)))
            .style("background", "oklch(1 0 0 / 0.012)")
        }))
        // keyframe diamonds — one per keyframe time. Rebuilt as a group whenever
        // `times` changes (dominator has no `children_signal`, so a `child_signal`
        // over a full-width container holds the diamonds). The container's left
        // edge == the lane content x=0, so diamond drag maps `clientX` to time.
        .child_signal(track.times.signal_cloned().map(clone!(mute => move |times| {
            Some(html!("div", {
                .style("position", "absolute").style("inset", "0")
                .children(times.into_iter().enumerate().map(clone!(mute => move |(key_idx, t)| {
                    diamond(clip, idx, key_idx, t, h, is_track, mute.clone(), geo)
                })))
            }))
        })))
    })
}

/// One keyframe diamond (rotate-45 square, ~9px) — selectable + horizontally
/// draggable (drag updates the keyframe time via `SetKeyframe`).
#[allow(clippy::too_many_arguments)]
fn diamond(
    clip: AssetId,
    track_idx: usize,
    key_idx: usize,
    t: f64,
    row_h: f64,
    is_track: bool,
    mute: Mutable<bool>,
    geo: Geo,
) -> Dom {
    let size = if is_track { 9.0 } else { 8.0 };
    // Captured lane-left (px, viewport coords) for the active drag.
    let drag_left: Rc<Cell<Option<f64>>> = Rc::new(Cell::new(None));
    html!("div" => web_sys::HtmlElement, {
        .style("position", "absolute")
        .style("left", &format!("{}px", geo.time_to_x(t)))
        .style("top", &format!("{}px", row_h / 2.0))
        .style("width", &format!("{size}px")).style("height", &format!("{size}px"))
        .style("cursor", "ew-resize").style("border-radius", "2px")
        .style("transform", "translate(-50%,-50%) rotate(45deg)")
        .style_signal("z-index", sel_signal(track_idx, Some(key_idx)).map(|s| if s { "3" } else { "2" }))
        .style_signal("background", clone!(mute => map_ref! {
            let m = mute.signal(),
            let s = sel_signal(track_idx, Some(key_idx)) =>
            if *s { "var(--text-0)" } else if *m { "var(--text-3)" } else { "var(--accent)" }
        }))
        .style_signal("border", sel_signal(track_idx, Some(key_idx)).map(|s| {
            if s { "1px solid var(--accent-bright)" } else { "1px solid color-mix(in oklch, var(--accent) 60%, black)" }
        }))
        .style_signal("box-shadow", sel_signal(track_idx, Some(key_idx)).map(|s| if s { "0 0 0 2px var(--accent-ghost)" } else { "none" }))
        // mousedown: select + capture lane-left for the drag.
        .with_node!(el => {
            .event(clone!(drag_left => move |e: events::MouseDown| {
                e.stop_propagation();
                dispatch(EditorCommand::SetAnimSelection {
                    sel: Some(AnimSel { track: track_idx, keyframe: Some(key_idx) }),
                });
                // The lane is the diamond's offset parent; its left edge anchors
                // the time mapping (the lane starts at content x = 0).
                let left = el.parent_element()
                    .map(|p| p.get_bounding_client_rect().left())
                    .unwrap_or(0.0);
                drag_left.set(Some(left));
            }))
        })
        .global_event(clone!(drag_left => move |e: events::MouseMove| {
            if let Some(left) = drag_left.get() {
                let nt = geo.x_to_time(e.x() - left).clamp(0.0, geo.dur);
                dispatch(EditorCommand::SetKeyframe {
                    clip,
                    track: track_idx,
                    index: key_idx,
                    t: Some(nt),
                    value: None,
                    interp: None,
                    in_tangent: None,
                    out_tangent: None,
                });
            }
        }))
        .global_event(clone!(drag_left => move |_: events::MouseUp| {
            drag_left.set(None);
        }))
    })
}

// ── playhead + empty state ───────────────────────────────────────────────────

/// The vertical body playhead line at `NAMES_W + t*pxPerSec`.
fn body_playhead(geo: Geo) -> Dom {
    html!("div", {
        .style("position", "absolute").style("top", "0").style("bottom", "0")
        .style("width", "1.5px").style("background", "var(--accent-bright)")
        .style("z-index", "6").style("pointer-events", "none")
        .style("box-shadow", "0 0 0 0.5px oklch(0 0 0 / 0.3)")
        .style_signal("left", controller().playhead.signal().map(move |t| {
            format!("{}px", NAMES_W + geo.time_to_x(t))
        }))
    })
}

/// The empty-state (no tracks): icon · headline · hint · Add Track button. The
/// button opens the target picker (`add_track::button`).
fn empty_state() -> Dom {
    html!("div", {
        .style("position", "sticky").style("left", "0").style("max-width", "100%")
        .style("display", "flex").style("flex-direction", "column")
        .style("align-items", "center").style("justify-content", "center")
        .style("gap", "12px").style("padding", "40px 0")
        .child(html!("div", {
            .style("width", "46px").style("height", "46px").style("border-radius", "var(--r3)")
            .style("display", "flex").style("align-items", "center").style("justify-content", "center")
            .style("background", "var(--bg-2)").style("border", "1px solid var(--line)")
            .child(Icon::new("curve").size(22.0).color("var(--text-3)").render())
        }))
        .child(html!("div", {
            .style("text-align", "center").style("line-height", "1.5")
            .child(html!("div", {
                .style("font-size", "13px").style("color", "var(--text-1)").style("font-weight", "540")
                .text("This clip has no tracks")
            }))
            .child(html!("div", {
                .style("font-size", "12px").style("color", "var(--text-3)").style("max-width", "320px")
                .text("Add a track to bind a bone, morph weight, or material uniform \u{2014} then key its value over time.")
            }))
        }))
        .child(super::super::add_track::button(BtnVariant::Primary, BtnSize::Sm))
    })
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// A signal of "is this (track, keyframe?) the current selection?".
fn sel_signal(track_idx: usize, keyframe: Option<usize>) -> impl Signal<Item = bool> {
    controller().anim_selection.signal().map(move |sel| {
        sel == Some(AnimSel {
            track: track_idx,
            keyframe,
        })
    })
}

fn dispatch(cmd: EditorCommand) {
    spawn_local(async move {
        let _ = controller().dispatch(cmd).await;
    });
}
