//! Animation mode — the third editor workspace (Scene · Material · **Animation**),
//! a clip-authoring studio. See `docs/plans/animation-editor.md`.
//!
//! M-A2 layout: the **ribbon** (active-clip header) over a `248px · 1fr`
//! workspace — the left column stacks the **ClipLibrary** over the **KeyInspector**
//! (key/track editor); the right column is a placeholder for the real viewport +
//! timeline dock that land in M-A3/M-A4.
//!
//! Load-bearing rule (§0.2): every animation mutation is a serializable
//! `EditorCommand` dispatched through the one `EditorController` — the UI never
//! mutates animation state directly.

mod inspector;
mod library;
mod ribbon;
mod viewport;

use crate::prelude::*;

pub fn render() -> Dom {
    html!("div", {
        .style("position", "absolute").style("inset", "0")
        .style("display", "flex").style("flex-direction", "column")
        .style("min-height", "0").style("background", "var(--bg-0)")
        // ── ribbon (active-clip header) ──────────────────────────────────────
        .child(ribbon::render())
        // ── workspace: 248px rail + viewport/timeline placeholder ────────────
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
                // M-A4 fills this; M-A3 leaves a sized placeholder so the viewport
                // canvas gets a real (non-zero) area to size to.
                .child(html!("div", {
                    .style("flex", "0 0 320px").style("min-height", "0")
                    .style("border-top", "1px solid var(--line)").style("background", "var(--bg-1)")
                    .style("display", "flex").style("align-items", "center").style("justify-content", "center")
                    .child(html!("span", {
                        .style("font-size", "12px").style("color", "var(--text-3)")
                        .text("Timeline dock \u{2014} M-A4")
                    }))
                }))
            }))
        }))
    })
}
