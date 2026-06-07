//! Timeline **transport** (anim-timeline.jsx `Transport`): the play controls
//! (to-start · prev-key · play/pause · next-key · to-end), a time readout that
//! toggles frames⇄seconds, a direction toggle, a loop toggle (LOOP→PP→ONCE) and
//! a speed slider. Heights ~28px, mono labels.
//!
//! Load-bearing rule (§0.2): play/pause/step + loop/direction/speed are all
//! `EditorCommand`s dispatched through the one controller. The frames/seconds
//! `unit` is pure view chrome (a local `Mutable` owned by the dock).

use std::sync::Arc;

use crate::controller::animation::{
    default_value_for, find_clip, ClipDirection, ClipLoop, CustomAnimation, StepKind,
};
use crate::controller::EditorCommand;
use crate::prelude::*;

use super::{fmt_time, TimeUnit};

/// The transport bar. `unit` is the dock's shared frames/seconds toggle.
pub fn render(unit: Mutable<TimeUnit>) -> Dom {
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("gap", "8px")
        // Reactive on the active clip — loop/direction/speed live on the clip.
        .child_signal(controller().current_clip.signal().map(move |id| {
            let clip = id.and_then(|id| find_clip(&controller().custom_animations, id));
            Some(body(clip, unit.clone()))
        }))
    })
}

fn body(clip: Option<Arc<CustomAnimation>>, unit: Mutable<TimeUnit>) -> Dom {
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("gap", "8px")
        // ── play-control button group ────────────────────────────────────────
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "2px")
            .style("padding", "2px").style("background", "var(--bg-3)")
            .style("border", "1px solid var(--line-soft)").style("border-radius", "var(--r2)")
            .child(transport_btn("To start", false, true, glyph_to_start(),
                || dispatch(EditorCommand::StepPlayhead { kind: StepKind::Home })))
            .child(transport_btn("Prev key", false, true, glyph_prev(),
                || dispatch(EditorCommand::StepPlayhead { kind: StepKind::Prev })))
            .child(play_pause_btn())
            .child(transport_btn("Next key", false, true, glyph_next(),
                || dispatch(EditorCommand::StepPlayhead { kind: StepKind::Next })))
            .child(transport_btn("To end", false, true, glyph_to_end(),
                || dispatch(EditorCommand::StepPlayhead { kind: StepKind::End })))
        }))
        // ── add-key button (inserts a keyframe on the selected track) ─────────
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center")
            .style("padding", "2px").style("background", "var(--bg-3)")
            .style("border", "1px solid var(--line-soft)").style("border-radius", "var(--r2)")
            .child(add_key_btn())
        }))
        // ── time readout (frames⇄seconds toggle) ─────────────────────────────
        .child(time_readout(clip.clone(), unit))
        // ── divider ──────────────────────────────────────────────────────────
        .child(html!("div", {
            .style("width", "1px").style("height", "18px").style("background", "var(--line)")
        }))
        // ── per-clip transport controls (only with a clip) ───────────────────
        .apply(|b| match clip {
            Some(clip) => b
                .child(direction_btn(&clip))
                .child(loop_btn(&clip))
                .child(speed_slider(&clip)),
            None => b,
        })
    })
}

// ── play controls ────────────────────────────────────────────────────────────

/// A 30×28 transport button: an inline SVG glyph that flips accent on `on`.
fn transport_btn(
    title: &str,
    on: bool,
    accent: bool,
    glyph: Dom,
    on_click: impl FnMut() + 'static,
) -> Dom {
    let hover = Mutable::new(false);
    let mut on_click = on_click;
    html!("button", {
        .class("t")
        .attr("title", title)
        .style("width", "30px").style("height", "28px")
        .style("display", "flex").style("align-items", "center").style("justify-content", "center")
        .style("border", "1px solid transparent").style("border-radius", "var(--r1)")
        .style("cursor", "pointer")
        .style("color", if on || accent { "var(--accent-bright)" } else { "var(--text-1)" })
        .style_signal("background", hover.signal().map(move |h| {
            if on { "var(--accent-ghost)" } else if h { "var(--bg-hover)" } else { "transparent" }
        }))
        .event(clone!(hover => move |_: events::MouseEnter| hover.set_neq(true)))
        .event(clone!(hover => move |_: events::MouseLeave| hover.set_neq(false)))
        .event(move |_: events::Click| on_click())
        .child(glyph)
    })
}

