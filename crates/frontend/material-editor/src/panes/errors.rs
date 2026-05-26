//! Errors pane — bottom-right.
//!
//! Phase 8: empty placeholder. Phase 9 populates from
//! `register_material` failures. Phase 11 parses line/column from
//! naga's diagnostics and surfaces clickable list entries that
//! position the WGSL textarea cursor.

use dominator::{html, Dom};
use futures_signals::signal::SignalExt;

use crate::state::EditState;

pub fn render(state: &EditState) -> Dom {
    let errors = state.errors.clone();
    html!("div", {
        .style("padding", "12px")
        .style("border-top", "1px solid #333")
        .style("border-left", "1px solid #333")
        .style("background", "#1a1010")
        .style("color", "#fcc")
        .style("overflow", "auto")
        .child(html!("h3", {
            .style("color", "#fcc")
            .text("Errors")
        }))
        .child_signal(errors.signal_cloned().map(|errs| {
            if errs.is_empty() {
                Some(html!("p", { .text("no compile errors") }))
            } else {
                Some(html!("ul", {
                    .style("padding-left", "16px")
                    .style("font-size", "12px")
                    .children(errs.into_iter().map(|e| {
                        html!("li", {
                            .style("margin-bottom", "8px")
                            .text(&format!(
                                "{}{}{}",
                                e.line.map(|l| format!("L{l}: ")).unwrap_or_default(),
                                e.column.map(|c| format!("C{c}: ")).unwrap_or_default(),
                                e.message,
                            ))
                        })
                    }).collect::<Vec<_>>())
                }))
            }
        }))
    })
}

