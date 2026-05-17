//! Locked "Loading…" modal with a live phase message.
//!
//! Wraps `Modal::open` + `Modal::lock` and adds a signal-driven body
//! line so async actions (Load, Save, Insert Model) can publish phase
//! updates — "Reading project.json…", "Loading texture files…" — as
//! they progress. The modal stays locked until the caller invokes
//! `close()`, so the user can't accidentally dismiss it mid-flight.

use std::cell::RefCell;
use std::sync::Arc;

use crate::prelude::*;
use crate::scene::{types::AssetStatus, Node, NodeKind};

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

/// Walk `roots`, find every Model node in the subtree, and await
/// each one's `asset_status` reaching `Ready` or `Failed`. This is
/// what lets callers keep the loading modal up until the renderer
/// bridge has actually allocated the GPU instances — the bridge
/// reacts to `bump_revision` on a microtask + then schedules
/// `instantiate_model_template`, so the synchronous tree-mutation
/// is well ahead of the visible draw.
pub async fn wait_for_models_ready(roots: &[Arc<Node>]) {
    use futures::StreamExt;
    use futures_signals::signal::SignalExt;

    let mut models: Vec<Arc<Node>> = Vec::new();
    fn walk(node: &Arc<Node>, out: &mut Vec<Arc<Node>>) {
        if matches!(&*node.kind.lock_ref(), NodeKind::Model(_)) {
            out.push(node.clone());
        }
        for child in node.children.lock_ref().iter() {
            walk(child, out);
        }
    }
    for root in roots {
        walk(root, &mut models);
    }

    for node in models {
        let mut stream = node.asset_status.signal_cloned().to_stream();
        while let Some(status) = stream.next().await {
            if matches!(status, AssetStatus::Ready | AssetStatus::Failed(_)) {
                break;
            }
        }
    }
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
