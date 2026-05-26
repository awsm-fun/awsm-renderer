//! Definition pane — left side.
//!
//! Phase 8 ships a read-only summary of the loaded material's
//! definition. Phase 10 turns it into a full table editor with
//! add/delete/reorder rows.

use dominator::{html, Dom};
use futures_signals::signal::SignalExt;

use crate::state::EditState;

pub fn render(state: &EditState) -> Dom {
    let definition = state.definition.clone();
    html!("div", {
        .style("padding", "12px")
        .style("border-right", "1px solid #333")
        .style("overflow", "auto")
        .style("background", "#1a1a1a")
        .style("color", "#ddd")
        .child(html!("h3", { .text("Definition") }))
        .child_signal(definition.signal_cloned().map(|def| {
            Some(html!("pre", {
                .style("font-size", "11px")
                .style("white-space", "pre-wrap")
                .text(&format!(
                    "name: {}\nversion: {}\nalpha_mode: {:?}\ndouble_sided: {}\nuniforms: {} field(s)\ntextures: {} slot(s)\nbuffers:  {} slot(s)",
                    def.name, def.version, def.alpha_mode, def.double_sided,
                    def.uniforms.len(), def.textures.len(), def.buffers.len(),
                ))
            }))
        }))
    })
}
