//! Runtime-global picking helpers — bridges the main-thread
//! [`web_sys::Window`] and worker [`web_sys::DedicatedWorkerGlobalScope`]
//! APIs behind a single call surface.
//!
//! Phase 4.4 scaffolding: the [`OffscreenCanvas`] deployment mode runs
//! the entire renderer inside a worker, where `web_sys::window()`
//! returns `None`. Code paths that need a `Window` for navigator /
//! performance / requestAnimationFrame go through these helpers
//! instead so the same renderer source works in both contexts.
//!
//! ### Scope (this commit — scaffolding only)
//!
//! This module ships the *call surface*. The renderer codebase still
//! reaches for `web_sys::window()` directly in many places; the
//! mechanical audit-and-replace pass is a follow-up. The helpers are
//! published now so new code (the upcoming Phase 4.4 worker
//! example, future Phase 4.3 jobs) can use them from day one without
//! waiting on the codebase-wide migration.
//!
//! ### Why pick at runtime, not at compile time
//!
//! The same wasm binary serves both deployment modes. A consumer's
//! game might run on main thread (small browser game with DOM
//! overlay) while the same library compiled in the same build is
//! used for a worker-mode shipped title. Compile-time gating
//! (`#[cfg(target_feature = "worker")]` etc.) would force consumers
//! to ship two builds, defeating the "library, both modes
//! first-class" promise.

use wasm_bindgen::{JsCast, JsValue};
use web_sys::js_sys;

/// Return the current JS global scope. On the main thread that's
/// `window`; in a worker it's the `DedicatedWorkerGlobalScope`.
pub fn global() -> js_sys::Object {
    js_sys::global().unchecked_into::<js_sys::Object>()
}

/// `Some(window)` on the main thread; `None` in a worker.
pub fn window() -> Option<web_sys::Window> {
    web_sys::window()
}

/// `Some(scope)` in a worker; `None` on the main thread.
pub fn worker_scope() -> Option<web_sys::DedicatedWorkerGlobalScope> {
    js_sys::global()
        .dyn_into::<web_sys::DedicatedWorkerGlobalScope>()
        .ok()
}

/// `navigator.gpu` from whichever global is active. Returns `None`
/// if WebGPU isn't exposed in the current context (locked behind a
/// flag, browser doesn't support, etc.).
///
/// `web_sys::Navigator::gpu()` is infallible at the binding level —
/// it returns a `Gpu` even when the underlying JS value is
/// `null`/`undefined` (Firefox without WebGPU enabled, Safari without
/// the relevant about:flags toggle, headless environments, etc.).
/// Without the `is_null() / is_undefined()` filter below, callers
/// would get a `Some(gpu)` that explodes on first use; the filter
/// makes the `Option` actually meaningful.
pub fn navigator_gpu() -> Option<web_sys::Gpu> {
    if let Some(w) = window() {
        let gpu = w.navigator().gpu();
        if gpu.is_null() || gpu.is_undefined() {
            return None;
        }
        return Some(gpu);
    }
    if let Some(ws) = worker_scope() {
        let gpu = ws.navigator().gpu();
        if gpu.is_null() || gpu.is_undefined() {
            return None;
        }
        return Some(gpu);
    }
    None
}

/// `performance` from whichever global is active. Workers expose
/// the same `Performance` API as main-thread (`performance.now()`,
/// `performance.measure(..)`, etc.).
pub fn performance() -> Option<web_sys::Performance> {
    if let Some(w) = window() {
        return w.performance();
    }
    if let Some(ws) = worker_scope() {
        return ws.performance();
    }
    None
}

/// Schedule a `requestAnimationFrame` callback against whichever
/// global is active. `DedicatedWorkerGlobalScope::requestAnimationFrame`
/// has shipped in every major browser since 2023.
pub fn request_animation_frame(callback: &js_sys::Function) -> Result<i32, JsValue> {
    if let Some(w) = window() {
        return w.request_animation_frame(callback);
    }
    if let Some(ws) = worker_scope() {
        return ws.request_animation_frame(callback);
    }
    Err(JsValue::from_str(
        "request_animation_frame: no main-thread Window or DedicatedWorkerGlobalScope",
    ))
}
