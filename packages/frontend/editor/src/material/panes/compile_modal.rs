//! Pipeline-compile status modal — Block A.4.
//!
//! Floating overlay above the editor chrome that displays
//! "Compiling N pipeline(s)…" while the renderer's pipeline scheduler
//! has any group in `Pending` state. Driven by
//! `state.compile_pending` (a `Mutable<usize>` updated each RAF tick
//! from `AwsmRenderer::drain_pipeline_status_events`).
//!
//! Failed transitions surface their error string in a "Last error"
//! subsection at the bottom of the modal (separate from the per-
//! material WGSL compile errors shown in the Errors pane — those come
//! from `register_material` results; this carries scheduler-level
//! Failed events for any pipeline group).

use std::sync::Arc;

use dominator::{html, Dom};
use futures_signals::signal::{Mutable, SignalExt};

use crate::material::state::EditState;

/// Render the compile-status modal. Returns a div positioned `fixed`
/// over the editor — the inner card swaps in/out via `child_signal`
/// keyed on the pending counter so the editor stays interactive when
/// the scheduler is idle.
pub fn render(state: &EditState) -> Dom {
    let pending = state.compile_pending.clone();
    let last_error = state.compile_last_error.clone();

    html!("div", {
        .style("position", "fixed")
        .style("inset", "0")
        .style("display", "flex")
        .style("align-items", "center")
        .style("justify-content", "center")
        .style("z-index", "10000")
        .style_signal("pointer-events", pending.signal().map(|n| if n > 0 { "auto" } else { "none" }))
        .style_signal("background", pending.signal().map(|n| if n > 0 { "rgba(0, 0, 0, 0.45)" } else { "transparent" }))
        .child_signal(pending.signal().map({
            let pending = pending.clone();
            let last_error = last_error.clone();
            move |n| {
                if n > 0 {
                    Some(card(pending.clone(), last_error.clone()))
                } else {
                    None
                }
            }
        }))
    })
}

fn card(pending: Arc<Mutable<usize>>, last_error: Arc<Mutable<Option<String>>>) -> Dom {
    html!("div", {
        .style("min-width", "320px")
        .style("max-width", "520px")
        .style("padding", "20px 28px")
        .style("background", "var(--bg-2)")
        .style("color", "var(--text-0)")
        .style("border", "1px solid var(--line)")
        .style("border-radius", "6px")
        .style("box-shadow", "0 8px 32px rgba(0, 0, 0, 0.6)")
        .style("font-family", "sans-serif")
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "10px")
        .child(html!("div", {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "12px")
            .child(html!("div", {
                // Animated CSS spinner — `@keyframes awsm-spin` is
                // injected at first card mount via `after_inserted`.
                .style("width", "18px")
                .style("height", "18px")
                .style("border", "2px solid #555")
                .style("border-top-color", "var(--accent-bright)")
                .style("border-radius", "50%")
                .style("animation", "awsm-spin 0.9s linear infinite")
            }))
            .child(html!("h3", {
                .style("margin", "0")
                .style("font-size", "1.05rem")
                .style("font-weight", "600")
                .text_signal(pending.signal().map(|n| {
                    format!("Compiling {n} pipeline{}…", if n == 1 { "" } else { "s" })
                }))
            }))
        }))
        .child(html!("p", {
            .style("margin", "0")
            .style("font-size", "0.85rem")
            .style("color", "var(--text-2)")
            .style("line-height", "1.4")
            .text("The renderer is building GPU pipelines for the current material configuration. This usually finishes in a frame or two; first-load compiles may take longer.")
        }))
        .child_signal(last_error.signal_cloned().map(|err| {
            err.map(|msg| {
                html!("div", {
                    .style("margin-top", "4px")
                    .style("padding", "8px 10px")
                    .style("background", "var(--danger-soft)")
                    .style("border", "1px solid color-mix(in oklch, var(--danger) 50%, transparent)")
                    .style("border-radius", "4px")
                    .style("color", "var(--danger-bright)")
                    .style("font-size", "0.8rem")
                    .style("font-family", "monospace")
                    .style("white-space", "pre-wrap")
                    .style("word-break", "break-word")
                    .style("max-height", "140px")
                    .style("overflow", "auto")
                    .child(html!("div", {
                        .style("font-weight", "600")
                        .style("margin-bottom", "4px")
                        .style("color", "var(--danger-bright)")
                        .text("Last error")
                    }))
                    .child(html!("div", {
                        .text(&msg)
                    }))
                })
            })
        }))
        .after_inserted(|_elem| {
            inject_spin_keyframes();
        })
    })
}

fn inject_spin_keyframes() {
    use std::sync::OnceLock;
    static INJECTED: OnceLock<()> = OnceLock::new();
    if INJECTED.get().is_some() {
        return;
    }
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let Some(head) = document.head() else { return };
    let Ok(style) = document.create_element("style") else {
        return;
    };
    style.set_text_content(Some(
        "@keyframes awsm-spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }",
    ));
    let _ = head.append_child(&style);
    let _ = INJECTED.set(());
}
