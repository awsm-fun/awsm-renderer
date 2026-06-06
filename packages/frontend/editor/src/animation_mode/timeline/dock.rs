//! Timeline **dock shell** (anim-timeline.jsx `TimelineDock`): the header
//! (transport · Dope/Curves/Mixer segmented · zoom buttons) over a freeze-pane
//! scroller (sticky ruler row + the active view's body). `render()` is the
//! Animation-mode entry point.
//!
//! Load-bearing rule (§0.2): the active view is controller state (`anim_view`,
//! so synced tabs agree) — the segmented drives it via `SetAnimView`. `px_per_sec`
//! (zoom) + the frames/seconds unit are pure view chrome (local `Mutable`s).
//!
//! M-A3 implements only the **Dope Sheet** body. Curves + Mixer are present but
//! inert segmented options that show a small placeholder; they light up in
//! M-A4/M-A5.

use std::sync::Arc;

use crate::controller::animation::{find_clip, AnimView, CustomAnimation};
use crate::controller::EditorCommand;
use crate::prelude::*;

use super::{dope, ruler, transport, Geo, TimeUnit, NAMES_W, RULER_H};

/// Zoom bounds + step (px-per-second), mirroring the JSX zoom buttons.
const PX_MIN: f64 = 40.0;
const PX_MAX: f64 = 900.0;
const PX_DEFAULT: f64 = 190.0;
const ZOOM_FACTOR: f64 = 1.3;

/// The timeline dock. Owns the view-chrome `Mutable`s (zoom + unit).
pub fn render() -> Dom {
    let px_per_sec = Mutable::new(PX_DEFAULT);
    let unit = Mutable::new(TimeUnit::Seconds);

    html!("div", {
        .style("display", "flex").style("flex-direction", "column")
        .style("height", "100%").style("min-height", "0").style("background", "var(--bg-1)")
        // ── dock header ──────────────────────────────────────────────────────
        .child(header(px_per_sec.clone(), unit.clone()))
        // ── freeze-pane scroller ─────────────────────────────────────────────
        .child(html!("div", {
            .style("flex", "1").style("min-height", "0").style("overflow", "auto").style("position", "relative")
            // Reactive on (active clip, zoom, unit, fps): rebuild the geometry +
            // body whenever any changes.
            .child_signal(map_ref! {
                let clip_id = controller().current_clip.signal(),
                let px = px_per_sec.signal(),
                let u = unit.signal(),
                let fps = controller().anim_fps.signal() => {
                    let clip = clip_id.and_then(|id| find_clip(&controller().custom_animations, id));
                    Some(scroller(clip, *px, *u, *fps))
                }
            })
        }))
    })
}

// ── header ───────────────────────────────────────────────────────────────────

fn header(px_per_sec: Mutable<f64>, unit: Mutable<TimeUnit>) -> Dom {
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("gap", "10px")
        .style("height", "46px").style("padding", "0 12px")
        .style("border-bottom", "1px solid var(--line)").style("flex", "0 0 auto")
        // transport (left)
        .child(transport::render(unit))
        .child(html!("div", { .style("flex", "1") }))
        // Dope / Curves / Mixer segmented (driven by anim_view)
        .child(view_segmented())
        // divider
        .child(html!("div", {
            .style("width", "1px").style("height", "18px").style("background", "var(--line)")
        }))
        // zoom out / fit / in
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "4px")
            .child(IconBtn::new("minus").title("Zoom out")
                .on_click(clone!(px_per_sec => move || {
                    px_per_sec.set((px_per_sec.get() / ZOOM_FACTOR).max(PX_MIN));
                })).render())
            .child(IconBtn::new("search").title("Fit / reset zoom")
                .on_click(clone!(px_per_sec => move || px_per_sec.set(PX_DEFAULT))).render())
            .child(IconBtn::new("plus").title("Zoom in")
                .on_click(clone!(px_per_sec => move || {
                    px_per_sec.set((px_per_sec.get() * ZOOM_FACTOR).min(PX_MAX));
                })).render())
        }))
    })
}

