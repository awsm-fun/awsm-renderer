//! Preview viewport — bottom-left.
//!
//! Phase 8: a blank `<canvas>` placeholder. Phase 9 wires the renderer
//! to draw a stub scene (quad / sphere / box) with the loaded
//! material applied.

use dominator::{html, Dom};

use crate::state::EditState;

pub fn render(_state: &EditState) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("height", "100%")
        .child(html!("div", {
            .style("padding", "8px 12px")
            .style("background", "#222")
            .style("color", "#aaa")
            .style("font-size", "12px")
            .text("Preview — Phase 9 wires the renderer")
        }))
        .child(html!("canvas" => web_sys::HtmlCanvasElement, {
            .attr("id", "preview-canvas")
            .style("flex", "1")
            .style("background", "#000")
        }))
    })
}
