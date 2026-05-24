use std::sync::LazyLock;

use wasm_bindgen::UnwrapThrowExt;

/// Main-thread `Window`. Lazy because some `cdylib` consumers (Phase
/// 4.4 worker-mode renderers, Phase 4.3a worker-job pools) re-use
/// this crate's wasm-bindgen glue in a worker context where
/// `web_sys::window()` returns `None`. Accessing `WINDOW` from a
/// worker panics — by design — but the entry-point helpers below
/// short-circuit on `web_sys::window().is_none()` so the panic only
/// fires on a *real* main-thread misuse.
pub static WINDOW: LazyLock<web_sys::Window> =
    LazyLock::new(|| web_sys::window().expect_throw("Window is not available"));

/// Remove the inline `#boot-loader` element that each `index.html`
/// renders before any WASM has run. Apps call this at the top of their
/// `main()` so the spinner disappears the moment the Rust entry point
/// starts executing — i.e. once WASM download + compile + bindgen-init
/// has finished, which is the part that produces the "blank screen for
/// a while" gap on cold loads.
///
/// Worker-safe: returns silently when there's no `Window` (worker
/// scope), so re-using the consumer's wasm bundle in a Phase 4.3a
/// pool worker doesn't panic on init.
pub fn remove_boot_loader() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(doc) = window.document() else {
        return;
    };
    if let Some(el) = doc.get_element_by_id("boot-loader") {
        el.remove();
    }
}
