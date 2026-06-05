//! Contract pane — right side.
//!
//! Phase 11 renders the contract markdown inline. Pre-baked at build
//! time via `include_str!` so there's no runtime fetch + no markdown
//! parser dep (the contract docs are plain text with code blocks; a
//! `<pre>` element preserves the formatting acceptably for an
//! authoring-tool sidebar).

use dominator::{html, Dom};
use futures_signals::signal::SignalExt;

use awsm_scene_schema::material::MaterialAlphaMode;

use crate::material::state::EditState;

const CONTRACT_OPAQUE_MD: &str =
    include_str!("../../../../../../docs/dynamic-materials/contract-opaque.md");
const CONTRACT_TRANSPARENT_MD: &str =
    include_str!("../../../../../../docs/dynamic-materials/contract-transparent.md");

pub fn render(state: &EditState) -> Dom {
    let definition = state.definition.clone();
    html!("div", {
        .style("padding", "12px")
        .style("border-left", "1px solid var(--line)")
        .style("background", "var(--bg-1)")
        .style("color", "var(--text-1)")
        .style("overflow", "auto")
        .style("font-size", "11px")
        .child(html!("h3", { .text("Contract") }))
        .child_signal(definition.signal_cloned().map(|def| {
            let text = match def.alpha_mode {
                MaterialAlphaMode::Blend => CONTRACT_TRANSPARENT_MD,
                _ => CONTRACT_OPAQUE_MD,
            };
            Some(html!("pre", {
                .style("white-space", "pre-wrap")
                .style("font-family", "monospace")
                .style("font-size", "10px")
                .style("line-height", "1.3")
                .text(text)
            }))
        }))
    })
}