/// Insert a keyframe at the current playhead on the *selected* track. The
/// inserted value is sampled from the track so it lands on the existing curve
/// (a fresh track keys its target's default). No-op with a hint if no track is
/// selected. Delete is in the keyframe inspector.
fn add_key_btn() -> Dom {
    transport_btn(
        "Add keyframe at playhead (on the selected track)",
        false,
        true,
        glyph_diamond(),
        || {
            let ctrl = controller();
            let Some(clip_id) = ctrl.current_clip.get() else {
                Toast::info("No active clip");
                return;
            };
            let Some(sel) = ctrl.anim_selection.get() else {
                Toast::info("Select a track first, then add a key");
                return;
            };
            let t = ctrl.playhead.get();
            let value = {
                let Some(clip) = find_clip(&ctrl.custom_animations, clip_id) else {
                    return;
                };
                let tracks = clip.tracks.lock_ref();
                let Some(track) = tracks.get(sel.track) else {
                    return;
                };
                track
                    .sample_at(t)
                    .unwrap_or_else(|| default_value_for(&track.target))
            };
            dispatch(EditorCommand::AddKeyframe {
                clip: clip_id,
                track: sel.track,
                t,
                value,
            });
        },
    )
}

/// Play/pause toggle — reads `controller().playing`, dispatches `SetPlaying`.
fn play_pause_btn() -> Dom {
    let hover = Mutable::new(false);
    html!("button", {
        .class("t")
        .attr("title", "Play / pause")
        .style("width", "30px").style("height", "28px")
        .style("display", "flex").style("align-items", "center").style("justify-content", "center")
        .style("border", "1px solid transparent").style("border-radius", "var(--r1)")
        .style("cursor", "pointer").style("color", "var(--accent-bright)")
        .style_signal("background", map_ref! {
            let h = hover.signal(),
            let playing = controller().playing.signal() =>
            if *playing { "var(--accent-ghost)" } else if *h { "var(--bg-hover)" } else { "transparent" }
        })
        .event(clone!(hover => move |_: events::MouseEnter| hover.set_neq(true)))
        .event(clone!(hover => move |_: events::MouseLeave| hover.set_neq(false)))
        .event(|_: events::Click| {
            let on = !controller().playing.get();
            dispatch(EditorCommand::SetPlaying { on });
        })
        .child_signal(controller().playing.signal().map(|p| {
            Some(if p { glyph_pause() } else { glyph_play() })
        }))
    })
}

// ── time readout ─────────────────────────────────────────────────────────────

/// `{t} / {dur} f|s` button — toggles the frames/seconds `unit` on click.
fn time_readout(clip: Option<Arc<CustomAnimation>>, unit: Mutable<TimeUnit>) -> Dom {
    html!("button", {
        .class("t").class("mono")
        .attr("title", "Toggle frames / seconds")
        .style("display", "flex").style("align-items", "baseline").style("gap", "5px")
        .style("height", "28px").style("padding", "0 10px").style("border-radius", "var(--r2)")
        .style("cursor", "pointer").style("border", "1px solid var(--line-soft)")
        .style("background", "var(--bg-3)").style("color", "var(--text-0)")
        .event(clone!(unit => move |_: events::Click| {
            unit.set(match unit.get() {
                TimeUnit::Frames => TimeUnit::Seconds,
                TimeUnit::Seconds => TimeUnit::Frames,
            });
        }))
        // current time
        .child(html!("span", {
            .style("font-size", "13px").style("font-weight", "600")
            .text_signal(map_ref! {
                let t = controller().playhead.signal(),
                let fps = controller().anim_fps.signal(),
                let u = unit.signal() =>
                fmt_time(*t, *fps, *u)
            })
        }))
        // / duration + unit suffix
        .child(html!("span", {
            .style("font-size", "9.5px").style("color", "var(--text-3)")
            .text_signal(map_ref! {
                let dur = dur_signal(&clip),
                let fps = controller().anim_fps.signal(),
                let u = unit.signal() =>
                format!("/ {} {}", fmt_time(*dur, *fps, *u), match u { TimeUnit::Frames => "f", TimeUnit::Seconds => "s" })
            })
        }))
    })
}

