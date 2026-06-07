//! The Animation-mode workspace layout: the **ribbon** (active-clip header) over
//! a `248px · 1fr` grid — the left column stacks the **ClipLibrary** over the
//! **KeyInspector**, the right column the real-scene **viewport** over the
//! **timeline dock**.

use super::{inspector, library, ribbon, timeline, viewport};
use crate::prelude::*;

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
            // LEFT column: library (flex:1) over inspector (max 48%, scroll).
            .child(html!("div", {
                .style("display", "flex").style("flex-direction", "column").style("min-height", "0")
                .style("background", "var(--bg-1)").style("border-right", "1px solid var(--line)")
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
