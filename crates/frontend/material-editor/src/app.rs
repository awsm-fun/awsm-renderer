//! Top-level dominator component for the material editor.
//!
//! Lays out a four-pane grid:
//!   ┌───────────────────────────────────────────┐
//!   │ Header (File / Preview mesh / Recompile)  │
//!   ├──────────┬──────────────────┬─────────────┤
//!   │ Defn     │ WGSL editor      │ Contract    │
//!   │ pane     │ (Phase 10/8 stub)│ (Phase 11)  │
//!   ├──────────┴──────────────────┴─────────────┤
//!   │ Preview viewport          │ Errors pane   │
//!   │ (Phase 9)                 │ (Phase 11)    │
//!   └───────────────────────────────────────────┘
//!
//! Phase 8 ships the skeleton + hard-coded scanline material in the
//! definition + wgsl panes (read-only). Subsequent phases fill in the
//! interactivity.

use dominator::{html, Dom};

use crate::{panes, state::EditState};

/// Construct the root DOM element for the material-editor with a
/// pre-built [`EditState`]. The caller (`main.rs`) keeps a clone
/// of the state so the recompile loop can listen for edits.
pub fn root_with_state(state: EditState) -> Dom {
    html!("div", {
        .style("display", "grid")
        .style("grid-template-rows", "40px 1fr 240px")
        .style("grid-template-columns", "320px 1fr 320px")
        .style("height", "100vh")
        .style("font-family", "sans-serif")
        .children(&mut [
            // Top bar — spans all three columns.
            html!("div", {
                .style("grid-column", "1 / span 3")
                .style("padding", "8px")
                .style("background", "#222")
                .style("color", "#eee")
                .text("awsm material editor — Phase 8 scaffold")
            }),
            panes::definition::render(&state),
            panes::wgsl_editor::render(&state),
            panes::contract::render(&state),
            // Bottom: preview (left half of last row) + errors (right).
            html!("div", {
                .style("grid-column", "1 / span 2")
                .style("border-top", "1px solid #333")
                .child(panes::preview::render(&state))
            }),
            panes::errors::render(&state),
        ])
    })
}
