//! Reusable focus-aware history coalescing for asset-inspector inputs.
//!
//! Without this every keystroke in a text input or every pixel of a
//! color-picker drag commits its own history entry — typing "Hello"
//! produces 5 entries, dragging a color across the spectrum produces
//! hundreds. `number_input` solves this for scalar inputs by taking
//! a snapshot on `FocusIn`, mutating freely on `Input`, and committing
//! one entry on `FocusOut`. This module gives the same shape to the
//! other input types asset inspectors use.
//!
//! `text_input` and `color_input` accept a `write` closure that just
//! mutates state — they coalesce the history commits themselves.
//! Single-event widgets (checkbox, select) don't need this helper:
//! one event = one entry is correct for them.

use crate::prelude::*;
use crate::scene::SceneSnapshot;
use crate::state::app_state;
use futures_signals::signal::SignalExt;
use std::sync::{Arc, Mutex};

/// Per-input focus-tracking state. Captures one scene snapshot on
/// the first `FocusIn` of a gesture, marks the input dirty on each
/// write, then commits exactly one history entry on `FocusOut`.
#[allow(clippy::arc_with_non_send_sync)]
fn new_focus_state() -> (Arc<Mutex<Option<SceneSnapshot>>>, Arc<Mutex<bool>>) {
    (Arc::new(Mutex::new(None)), Arc::new(Mutex::new(false)))
}

fn handle_focus_in(pre_edit: &Arc<Mutex<Option<SceneSnapshot>>>, dirty: &Arc<Mutex<bool>>) {
    *pre_edit.lock().unwrap() = Some(app_state().snapshot_scene());
    *dirty.lock().unwrap() = false;
}

fn handle_focus_out(pre_edit: &Arc<Mutex<Option<SceneSnapshot>>>, dirty: &Arc<Mutex<bool>>) {
    let was_dirty = std::mem::replace(&mut *dirty.lock().unwrap(), false);
    let snap = pre_edit.lock().unwrap().take();
    if was_dirty {
        if let Some(snap) = snap {
            app_state().commit_history(snap);
        }
    }
}

/// `<input type="text">` whose history is coalesced across a single
/// focus gesture. `read` returns the current value (called on every
/// revision tick to keep the DOM in sync); `write` applies a new
/// value (called on each keystroke, without committing history).
/// Snapshot lifecycle is handled here.
pub fn text_input(
    read: impl Fn() -> String + Clone + 'static,
    write: impl Fn(String) + 'static,
    revision: impl futures_signals::signal::Signal<Item = u64> + 'static,
) -> Dom {
    let read_for_signal = read.clone();
    let (pre_edit, dirty) = new_focus_state();
    html!("input" => web_sys::HtmlInputElement, {
        .style("width", "100%")
        .style("padding", "0.3rem 0.45rem")
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.25rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("font-size", "0.85rem")
        .attr("type", "text")
        .with_node!(input => {
            .future(clone!(input => {
                revision.for_each(move |_| {
                    let v = read_for_signal();
                    if input.value() != v {
                        input.set_value(&v);
                    }
                    async {}
                })
            }))
            .event(clone!(pre_edit, dirty => move |_: events::FocusIn| {
                handle_focus_in(&pre_edit, &dirty);
            }))
            .event(clone!(input, dirty => move |_: events::Input| {
                write(input.value());
                *dirty.lock().unwrap() = true;
            }))
            .event(clone!(pre_edit, dirty => move |_: events::FocusOut| {
                handle_focus_out(&pre_edit, &dirty);
            }))
        })
    })
}

/// `<input type="color">` whose history is coalesced across a single
/// focus gesture. The native color picker opens on click and emits
/// per-pixel `Input` events while dragging; closing the picker fires
/// `FocusOut`. Same shape as [`text_input`].
///
/// `read_hex` returns the hex string (e.g. `"#ff8800"`) so the input
/// can sync without the caller having to handle rgba conversion.
/// `write_hex` is called with the new hex on each Input event.
pub fn color_input(
    read_hex: impl Fn() -> String + Clone + 'static,
    write_hex: impl Fn(String) + 'static,
    revision: impl futures_signals::signal::Signal<Item = u64> + 'static,
) -> Dom {
    let read_for_signal = read_hex.clone();
    let (pre_edit, dirty) = new_focus_state();
    html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "color")
        .style("cursor", "pointer")
        .style("width", "3rem")
        .with_node!(input => {
            .future(clone!(input => {
                revision.for_each(move |_| {
                    let v = read_for_signal();
                    if input.value() != v {
                        input.set_value(&v);
                    }
                    async {}
                })
            }))
            .event(clone!(pre_edit, dirty => move |_: events::FocusIn| {
                handle_focus_in(&pre_edit, &dirty);
            }))
            .event(clone!(input, dirty => move |_: events::Input| {
                write_hex(input.value());
                *dirty.lock().unwrap() = true;
            }))
            .event(clone!(pre_edit, dirty => move |_: events::FocusOut| {
                handle_focus_out(&pre_edit, &dirty);
            }))
        })
    })
}
