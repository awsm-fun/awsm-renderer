//! Contract pane — right side.
//!
//! Phase 8: hard-coded link / blurb pointing to the contract docs.
//! Phase 11 renders the markdown inline (pre-baked HTML via
//! include_str!).

use dominator::{html, Dom};
use futures_signals::signal::SignalExt;

use crate::state::EditState;

pub fn render(state: &EditState) -> Dom {
    let definition = state.definition.clone();
    html!("div", {
        .style("padding", "12px")
        .style("border-left", "1px solid #333")
        .style("background", "#1a1a1a")
        .style("color", "#ddd")
        .style("overflow", "auto")
        .child(html!("h3", { .text("Contract") }))
        .child_signal(definition.signal_cloned().map(|def| {
            use awsm_scene_schema::material::MaterialAlphaMode;
            let which = match def.alpha_mode {
                MaterialAlphaMode::Blend => "transparent",
                _ => "opaque",
            };
            Some(html!("p", {
                .style("font-size", "12px")
                .text(&format!(
                    "Current alpha_mode → {} contract. \
                     See docs/dynamic-materials/contract-{}.md for the \
                     full surface (Phase 11 renders inline).",
                    which, which,
                ))
            }))
        }))
    })
}
