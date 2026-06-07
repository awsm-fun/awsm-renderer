//! Animation viewport — the **real WebGPU scene**, reparented into
//! this slot when Animation mode is active, posed by the active clip at the
//! playhead. No mock rig. Overlay chrome: clip-name + playing chip, a
//! Solo-subtree control, and a Frame-selection button.
//!
//! Canvas reparent: the single live canvas can only live in one DOM slot, so on
//! every switch INTO Animation mode we re-append it here (and Scene mode does the
//! same for its slot — see `scene_mode::viewport`). The WebGPU surface is bound
//! to the canvas element, not its parent, so moving it in the DOM is free and the
//! render loop keeps ticking.

use crate::controller::{controller, EditorCommand, EditorMode};
use crate::engine::scene::NodeId;
use crate::prelude::*;

const CHIP_BG: &str = "oklch(0.13 0.006 255 / 0.85)";

pub fn render() -> Dom {
    html!("div", {
        .style("position", "absolute")
        .style("inset", "0")
        .style("overflow", "hidden")
        .style("background", "var(--bg-0)")
        // The live WebGPU canvas, reparented into this slot whenever Animation
        // mode is active.
        .child(html!("div", {
            .style("position", "absolute")
            .style("inset", "0")
            .after_inserted(|elem| {
                let slot: web_sys::Element = elem.into();
                // Grab the canvas now if we're already in Animation mode.
                reparent_if_active(&slot);
                // …and re-grab it on every later switch into Animation mode.
                spawn_local(clone!(slot => async move {
                    controller().mode.signal().for_each(move |m| {
                        if m == EditorMode::Animation {
                            reparent_canvas(&slot);
                        }
                        async {}
                    }).await;
                }));
            })
        }))
        // Overlay chrome.
        .child(transport_overlay())
        .child(clip_chip())
    })
}

fn reparent_if_active(slot: &web_sys::Element) {
    if controller().mode.get() == EditorMode::Animation {
        reparent_canvas(slot);
    }
}

/// Move the single live WebGPU canvas into this slot, surfacing a mount failure
/// (mirrors the Scene viewport) instead of swallowing it — a broken reparent
/// would otherwise leave the viewport blank with no diagnostics.
fn reparent_canvas(slot: &web_sys::Element) {
    crate::engine::context::with_canvas(|c| {
        if let Err(err) = slot.append_child(c) {
            Modal::error(format!("Failed to mount viewport canvas: {err:?}"));
        }
    });
    crate::engine::context::sync_canvas_size();
}

/// Bottom-left: active clip name + a "playing" indicator.
fn clip_chip() -> Dom {
    html!("div", {
        .style("position", "absolute").style("left", "12px").style("bottom", "12px")
        .style("display", "flex").style("align-items", "center").style("gap", "8px")
        .style("pointer-events", "none")
        .child(html!("span", {
            .class("mono")
            .style("font-size", "10.5px").style("font-weight", "500")
            .style("white-space", "nowrap").style("color", "oklch(0.86 0.01 255)")
            .style("padding", "4px 8px").style("background", CHIP_BG)
            .style("backdrop-filter", "blur(8px)").style("border-radius", "var(--r1)")
            .style("border", "1px solid var(--line)")
            .text_signal(active_clip_name())
        }))
        .child_signal(controller().playing.signal().map(|p| if p {
            Some(html!("span", {
                .class("mono")
                .style("font-size", "10.5px").style("color", "var(--accent-bright)")
                .style("padding", "4px 8px").style("background", CHIP_BG)
                .style("backdrop-filter", "blur(8px)").style("border-radius", "var(--r1)")
                .style("border", "1px solid var(--accent-line)")
                .text("\u{25B6} playing")
            }))
        } else { None }))
    })
}

fn active_clip_name() -> impl Signal<Item = String> {
    use crate::controller::animation::find_clip;
    use futures_signals::signal::always;
    // Flatten into the active clip's own `name` signal (not a one-shot
    // `get_cloned`) so the chip stays live while the clip is renamed — without
    // the inner subscription it would only refresh when `current_clip` changes.
    controller().current_clip.signal().switch(|cur| match cur {
        Some(id) => match find_clip(&controller().custom_animations, id) {
            Some(c) => c
                .name
                .signal_cloned()
                .map(|n| format!("{n} \u{00B7} scene preview"))
                .boxed_local(),
            None => always("\u{2014}".to_string()).boxed_local(),
        },
        None => always("no clip".to_string()).boxed_local(),
    })
}

/// Top-right overlay: Solo-subtree toggle + Frame-selection button.
fn transport_overlay() -> Dom {
    html!("div", {
        .style("position", "absolute").style("right", "12px").style("top", "12px")
        .style("display", "flex").style("align-items", "center").style("gap", "6px")
        .child(IconBtn::new("target").title("Frame selection")
            .on_click(|| {
                // Reuse the Scene-mode camera-fit ("Reset View" frames the scene).
                dispatch(EditorCommand::ResetCamera);
            }).render())
        .child(solo_button())
    })
}

/// Solo-subtree: when a node is selected, solo its subtree (others rest-hold);
/// otherwise clear back to whole-scene. Toggles `SetSoloRoot`.
fn solo_button() -> Dom {
    let label = map_ref! {
        let solo = controller().anim_solo_root.signal(),
        let _sel = controller().selected.signal_cloned() => {
            if solo.is_some() { "Solo: subtree".to_string() } else { "Whole scene".to_string() }
        }
    };
    html!("button", {
        .class("t")
        .class("mono")
        .attr("title", "Solo the selected subtree (others hold at rest)")
        .style("display", "flex").style("align-items", "center").style("gap", "5px")
        .style("height", "28px").style("padding", "0 10px")
        .style("border-radius", "var(--r2)").style("cursor", "pointer")
        .style("background", CHIP_BG).style("backdrop-filter", "blur(8px)")
        .style("border", "1px solid var(--line)").style("color", "var(--text-1)")
        .style("font-size", "11px")
        .child(Icon::new("layers").size(13.0).render())
        .child(html!("span", { .text_signal(label) }))
        .event(|_: events::Click| {
            let solo = controller().anim_solo_root.get();
            let next: Option<NodeId> = if solo.is_some() {
                None
            } else {
                controller().selected.lock_ref().last().copied()
            };
            dispatch(EditorCommand::SetSoloRoot { id: next });
        })
    })
}

fn dispatch(cmd: EditorCommand) {
    spawn_local(async move {
        let _ = controller().dispatch(cmd).await;
    });
}
