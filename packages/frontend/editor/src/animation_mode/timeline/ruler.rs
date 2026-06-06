//! Timeline **ruler** (anim-timeline.jsx `Ruler`): the tick strip with major-tick
//! spacing from [`nice_step_sec`], tick labels via [`fmt_time`], an end-of-clip
//! marker, and a draggable playhead handle. Click/drag on the ruler scrubs.
//!
//! Load-bearing rule (§0.2): scrubbing dispatches `SetPlayhead { t }`. Time is
//! computed from `clientX` relative to the ruler's `getBoundingClientRect().left`
//! (captured on mousedown), then clamped to `[0, dur]`.

use std::cell::Cell;
use std::rc::Rc;

use crate::controller::EditorCommand;
use crate::prelude::*;

use super::{fmt_time, nice_step_sec, Geo, RULER_H};

/// The ruler row (width = `geo.content_w`). Scrub drag is window-level.
pub fn render(geo: Geo) -> Dom {
    // Captured ruler-left (px, viewport coords) for the active scrub drag.
    let drag_left: Rc<Cell<Option<f64>>> = Rc::new(Cell::new(None));

    // Build the tick list. In frames mode the step snaps to a whole number of
    // frames so labels land on integer frames.
    let step = match geo.unit {
        super::TimeUnit::Frames => {
            let frames = (nice_step_sec(geo.px_per_sec) * geo.fps as f64)
                .round()
                .max(1.0);
            frames / geo.fps as f64
        }
        super::TimeUnit::Seconds => nice_step_sec(geo.px_per_sec),
    };
    let mut ticks: Vec<f64> = Vec::new();
    let mut s = 0.0;
    while s <= geo.dur + 1e-6 {
        ticks.push(s);
        s += step;
    }

    html!("div" => web_sys::HtmlElement, {
        .style("position", "relative")
        .style("width", &format!("{}px", geo.content_w)).style("height", &format!("{RULER_H}px"))
        .style("cursor", "ew-resize").style("background", "var(--bg-2)")
        .style("border-bottom", "1px solid var(--line)").style("user-select", "none")
        // mousedown captures the ruler-left + scrubs to the press point.
        .with_node!(el => {
            .event(clone!(drag_left => move |e: events::MouseDown| {
                let left = el.get_bounding_client_rect().left();
                drag_left.set(Some(left));
                scrub_to(e.x(), left, geo);
            }))
        })
        // window-level move/up while a drag is active.
        .global_event(clone!(drag_left => move |e: events::MouseMove| {
            if let Some(left) = drag_left.get() {
                scrub_to(e.x(), left, geo);
            }
        }))
        .global_event(clone!(drag_left => move |_: events::MouseUp| {
            drag_left.set(None);
        }))
        // ── ticks ────────────────────────────────────────────────────────────
        .children(ticks.into_iter().map(move |s| tick(s, geo)))
        // ── end-of-clip marker ───────────────────────────────────────────────
        .child(html!("div", {
            .style("position", "absolute").style("left", &format!("{}px", geo.time_to_x(geo.dur)))
            .style("top", "0").style("bottom", "0").style("width", "1px")
            .style("background", "var(--line-strong)")
        }))
        // ── playhead handle (tracks controller().playhead) ───────────────────
        .child(playhead_handle(geo))
    })
}

fn tick(s: f64, geo: Geo) -> Dom {
    html!("div", {
        .style("position", "absolute").style("left", &format!("{}px", geo.time_to_x(s)))
        .style("top", "0").style("bottom", "0").style("pointer-events", "none")
        .child(html!("div", {
            .style("position", "absolute").style("left", "0").style("bottom", "0")
            .style("width", "1px").style("height", "8px").style("background", "var(--line-strong)")
        }))
        .child(html!("span", {
            .class("mono")
            .style("position", "absolute").style("left", "4px").style("top", "6px")
            .style("font-size", "9.5px").style("color", "var(--text-3)").style("white-space", "nowrap")
            .text(&fmt_time(s, geo.fps, geo.unit))
        }))
    })
}

/// The accent playhead handle + descending line; `left` tracks the playhead time.
fn playhead_handle(geo: Geo) -> Dom {
    html!("div", {
        .style("position", "absolute").style("top", "0")
        .style("width", "12px").style("height", &format!("{RULER_H}px"))
        .style("pointer-events", "none")
        .style_signal("left", controller().playhead.signal().map(move |t| {
            format!("{}px", geo.time_to_x(t) - 6.0)
        }))
        .child(html!("div", {
            .style("position", "absolute").style("left", "1px").style("top", "2px")
            .style("width", "10px").style("height", "12px").style("border-radius", "2px")
            .style("background", "var(--accent-bright)").style("box-shadow", "var(--shadow-1)")
        }))
    })
}

/// Scrub the playhead to the viewport `client_x`, given the ruler-left + geometry.
fn scrub_to(client_x: f64, ruler_left: f64, geo: Geo) {
    let t = geo.x_to_time(client_x - ruler_left).clamp(0.0, geo.dur);
    dispatch(EditorCommand::SetPlayhead { t });
}

fn dispatch(cmd: EditorCommand) {
    spawn_local(async move {
        let _ = controller().dispatch(cmd).await;
    });
}
