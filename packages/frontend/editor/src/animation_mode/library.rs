//! Animation-mode **ClipLibrary** (anim-rail.jsx `ClipLibrary`): the reactive
//! list of authored clips with a "+ New clip" header action. Mirrors
//! `material_mode::library`.
//!
//! Load-bearing rule: row click → `SetCurrentClip`, "+" → `AddClip`,
//! both dispatched through the one `EditorController`. The UI never mutates the
//! clip library directly.

use std::sync::Arc;

use crate::controller::animation::CustomAnimation;
use crate::controller::{ClipLoop, EditorCommand};
use crate::prelude::*;

pub fn render() -> Dom {
    html!("div", {
        .style("display", "flex").style("flex-direction", "column").style("min-height", "0").style("height", "100%")
        // ── header (height 38) ───────────────────────────────────────────────
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("height", "38px")
            .style("padding", "0 10px 0 14px").style("border-bottom", "1px solid var(--line)").style("flex", "0 0 auto")
            .child(html!("span", {
                .style("font-size", "12.5px").style("font-weight", "620").style("color", "var(--text-0)").text("Animations")
            }))
            .child(html!("span", {
                .class("mono").style("margin-left", "8px").style("font-size", "10.5px").style("color", "var(--text-3)")
                .text_signal(controller().custom_animations.signal_vec_cloned().len().map(|n| n.to_string()))
            }))
            .child(html!("div", { .style("flex", "1") }))
            .child(IconBtn::new("plus").title("New clip").size(15.0)
                .on_click(|| dispatch(EditorCommand::AddClip { id: crate::engine::scene::AssetId::new() })).render())
        }))
        // ── scrollable list (gap 5, padding 8) ───────────────────────────────
        .child(html!("div", {
            .style("flex", "1").style("min-height", "0").style("overflow", "auto").style("padding", "8px")
            .style("display", "flex").style("flex-direction", "column").style("gap", "5px")
            .children_signal_vec(controller().custom_animations.signal_vec_cloned().map(clip_row))
            .child_signal(controller().custom_animations.signal_vec_cloned().len().map(|n| {
                if n == 0 {
                    Some(html!("div", {
                        .style("padding", "10px 4px").style("font-size", "12px").style("color", "var(--text-3)").style("line-height", "1.5")
                        .text("No clips yet. Create one to author keyframes.")
                    }))
                } else { None }
            }))
        }))
    })
}

fn clip_row(clip: Arc<CustomAnimation>) -> Dom {
    let id = clip.id;
    let on_border = controller()
        .current_clip
        .signal()
        .map(move |c| c == Some(id));
    let on_bg = controller()
        .current_clip
        .signal()
        .map(move |c| c == Some(id));
    let on_name_w = controller()
        .current_clip
        .signal()
        .map(move |c| c == Some(id));
    let on_name_c = controller()
        .current_clip
        .signal()
        .map(move |c| c == Some(id));

    html!("div", {
        .class("t")
        .style("padding", "8px 10px").style("border-radius", "var(--r2)").style("cursor", "pointer")
        .style("border-width", "1px").style("border-style", "solid")
        .style_signal("border-color", on_border.map(|on| if on { "var(--accent-line)" } else { "var(--line-soft)" }))
        .style_signal("background", on_bg.map(|on| if on { "var(--accent-ghost)" } else { "var(--bg-2)" }))
        // ── line 1: color dot · name · {dur}s ────────────────────────────────
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "8px")
            .child(html!("span", {
                .style("width", "8px").style("height", "8px").style("border-radius", "2px").style("flex", "0 0 auto")
                .style_signal("background", clip.color.signal_cloned())
            }))
            .child(html!("span", {
                .style("flex", "1").style("min-width", "0").style("font-size", "12.5px")
                .style("white-space", "nowrap").style("overflow", "hidden").style("text-overflow", "ellipsis")
                .style_signal("font-weight", on_name_w.map(|on| if on { "600" } else { "540" }))
                .style_signal("color", on_name_c.map(|on| if on { "var(--text-0)" } else { "var(--text-1)" }))
                .text_signal(clip.name.signal_cloned())
            }))
            .child(html!("span", {
                .class("mono").style("font-size", "10px").style("color", "var(--text-3)")
                .text_signal(clip.duration.signal().map(|d| format!("{d:.2}s")))
            }))
        }))
        // ── line 2: LOOP/PING-PONG/ONCE badge · {n} tracks · ·×speed ─────────
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "6px")
            .style("margin-top", "6px").style("white-space", "nowrap")
            .child_signal(map_ref! {
                let active = controller().current_clip.signal().map(move |c| c == Some(id)),
                let loop_style = clip.loop_style.signal() =>
                Some(badge(loop_label(*loop_style), if *active { Tone::Accent } else { Tone::Neutral }))
            })
            .child(html!("span", {
                .class("mono").style("font-size", "10px").style("color", "var(--text-3)").style("white-space", "nowrap")
                .text_signal(clip.tracks.signal_vec_cloned().len().map(|n| format!("{n} tracks")))
            }))
            .child_signal(clip.speed.signal().map(|s| {
                if (s - 1.0).abs() > f64::EPSILON {
                    Some(html!("span", {
                        .class("mono").style("font-size", "10px").style("color", "var(--text-3)").style("white-space", "nowrap")
                        .text(&format!("\u{00b7} \u{00d7}{s}"))
                    }))
                } else { None }
            }))
        }))
        .event(move |_: events::Click| dispatch(EditorCommand::SetCurrentClip { id: Some(id) }))
    })
}

fn loop_label(l: ClipLoop) -> &'static str {
    match l {
        ClipLoop::Loop => "LOOP",
        ClipLoop::PingPong => "PING-PONG",
        ClipLoop::Once => "ONCE",
    }
}

fn dispatch(cmd: EditorCommand) {
    spawn_local(async move {
        let _ = controller().dispatch(cmd).await;
    });
}
