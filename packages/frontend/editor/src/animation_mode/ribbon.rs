//! Animation-mode **ribbon** (anim-app.jsx, the bar right after the top bar):
//! the active clip's color chip · name · Duration · FPS, a `N tracks → N players`
//! readout, an **Add Track** ghost button, and a green **Live · N players** chip.
//!
//! Load-bearing rule (§0.2): every mutation is an `EditorCommand` dispatched
//! through the one `EditorController` (`controller().dispatch(..)` via
//! `spawn_local`). The fields here are local `Mutable` mirrors that dispatch on
//! change while skipping the first emission (the seed) — mirroring the
//! field-binding pattern in `material_mode`.

use std::sync::Arc;

use crate::controller::animation::{find_clip, CustomAnimation};
use crate::prelude::*;

pub fn render() -> Dom {
    html!("div", {
        .style("display", "flex").style("align-items", "center").style("gap", "10px")
        .style("height", "46px").style("padding", "0 12px")
        .style("background", "var(--bg-1)").style("border-bottom", "1px solid var(--line)")
        .style("flex", "0 0 auto")
        // The whole ribbon body is reactive on the active clip — rebuilds when the
        // current clip changes (the per-field reactivity lives inside `body`).
        .child_signal(controller().current_clip.signal().map(|id| {
            Some(body(id.and_then(|id| find_clip(&controller().custom_animations, id))))
        }))
    })
}

/// The ribbon controls for `clip` (or an empty hint when no clip is selected).
fn body(clip: Option<Arc<CustomAnimation>>) -> Dom {
    let Some(clip) = clip else {
        return html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "10px")
            .style("flex", "1")
            .child(html!("span", {
                .style("font-size", "12px").style("color", "var(--text-3)")
                .text("No clip selected — create one in the library to author it.")
            }))
        });
    };

    html!("div", {
        .style("display", "contents")
        // ── color chip (9×9, radius 2) ───────────────────────────────────────
        .child(html!("span", {
            .style("width", "9px").style("height", "9px").style("border-radius", "2px").style("flex", "0 0 auto")
            .style_signal("background", clip.color.signal_cloned())
        }))
        // ── clip name (~168px) ───────────────────────────────────────────────
        .child(html!("div", { .style("width", "168px")
            .child(name_field(&clip))
        }))
        // ── Duration kicker + NumField (~86px) ───────────────────────────────
        .child(html!("span", { .class("kicker").style("margin-left", "4px").text("Duration") }))
        .child(html!("div", { .style("width", "86px")
            .child(duration_field(&clip))
        }))
        // ── FPS kicker + Select (~78px) ──────────────────────────────────────
        .child(html!("span", { .class("kicker").text("FPS") }))
        .child(html!("div", { .style("width", "78px")
            .child(fps_field())
        }))
        // ── spacer ───────────────────────────────────────────────────────────
        .child(html!("div", { .style("flex", "1") }))
        // ── N tracks → N players readout (mono) ──────────────────────────────
        .child(html!("span", {
            .class("mono").style("font-size", "11px").style("color", "var(--text-2)").style("white-space", "nowrap")
            .text_signal(clip.tracks.signal_vec_cloned().len().map(|n| {
                format!("{n} tracks \u{2192} {n} player{}", if n == 1 { "" } else { "s" })
            }))
        }))
        // ── vertical divider ─────────────────────────────────────────────────
        .child(html!("span", {
            .style("width", "1px").style("height", "18px").style("background", "var(--line)")
        }))
        // ── Add Track (ghost) — picker lands in M-A6 ─────────────────────────
        .child(Btn::new().label("Add Track").icon("target").variant(BtnVariant::Ghost).size(BtnSize::Sm)
            .on_click(|| {
                // TODO(M-A6): open the real node/property target picker (anim-rail.jsx
                // AddTrackMenu) and dispatch `EditorCommand::AddTrack { clip, target }`.
                tracing::info!("add-track picker lands in M-A6");
            })
            .render())
        // ── Live · N players chip ────────────────────────────────────────────
        .child(live_chip(&clip))
    })
}

/// Clip name input — dispatches `RenameClip` on change.
fn name_field(clip: &Arc<CustomAnimation>) -> Dom {
    let id = clip.id;
    // A local mirror seeded from the clip; dispatch on user edits.
    let value = Mutable::new(clip.name.get_cloned());
    bind_string(&value, move |name| EditorCommand::RenameClip { id, name });
    TextInput::new(value).placeholder("Clip name").render()
}

/// Clip duration field — dispatches `SetClipDuration` on change.
fn duration_field(clip: &Arc<CustomAnimation>) -> Dom {
    let id = clip.id;
    NumField::new(clip.duration.get())
        .min(0.1)
        .step(0.1)
        .suffix("s")
        .on_change(move |duration| dispatch(EditorCommand::SetClipDuration { id, duration }))
        .render()
}

/// Display FPS select — dispatches `SetAnimFps` on change. FPS is global editor
/// state (`anim_fps`), not per-clip.
fn fps_field() -> Dom {
    let value = Mutable::new(controller().anim_fps.get().to_string());
    spawn_local(clone!(value => async move {
        let mut first = true;
        value.signal_cloned().for_each(move |v| {
            let fire = !first;
            first = false;
            async move {
                if fire {
                    if let Ok(fps) = v.parse::<u32>() {
                        dispatch(EditorCommand::SetAnimFps { fps });
                    }
                }
            }
        }).await;
    }));
    select(
        value,
        ["12", "24", "30", "60"]
            .iter()
            .map(|v| (v.to_string(), v.to_string()))
            .collect(),
    )
}

/// The green **Live · N players** chip (tooltip: edits auto-compile to players).
fn live_chip(clip: &Arc<CustomAnimation>) -> Dom {
    html!("span", {
        .attr("title", "Edits compile to AnimationPlayers automatically \u{2014} no manual bake. Same as material hot-reload.")
        .style("display", "flex").style("align-items", "center").style("gap", "6px")
        .style("height", "26px").style("padding", "0 10px").style("border-radius", "var(--r2)")
        .style("border", "1px solid oklch(0.74 0.13 150 / .35)")
        .style("background", "oklch(0.74 0.13 150 / .12)")
        .child(html!("span", {
            .style("width", "7px").style("height", "7px").style("border-radius", "50%").style("background", "var(--ok)")
        }))
        .child(html!("span", {
            .class("mono").style("font-size", "10.5px").style("color", "var(--ok)").style("white-space", "nowrap")
            .text_signal(clip.tracks.signal_vec_cloned().len().map(|n| {
                format!("Live \u{00b7} {n} player{}", if n == 1 { "" } else { "s" })
            }))
        }))
    })
}

// ── shared field-binding helpers ─────────────────────────────────────────────

/// Two-way-ish binding: dispatch a command derived from a `Mutable<String>` on
/// every change after the seed. Mirrors the `first`/`fire` pattern in
/// `material_mode`.
fn bind_string(value: &Mutable<String>, to_cmd: impl Fn(String) -> EditorCommand + 'static) {
    spawn_local(clone!(value => async move {
        let mut first = true;
        value.signal_cloned().for_each(move |v| {
            let fire = !first;
            first = false;
            let cmd = to_cmd(v);
            async move { if fire { dispatch(cmd); } }
        }).await;
    }));
}

/// Dispatch a command through the one controller (`spawn_local`).
fn dispatch(cmd: EditorCommand) {
    spawn_local(async move {
        let _ = controller().dispatch(cmd).await;
    });
}
