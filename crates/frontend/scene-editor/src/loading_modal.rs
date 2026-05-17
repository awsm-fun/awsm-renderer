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

/// Hard ceiling on how long [`wait_for_models_ready`] will block.
/// The modal is locked while it runs, so anything stalled past this
/// surfaces as an explicit "took longer than expected" error rather
/// than leaving the user staring at a frozen "Materializing on
/// GPU…". 15s is well past a healthy glb-on-localhost load — pre-
/// flight checks (`collect_missing_assets`) already short-circuit
/// the "asset file genuinely missing" case before we reach this
/// wait, so hitting the timeout means the bridge is actually stuck
/// and the user should see that loud + clear.
const MODELS_READY_TIMEOUT_MS: u32 = 15_000;

/// Result of [`wait_for_models_ready`]. `failures` carries any
/// `(label, error)` pairs whose nodes resolved to
/// `AssetStatus::Failed`; `timed_out` is true when the wall-clock
/// deadline fired before every model settled. Both empty + false on
/// the happy path.
#[derive(Default)]
pub struct ModelsReady {
    pub failures: Vec<(String, String)>,
    pub timed_out: bool,
}

impl ModelsReady {
    pub fn is_clean(&self) -> bool {
        self.failures.is_empty() && !self.timed_out
    }

    /// Build a user-facing error message for the caller's
    /// `Modal::error` surface. `action_label` is the verb prefix
    /// (e.g. "Insert Model", "Load Project") so the user sees the
    /// failed operation in context.
    pub fn error_message(&self, action_label: &str) -> String {
        let mut buf = format!("{action_label} finished with errors.\n");
        if self.timed_out {
            buf.push_str(&format!(
                "\nTimed out after {}s waiting for models to finish materializing.\n",
                MODELS_READY_TIMEOUT_MS / 1000
            ));
        }
        if !self.failures.is_empty() {
            buf.push_str("\nThe following assets failed to load:\n");
            for (label, err) in &self.failures {
                buf.push_str(&format!("  • {label}: {err}\n"));
            }
        }
        buf.push_str("\nCheck the console for more detail.");
        buf
    }
}

/// Walk `roots`, find every Model node in the subtree, and await
/// each one's `asset_status` settling to `Ready` or `Failed`. This is
/// what lets callers keep the loading modal up until the renderer
/// bridge has actually allocated the GPU instances — the bridge
/// reacts to `bump_revision` on a microtask + then schedules
/// `instantiate_model_template`, so the synchronous tree-mutation is
/// well ahead of the visible draw.
///
/// Returns any per-node failures + a timeout flag so the caller can
/// surface them via `Modal::error` instead of silently closing the
/// loading modal with a half-built scene.
pub async fn wait_for_models_ready(roots: &[Arc<Node>]) -> ModelsReady {
    use futures::FutureExt;
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

    let wait = async move {
        let mut failures: Vec<(String, String)> = Vec::new();
        for node in models {
            let mut stream = node.asset_status.signal_cloned().to_stream();
            while let Some(status) = stream.next().await {
                match status {
                    AssetStatus::Ready => break,
                    AssetStatus::Failed(err) => {
                        let label = node.name.get_cloned();
                        failures.push((label, err));
                        break;
                    }
                    AssetStatus::Idle | AssetStatus::Loading => continue,
                }
            }
        }
        failures
    };

    let timeout = gloo_timers::future::TimeoutFuture::new(MODELS_READY_TIMEOUT_MS);
    futures::select! {
        failures = wait.fuse() => ModelsReady { failures, timed_out: false },
        _ = timeout.fuse() => ModelsReady {
            failures: Vec::new(),
            timed_out: true,
        },
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