/// The active clip's duration as a signal (0 when no clip).
fn dur_signal(clip: &Option<Arc<CustomAnimation>>) -> impl Signal<Item = f64> {
    use futures_signals::signal::always;
    match clip {
        Some(c) => c.duration.signal().boxed_local(),
        None => always(0.0).boxed_local(),
    }
}

// ── direction / loop / speed ─────────────────────────────────────────────────

/// Direction toggle (Forward ⇄ Reverse) — `redo`/`undo` glyph, accent on reverse.
fn direction_btn(clip: &Arc<CustomAnimation>) -> Dom {
    let id = clip.id;
    let dir = clip.direction.clone();
    let rev = dir.signal().map(|d| d == ClipDirection::Reverse);
    let hover = Mutable::new(false);
    html!("button", {
        .class("t")
        .attr("title", "Playback direction")
        .style("width", "28px").style("height", "28px")
        .style("display", "flex").style("align-items", "center").style("justify-content", "center")
        .style("border", "1px solid transparent").style("border-radius", "var(--r2)")
        .style("cursor", "pointer")
        .style_signal("color", dir.signal().map(|d| if d == ClipDirection::Reverse { "var(--accent-bright)" } else { "var(--text-1)" }))
        .style_signal("background", map_ref! {
            let h = hover.signal(),
            let r = rev =>
            if *r { "var(--accent-ghost)" } else if *h { "var(--bg-hover)" } else { "transparent" }
        })
        .event(clone!(hover => move |_: events::MouseEnter| hover.set_neq(true)))
        .event(clone!(hover => move |_: events::MouseLeave| hover.set_neq(false)))
        .event(clone!(dir => move |_: events::Click| {
            let next = match dir.get() {
                ClipDirection::Forward => ClipDirection::Reverse,
                ClipDirection::Reverse => ClipDirection::Forward,
            };
            dispatch(EditorCommand::SetClipDirection { id, direction: next });
        }))
        .child_signal(dir.signal().map(|d| {
            Some(Icon::new(if d == ClipDirection::Forward { "redo" } else { "undo" }).size(14.0).render())
        }))
    })
}

/// Loop toggle cycling LOOP → PING-PONG → ONCE.
fn loop_btn(clip: &Arc<CustomAnimation>) -> Dom {
    let id = clip.id;
    let style = clip.loop_style.clone();
    let is_once = style.signal().map(|l| l == ClipLoop::Once);
    html!("button", {
        .class("t")
        .attr("title", "Loop mode")
        .style("display", "flex").style("align-items", "center").style("gap", "5px")
        .style("height", "28px").style("padding", "0 9px").style("border-radius", "var(--r2)")
        .style("cursor", "pointer").style("border", "1px solid var(--line-soft)")
        .style_signal("background", style.signal().map(|l| if l != ClipLoop::Once { "var(--accent-ghost)" } else { "var(--bg-3)" }))
        .style_signal("color", is_once.map(|once| if once { "var(--text-1)" } else { "var(--accent-bright)" }))
        .event(clone!(style => move |_: events::Click| {
            let next = match style.get() {
                ClipLoop::Loop => ClipLoop::PingPong,
                ClipLoop::PingPong => ClipLoop::Once,
                ClipLoop::Once => ClipLoop::Loop,
            };
            dispatch(EditorCommand::SetClipLoop { id, loop_style: next });
        }))
        .child(Icon::new("reset").size(13.0).render())
        .child(html!("span", {
            .class("mono").style("font-size", "10px")
            .text_signal(style.signal().map(|l| loop_short(l).to_string()))
        }))
    })
}

