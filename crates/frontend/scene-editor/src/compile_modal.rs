//! Pipeline-compile status modal — Block A.4.
//!
//! Floating overlay that displays "Compiling N pipeline(s)…" while
//! the renderer's pipeline scheduler has any group in `Pending`. The
//! state is owned by `AppContext::compile_pending` / `…_last_error`;
//! `renderer_bridge::render_one_frame` updates it each tick by
//! draining `AwsmRenderer::drain_pipeline_status_events`.
//!
//! Distinct from `loading_modal` (which is a locking, action-scoped
//! splash for project-load / insert-model flows): this one is purely
//! non-blocking ambient feedback for the background pipeline-compile
//! scheduler — it auto-shows on demand, doesn't take focus, and the
//! user can still pan / select / edit while pipelines compile in the
//! background.

use crate::context::{compile_last_error_handle, compile_pending_handle};
use crate::prelude::*;
use futures_signals::signal::SignalExt;

/// Render the floating compile-status overlay. Mounted once into the
/// root layout — the inner card swaps in/out via `child_signal` keyed
/// on the pending counter so the editor stays interactive when the
/// scheduler is idle.
pub fn render() -> Dom {
    let pending = compile_pending_handle();
    let last_error = compile_last_error_handle();

    html!("div", {
        // Pinned to the top-right corner so the overlay doesn't fight
        // the gizmo / sidebar / properties pane for screen real
        // estate. Stays out of the way of authoring but is
        // immediately visible.
        .style("position", "fixed")
        .style("top", "60px")
        .style("right", "24px")
        .style("z-index", "9000")
        .style("pointer-events", "none")
        .child_signal(pending.signal().map(clone!(pending, last_error => move |n| {
            if n > 0 {
                Some(card(pending.clone(), last_error.clone()))
            } else {
                None
            }
        })))
    })
}

fn card(pending: Arc<Mutable<usize>>, last_error: Arc<Mutable<Option<String>>>) -> Dom {
    html!("div", {
        .style("min-width", "260px")
        .style("max-width", "420px")
        .style("padding", "12px 16px")
        .style("background", "rgba(28, 28, 32, 0.94)")
        .style("color", ColorText::SidebarHeader.value())
        .style("border", "1px solid #3a3a40")
        .style("border-radius", "6px")
        .style("box-shadow", "0 6px 24px rgba(0, 0, 0, 0.5)")
        .style("font-family", "sans-serif")
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "8px")
        .child(html!("div", {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "10px")
            .child(html!("div", {
                // Animated CSS spinner — `@keyframes awsm-spin` is
                // injected at first card mount via `after_inserted`.
                .style("width", "16px")
                .style("height", "16px")
                .style("border", "2px solid #555")
                .style("border-top-color", "#7fb3ff")
                .style("border-radius", "50%")
                .style("animation", "awsm-spin 0.9s linear infinite")
            }))
            .child(html!("div", {
                .style("font-size", "0.95rem")
                .style("font-weight", "600")
                .text_signal(pending.signal().map(|n| {
                    format!("Compiling {n} pipeline{}…", if n == 1 { "" } else { "s" })
                }))
            }))
        }))
        .child_signal(last_error.signal_cloned().map(|err| {
            err.map(|msg| {
                html!("div", {
                    .style("padding", "6px 8px")
                    .style("background", "#2a1818")
                    .style("border", "1px solid #5a2a2a")
                    .style("border-radius", "4px")
                    .style("color", ColorText::ErrorMuted.value())
                    .style("font-size", "0.75rem")
                    .style("font-family", "monospace")
                    .style("white-space", "pre-wrap")
                    .style("word-break", "break-word")
                    .style("max-height", "120px")
                    .style("overflow", "auto")
                    .child(html!("div", {
                        .style("font-weight", "600")
                        .style("margin-bottom", "2px")
                        .style("color", ColorText::Error.value())
                        .text("Last error")
                    }))
                    .child(html!("div", {
                        .text(&msg)
                    }))
                })
            })
        }))
        .after_inserted(|_elem| inject_spin_keyframes())
    })
}

fn inject_spin_keyframes() {
    use std::sync::OnceLock;
    static INJECTED: OnceLock<()> = OnceLock::new();
    if INJECTED.get().is_some() {
        return;
    }
    let Some(window) = web_sys::window() else { return };
    let Some(document) = window.document() else { return };
    let Some(head) = document.head() else { return };
    let Ok(style) = document.create_element("style") else { return };
    style.set_text_content(Some(
        "@keyframes awsm-spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }",
    ));
    let _ = head.append_child(&style);
    let _ = INJECTED.set(());
}
