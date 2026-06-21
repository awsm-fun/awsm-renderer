//! The Animation-mode workspace layout: the **ribbon** (active-clip header) over
//! a `248px · 1fr` grid — the left column stacks a collapsible **Scene tree**
//! (the shared Scene-mode outliner, so you can see + pick which node/mesh a track
//! binds to — U2), the **ClipLibrary**, and the **KeyInspector**; the right
//! column the real-scene **viewport** over the **timeline dock**.

use super::{inspector, library, ribbon, timeline, viewport};
use crate::prelude::*;
use crate::scene_mode::outliner;

pub fn render() -> Dom {
    html!("div", {
        .style("position", "absolute").style("inset", "0")
        .style("display", "flex").style("flex-direction", "column")
        .style("min-height", "0").style("background", "var(--bg-0)")
        // ── ribbon (active-clip header) ──────────────────────────────────────
        .child(ribbon::render())
        // ── workspace: 248px rail + viewport over timeline dock ──────────────
        .child(html!("div", {
            .style("flex", "1").style("min-height", "0")
            .style("display", "grid").style("grid-template-columns", "248px 1fr")
            // LEFT column: collapsible scene tree, then library (flex:1) over
            // inspector (max 48%, scroll).
            .child(html!("div", {
                .style("display", "flex").style("flex-direction", "column").style("min-height", "0")
                .style("background", "var(--bg-1)").style("border-right", "1px solid var(--line)")
                // Scene tree (U2): the SAME outliner Scene mode uses — selection is
                // the shared `controller().selected`, so picking a node here drives
                // the gizmo + the selection-aware Add-Track flow, and you can see
                // what a track binds to. Collapsible to reclaim vertical space.
                .child(outliner_section())
                .child(html!("div", {
                    .style("flex", "1").style("min-height", "0")
                    .child(library::render())
                }))
                .child(html!("div", {
                    .style("flex", "0 0 auto").style("max-height", "48%").style("overflow", "auto")
                    .style("border-top", "1px solid var(--line)")
                    .child(inspector::render())
                }))
            }))
            // RIGHT column: real-scene viewport (top) over the timeline dock.
            .child(html!("div", {
                .style("min-width", "0").style("min-height", "0")
                .style("display", "flex").style("flex-direction", "column")
                // Viewport (the reparented WebGPU canvas) — flexes to fill.
                .child(html!("div", {
                    .style("position", "relative").style("flex", "1").style("min-height", "0")
                    .child(viewport::render())
                }))
                // Timeline dock (transport · ruler · Dope Sheet / Curves / Mixer).
                .child(html!("div", {
                    .style("flex", "0 0 320px").style("min-height", "0")
                    .style("border-top", "1px solid var(--line)").style("background", "var(--bg-1)")
                    .child(timeline::dock::render())
                }))
            }))
        }))
    })
}

/// Collapsible **Scene tree** section for the Animation-mode left rail: a slim
/// toggle bar over the shared Scene-mode [`outliner`] (bounded + scrollable).
/// Defaults open. The outliner reads/writes `controller().selected`, so selection
/// is shared with Scene mode — the whole point of U2.
fn outliner_section() -> Dom {
    let open = Mutable::new(true);
    html!("div", {
        .style("flex", "0 0 auto").style("display", "flex").style("flex-direction", "column")
        .style("min-height", "0").style("border-bottom", "1px solid var(--line)")
        // Toggle bar.
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "6px")
            .style("padding", "6px 10px").style("cursor", "pointer").style("user-select", "none")
            .style("font-size", "11px").style("font-weight", "600")
            .style("color", "var(--text-1)").style("text-transform", "uppercase")
            .style("letter-spacing", "0.04em")
            .child(html!("span", {
                .text_signal(open.signal().map(|o| if o { "\u{25be}" } else { "\u{25b8}" }))
            }))
            .child(html!("span", { .text("Scene Tree") }))
            .event(clone!(open => move |_: events::Click| open.set(!open.get())))
        }))
        // Body: the reused outliner, bounded so the library/inspector still fit.
        .child_signal(open.signal().map(|o| {
            o.then(|| html!("div", {
                .style("flex", "0 0 auto").style("min-height", "0")
                .style("max-height", "240px").style("overflow", "auto")
                .child(outliner::render())
            }))
        }))
    })
}
