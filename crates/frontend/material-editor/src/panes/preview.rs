//! Preview viewport — bottom-left.
//!
//! Phase 8: a blank `<canvas>` placeholder. Phase 9 wires the renderer
//! to draw a stub quad with the loaded material applied.
//!
//! The canvas dimensions are pinned at 800×600 so the renderer's
//! swap-chain texture is built at a sensible resolution at boot time.
//! Without explicit `width` / `height` attributes the browser
//! defaults a `<canvas>` to 300×150, which is too small for the
//! visibility-buffer tile compute kernel to show useful detail and
//! also makes the CSS-scaled preview look pixelated.

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
            .text("Preview — live scanline material on a 2×2 plane")
        }))
        .child(html!("canvas" => web_sys::HtmlCanvasElement, {
            .attr("id", "preview-canvas")
            .attr("width", "800")
            .attr("height", "600")
            .style("width", "800px")
            .style("height", "600px")
            .style("display", "block")
            .style("margin", "8px auto")
            .style("background", "#000")
        }))
    })
}