fn loop_short(l: ClipLoop) -> &'static str {
    match l {
        ClipLoop::Loop => "LOOP",
        ClipLoop::PingPong => "PP",
        ClipLoop::Once => "ONCE",
    }
}

/// Speed slider (0.1–3, ×) — dispatches `SetClipSpeed` on input.
fn speed_slider(clip: &Arc<CustomAnimation>) -> Dom {
    let id = clip.id;
    let speed = clip.speed.clone();
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("gap", "5px")
        .style("height", "28px").style("padding", "0 8px").style("border-radius", "var(--r2)")
        .style("border", "1px solid var(--line-soft)").style("background", "var(--bg-3)")
        .child(html!("span", {
            .class("mono").style("font-size", "10px").style("color", "var(--text-3)").text("\u{00d7}")
        }))
        .child(html!("input" => web_sys::HtmlInputElement, {
            .attr("type", "range").attr("min", "0.1").attr("max", "3").attr("step", "0.1")
            .prop_signal("value", speed.signal().map(|s| format!("{s}")))
            .style("width", "56px").style("accent-color", "var(--accent)").style("height", "3px")
            .with_node!(input => {
                .event(move |_: events::Input| {
                    if let Ok(v) = input.value().parse::<f64>() {
                        dispatch(EditorCommand::SetClipSpeed { id, speed: v });
                    }
                })
            })
        }))
        .child(html!("span", {
            .class("mono").style("font-size", "10.5px").style("color", "var(--text-1)").style("width", "22px")
            .text_signal(speed.signal().map(|s| format!("{s:.1}")))
        }))
    })
}

// ── inline transport glyphs (12×12 viewbox-16 filled SVGs) ───────────────────

fn tri(children: Vec<Dom>) -> Dom {
    svg!("svg", {
        .attr("width", "13").attr("height", "13").attr("viewBox", "0 0 16 16").attr("fill", "currentColor")
        .attr("style", "display:block")
        .children(children)
    })
}
fn rect(x: &str, y: &str, w: &str, h: &str) -> Dom {
    svg!("rect", { .attr("x", x).attr("y", y).attr("width", w).attr("height", h) })
}
fn path(d: &str) -> Dom {
    svg!("path", { .attr("d", d) })
}

fn glyph_to_start() -> Dom {
    tri(vec![rect("3", "3", "2", "10"), path("M13 3L6 8l7 5z")])
}
fn glyph_prev() -> Dom {
    tri(vec![rect("3", "3", "2", "10"), path("M12 3L5 8l7 5z")])
}
fn glyph_next() -> Dom {
    tri(vec![path("M4 3l7 5-7 5z"), rect("11", "3", "2", "10")])
}
fn glyph_to_end() -> Dom {
    tri(vec![path("M3 3l7 5-7 5z"), rect("11", "3", "2", "10")])
}
fn glyph_play() -> Dom {
    tri(vec![path("M4 3l9 5-9 5z")])
}
fn glyph_pause() -> Dom {
    tri(vec![rect("4", "3", "3", "10"), rect("9", "3", "3", "10")])
}
/// A keyframe diamond (the "add key" affordance).
fn glyph_diamond() -> Dom {
    tri(vec![path("M8 2l6 6-6 6-6-6z")])
}

fn dispatch(cmd: EditorCommand) {
    spawn_local(async move {
        let _ = controller().dispatch(cmd).await;
    });
}
