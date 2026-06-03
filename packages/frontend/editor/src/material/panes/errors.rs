//! Errors pane — bottom-right.
//!
//! Phase 8: empty placeholder. Phase 9 populates from
//! `register_material` failures. Phase 11 parses line/column from
//! naga's diagnostics and surfaces clickable list entries that
//! position the WGSL textarea cursor via [`super::wgsl_editor::focus_at`].

use dominator::{clone, events, html, Dom};
use futures_signals::signal::SignalExt;

use crate::material::state::EditState;

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
                        // Only entries with a parsed line are
                        // clickable — the others have nothing to
                        // navigate to.
                        let line = e.line;
                        let column = e.column;
                        let clickable = line.is_some();
                        html!("li", {
                            .style("margin-bottom", "8px")
                            .style_signal("cursor", futures_signals::signal::always(
                                if clickable { "pointer" } else { "default" }
                            ))
                            .apply_if(clickable, |b| {
                                b.style("text-decoration", "underline")
                                 .style("text-decoration-color", "#633")
                                 .style("text-underline-offset", "3px")
                            })
                            .text(&format!(
                                "{}{}{}",
                                line.map(|l| format!("L{l}: ")).unwrap_or_default(),
                                column.map(|c| format!("C{c}: ")).unwrap_or_default(),
                                e.message,
                            ))
                            .apply_if(clickable, clone!(line => move |b| {
                                b.event(move |_: events::Click| {
                                    if let Some(l) = line {
                                        super::wgsl_editor::focus_at(l, column);
                                    }
                                })
                            }))
                        })
                    }).collect::<Vec<_>>())
                }))
            }
        }))
    })
}
