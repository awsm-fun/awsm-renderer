//! WGSL editor pane — center.
//!
//! Phase 8: read-only `<textarea>` showing the loaded material's WGSL.
//! Phase 9 wires Ctrl-S / blur to a debounced recompile. Phase 10
//! adds the auto-generated `struct MaterialData { ... }` preview
//! above the user's body.

use dominator::{html, with_node, Dom};
use wasm_bindgen::JsCast;

use crate::state::EditState;

pub fn render(state: &EditState) -> Dom {
    let wgsl = state.wgsl_source.clone();
    let wgsl_for_input = wgsl.clone();
    html!("div", {
        .style("padding", "12px")
        .style("border-right", "1px solid #333")
        .style("background", "#111")
        .style("color", "#ddd")
        .style("display", "flex")
        .style("flex-direction", "column")
        .child(html!("h3", { .text("shader.wgsl") }))
        .child(html!("textarea" => web_sys::HtmlTextAreaElement, {
            .style("flex", "1")
            .style("font-family", "monospace")
            .style("font-size", "12px")
            .style("background", "#0b0b0b")
            .style("color", "#cce")
            .style("border", "1px solid #333")
            .style("padding", "8px")
            .style("resize", "none")
            .prop_signal("value", wgsl.signal_cloned())
            .with_node!(_elem => {
                .event(move |e: dominator::events::Input| {
                    if let Some(target) = e.target() {
                        if let Ok(ta) = target.dyn_into::<web_sys::HtmlTextAreaElement>() {
                            wgsl_for_input.set(ta.value());
                        }
                    }
                })
            })
        }))
    })
}
