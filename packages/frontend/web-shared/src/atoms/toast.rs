//! Lightweight toast for transient user-facing notices (rate-limit hits,
//! save confirmations, etc.). Singleton — call `Toast::show` from anywhere.

use std::cell::RefCell;
use std::time::Duration;

use crate::prelude::*;
use gloo_timers::future::TimeoutFuture;
use wasm_bindgen_futures::spawn_local;

/// A sink invoked for every shown toast (kind + message).
type ToastLogHook = Box<dyn Fn(ToastKind, &str)>;

thread_local! {
    static TOAST: ToastInstance = ToastInstance::new();
    /// Optional sink that every shown toast is mirrored into (besides the
    /// on-screen toast). Lets a host capture the user-facing notice stream into
    /// an in-process log buffer — e.g. the editor's MCP `get_console_logs`.
    static LOG_HOOK: RefCell<Option<ToastLogHook>> = const { RefCell::new(None) };
}

/// Install a sink invoked for every `Toast::show` (kind + message). Replaces any
/// prior hook. Used by the editor to feed its console-log ring buffer.
pub fn set_toast_log_hook(f: impl Fn(ToastKind, &str) + 'static) {
    LOG_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(f)));
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Warning,
    Error,
}

/// Global toast namespace. State lives in the thread-local `TOAST`;
/// this is a zero-sized handle so `Toast::info` / `Toast::warning` /
/// `Toast::error` read like methods.
pub struct Toast;

impl Toast {
    pub fn info(msg: impl Into<String>) {
        Self::show(ToastKind::Info, msg.into(), Duration::from_secs(4));
    }

    pub fn warning(msg: impl Into<String>) {
        Self::show(ToastKind::Warning, msg.into(), Duration::from_secs(6));
    }

    pub fn error(msg: impl Into<String>) {
        Self::show(ToastKind::Error, msg.into(), Duration::from_secs(8));
    }

    pub fn show(kind: ToastKind, msg: String, ttl: Duration) {
        LOG_HOOK.with(|h| {
            if let Some(f) = h.borrow().as_ref() {
                f(kind, &msg);
            }
        });
        TOAST.with(|t| t.show(kind, msg, ttl));
    }

    pub fn render() -> Dom {
        TOAST.with(|t| t.render())
    }

    /// Dismiss the currently-shown toast, if any. The toast is a singleton (one
    /// notice at a time, each replacing the last), so this clears whatever is
    /// visible — a programmatic "dismiss all" for hosts that want to clear the
    /// corner on demand (e.g. the editor's "dismiss MCP notifications").
    pub fn clear() {
        TOAST.with(|t| t.message.set(None));
    }
}

struct ToastInstance {
    kind: Mutable<ToastKind>,
    message: Mutable<Option<String>>,
}

impl ToastInstance {
    fn new() -> Self {
        Self {
            kind: Mutable::new(ToastKind::Info),
            message: Mutable::new(None),
        }
    }

    fn show(&self, kind: ToastKind, msg: String, ttl: Duration) {
        self.kind.set(kind);
        self.message.set(Some(msg));
        let message = self.message.clone();
        spawn_local(async move {
            TimeoutFuture::new(ttl.as_millis() as u32).await;
            message.set(None);
        });
    }

    fn render(&self) -> Dom {
        static DISMISS_BTN: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("position", "absolute")
                .style("top", "0.25rem")
                .style("right", "0.4rem")
                .style("background", "transparent")
                .style("border", "0")
                .style("color", "white")
                .style("font-size", "1.1rem")
                .style("line-height", "1")
                .style("cursor", "pointer")
                .style("padding", "0.15rem 0.35rem")
                .style("opacity", "0.7")
                .pseudo!(":hover", { .style("opacity", "1") })
            }
        });

        let message = self.message.clone();
        let kind = self.kind.clone();

        html!("div", {
            .style("position", "fixed")
            .style("bottom", "1rem")
            .style("right", "1rem")
            .style("z-index", Zindex::Toast.value())
            .style("max-width", "24rem")
            .style_signal("display", message.signal_cloned().map(|m| if m.is_some() { "block" } else { "none" }))
            .child_signal(message.signal_cloned().map(clone!(kind => move |maybe_msg| {
                maybe_msg.map(|msg| {
                    let (bg, border) = match kind.get() {
                        ToastKind::Info => ("rgba(60, 110, 180, 0.95)", "rgba(80, 130, 200, 0.95)"),
                        ToastKind::Warning => ("rgba(180, 130, 40, 0.95)", "rgba(220, 150, 60, 0.95)"),
                        ToastKind::Error => ("rgba(180, 60, 60, 0.95)", "rgba(230, 90, 90, 0.95)"),
                    };
                    html!("div", {
                        .style("padding", "0.6rem 2.25rem 0.6rem 0.95rem")
                        .style("border-radius", "0.5rem")
                        .style("border", &format!("1px solid {border}"))
                        .style("background", bg)
                        .style("color", "white")
                        .style("font-size", FontSize::Md.value())
                        .style("box-shadow", "0 6px 20px rgba(0, 0, 0, 0.35)")
                        .style("position", "relative")
                        .text(&msg)
                        // Manual-dismiss "×" in the top-right. The 4–8s
                        // auto-hide is still there as the lazy path; the
                        // X is for when a toast lingers in the corner and
                        // the user wants it out of the way now.
                        .child(html!("button", {
                            .class(&*DISMISS_BTN)
                            .text("×")
                            .event(clone!(message => move |_: events::Click| {
                                message.set(None);
                            }))
                        }))
                    })
                })
            })))
        })
    }
}