/// The Dope/Curves/Mixer segmented, two-way-bound to the controller's `anim_view`.
fn view_segmented() -> Dom {
    // Local string mirror: seed from anim_view, dispatch SetAnimView on user
    // change (skip the seed), and reflect external anim_view changes back in.
    let value = Mutable::new(view_key(controller().anim_view.get()).to_string());

    // user change → dispatch (skip the seed).
    spawn_local(clone!(value => async move {
        let mut first = true;
        value.signal_cloned().for_each(move |k| {
            let fire = !first;
            first = false;
            let view = view_from_key(&k);
            async move {
                if fire && controller().anim_view.get() != view {
                    dispatch(EditorCommand::SetAnimView { view });
                }
            }
        }).await;
    }));
    // external anim_view change → reflect into the mirror.
    spawn_local(clone!(value => async move {
        controller().anim_view.signal().for_each(move |v| {
            value.set_neq(view_key(v).to_string());
            async {}
        }).await;
    }));

    segmented(
        value,
        vec![
            SegOption::new("dope", "Dope Sheet").icon("sliders"),
            SegOption::new("curves", "Curves").icon("curve"),
            SegOption::new("mixer", "Mixer").icon("layers"),
        ],
        true,
        false,
    )
}

// ── scroller (geometry + sticky header + body) ───────────────────────────────

fn scroller(clip: Option<Arc<CustomAnimation>>, px: f64, unit: TimeUnit, fps: u32) -> Dom {
    let Some(clip) = clip else {
        return no_clip();
    };
    let geo = Geo::new(px, clip.duration.get(), fps, unit);

    html!("div", {
        .style("position", "relative")
        .style("width", &format!("{}px", NAMES_W + geo.content_w)).style("min-height", "100%")
        // ── sticky header row: left names header + ruler ──────────────────────
        .child(html!("div", {
            .style("display", "flex").style("position", "sticky").style("top", "0")
            .style("z-index", "8").style("height", &format!("{RULER_H}px"))
            .child(html!("div", {
                .style("position", "sticky").style("left", "0").style("z-index", "9")
                .style("width", &format!("{NAMES_W}px")).style("flex", "0 0 auto").style("height", &format!("{RULER_H}px"))
                .style("display", "flex").style("align-items", "center").style("gap", "7px")
                .style("padding", "0 8px 0 12px").style("background", "var(--bg-2)")
                .style("border-bottom", "1px solid var(--line)").style("border-right", "1px solid var(--line)")
                .child(html!("span", { .class("kicker").style("font-size", "9.5px").text("Tracks") }))
                .child(html!("div", { .style("flex", "1") }))
                .child(IconBtn::new("plus").title("Add track").size(14.0)
                    .on_click(|| {
                        // TODO(M-A6): open the node/property target picker + AddTrack.
                        tracing::info!("add-track picker lands in M-A6");
                    }).render())
            }))
            .child(ruler::render(geo))
        }))
        // ── body: the active view (Dope real; Curves/Mixer placeholders) ──────
        .child_signal(controller().anim_view.signal().map(clone!(clip => move |view| {
            Some(match view {
                AnimView::Dope => dope::render(clip.clone(), geo),
                AnimView::Curves => placeholder("Curves \u{2014} M-A4"),
                AnimView::Mixer => placeholder("Mixer \u{2014} M-A5"),
            })
        })))
    })
}

/// The no-clip-selected empty state for the dock body.
fn no_clip() -> Dom {
    html!("div", {
        .style("position", "absolute").style("inset", "0")
        .style("display", "flex").style("align-items", "center").style("justify-content", "center")
        .child(html!("span", {
            .style("font-size", "12px").style("color", "var(--text-3)")
            .text("No clip selected \u{2014} create one in the library to author it.")
        }))
    })
}

/// A small inert placeholder for the not-yet-built views (Curves/Mixer).
fn placeholder(label: &str) -> Dom {
    html!("div", {
        .style("position", "sticky").style("left", "0").style("max-width", "100%")
        .style("display", "flex").style("align-items", "center").style("justify-content", "center")
        .style("padding", "40px 0").style("font-size", "12.5px").style("color", "var(--text-3)")
        .text(label)
    })
}

// ── AnimView ⇄ segmented key ──────────────────────────────────────────────────

fn view_key(v: AnimView) -> &'static str {
    match v {
        AnimView::Dope => "dope",
        AnimView::Curves => "curves",
        AnimView::Mixer => "mixer",
    }
}
fn view_from_key(k: &str) -> AnimView {
    match k {
        "curves" => AnimView::Curves,
        "mixer" => AnimView::Mixer,
        _ => AnimView::Dope,
    }
}

fn dispatch(cmd: EditorCommand) {
    spawn_local(async move {
        let _ = controller().dispatch(cmd).await;
    });
}
