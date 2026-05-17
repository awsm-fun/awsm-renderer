//! Locked "Loading…" modal with a live phase message.
//!
//! Wraps `Modal::open` + `Modal::lock` and adds a signal-driven body
//! line so async actions (Load, Save, Insert Model) can publish phase
//! updates — "Reading project.json…", "Loading texture files…" — as
//! they progress. The modal stays locked until the caller invokes
//! `close()`, so the user can't accidentally dismiss it mid-flight.

use std::cell::RefCell;

use crate::prelude::*;

thread_local! {
    static MESSAGE: Mutable<String> = Mutable::new(String::new());
    static TITLE: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Open the loading modal. `title` is the bold heading (rarely
/// changes); `initial_message` is the first phase line and can be
/// updated later via `set`.
pub fn open(title: impl Into<String>, initial_message: impl Into<String>) {
    TITLE.with(|t| *t.borrow_mut() = title.into());
    MESSAGE.with(|m| m.set(initial_message.into()));
    Modal::open(render);
    Modal::lock();
}

/// Update the phase line on the currently open loading modal. No-op
/// if the modal isn't open — the message Mutable is global so the
/// next `open` call resets it anyway.
pub fn set(message: impl Into<String>) {
    MESSAGE.with(|m| m.set(message.into()));
}

pub fn close() {
    Modal::close();
}

fn render() -> Dom {
    let title = TITLE.with(|t| t.borrow().clone());
    let message_signal = MESSAGE.with(|m| m.clone());
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("align-items", "center")
        .style("gap", "1rem")
        .style("padding", "0.5rem 1.5rem")
        .style("color", ColorText::SidebarHeader.value())
        .style("min-width", "320px")
        .child(html!("h2", {
            .style("margin", "0")
            .style("font-size", "1.1rem")
            .text(&title)
        }))
        .child(html!("p", {
            .style("margin", "0")
            .style("font-size", "0.95rem")
            .style("line-height", "1.4")
            .text_signal(message_signal.signal_cloned())
        }))
    })
}
