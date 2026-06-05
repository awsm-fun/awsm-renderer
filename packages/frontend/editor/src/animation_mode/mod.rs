//! Animation mode — the third editor workspace (Scene · Material · **Animation**),
//! a clip-authoring studio. See `docs/plans/animation-editor.md`.
//!
//! M-A0 scaffold: an empty shell that mounts under the mode router and proves the
//! Scene/Material/Animation switch + WebGPU-canvas reparent works. The real panels
//! (ribbon · clip library · key/track inspector · real-scene viewport · timeline
//! dock with Dope Sheet / Curves / Mixer) land in M-A2…M-A6.
//!
//! Load-bearing rule (§0.2): every animation mutation is a serializable
//! `EditorCommand` dispatched through the one `EditorController` — the UI never
//! mutates animation state directly.

use crate::prelude::*;

pub fn render() -> Dom {
    html!("div", {
        .style("position", "absolute").style("inset", "0")
        .style("display", "flex").style("flex-direction", "column")
        .style("align-items", "center").style("justify-content", "center")
        .style("gap", "10px")
        .style("min-height", "0").style("background", "var(--bg-0)")
        .child(html!("div", {
            .style("font-size", "13px").style("color", "var(--text-2)")
            .text("Animation workspace")
        }))
        .child(html!("div", {
            .style("font-size", "12px").style("color", "var(--text-3)")
            .text("Clip authoring lands here (M-A2…M-A6).")
        }))
    })
}
