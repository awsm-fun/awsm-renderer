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

use dominator::{clone, events, html, with_node, Dom};

use crate::{
    panes,
    state::{EditState, Starter},
};

/// Starter templates exposed by the File → New menu. Order matches the
/// dropdown order. `Scanline` is the default `EditState::new_scanline`
/// seed; the other two are clean-slate options.
const FILE_NEW_STARTERS: &[Starter] = &[
    Starter::ConstantRed,
    Starter::UnlitBaseline,
    Starter::Scanline,
];

/// Construct the root DOM element for the material-editor with a
/// pre-built [`EditState`]. The caller (`main.rs`) keeps a clone
/// of the state so the recompile loop can listen for edits.
pub fn root_with_state(state: EditState) -> Dom {
    html!("div", {
        .style("position", "relative")
        .style("height", "100vh")
        .style("font-family", "sans-serif")
        .child(html!("div", {
            .style("display", "grid")
            .style("grid-template-rows", "40px 1fr 240px")
            .style("grid-template-columns", "320px 1fr 320px")
            .style("height", "100vh")
            .children(&mut [
                // Top bar — spans all three columns.
                html!("div", {
                    .style("grid-column", "1 / span 3")
                    .style("padding", "8px")
                    .style("background", "#222")
                    .style("color", "#eee")
                    .style("display", "flex")
                    .style("align-items", "center")
                    .style("gap", "12px")
                    .child(html!("span", {
                        .style("flex", "0 0 auto")
                        .text("awsm material editor")
                    }))
                    // File → New starter dropdown. Selecting a value
                    // wipes the live state and seeds it with the
                    // chosen template, then resets the <select> back
                    // to its placeholder so the next pick fires the
                    // change event again. The debounced recompile
                    // loop picks the new wgsl + definition up on its
                    // next tick.
                    .child(html!("label", {
                        .style("color", "#aaa")
                        .style("font-size", "12px")
                        .text("File:")
                    }))
                    .child(html!("select" => web_sys::HtmlSelectElement, {
                        .style("background", "#111")
                        .style("color", "#eee")
                        .style("border", "1px solid #444")
                        .style("padding", "2px 6px")
                        .style("font-size", "12px")
                        .child(html!("option", {
                            .attr("value", "")
                            .text("New…")
                        }))
                        .children(FILE_NEW_STARTERS.iter().enumerate().map(|(i, s)| {
                            html!("option", {
                                .attr("value", &i.to_string())
                                .text(s.label())
                            })
                        }).collect::<Vec<_>>())
                        .with_node!(elem => {
                            .event(clone!(state => move |_: events::Change| {
                                let idx = elem.value();
                                if idx.is_empty() {
                                    return;
                                }
                                if let Ok(i) = idx.parse::<usize>() {
                                    if let Some(starter) = FILE_NEW_STARTERS.get(i) {
                                        state.reset_to(*starter);
                                    }
                                }
                                // Reset back to the placeholder so the
                                // same selection can re-fire on a later
                                // pick. setting "" matches the placeholder
                                // option's value above.
                                elem.set_value("");
                            }))
                        })
                    }))
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
        }))
        // Block A.4: floating compile-status overlay. Auto-shows
        // whenever the renderer's pipeline scheduler has any group
        // `Pending`; auto-dismisses when all transitions resolve.
        .child(panes::compile_modal::render(&state))
        // Buffer Converter modal — shows when EditState's
        // converter_open_for_slot is Some. Mounted at root so its
        // fixed-positioned backdrop covers the full window.
        .child(panes::buffer_converter::render(&state))
    })
}
