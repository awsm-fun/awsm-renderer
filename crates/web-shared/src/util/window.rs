use std::sync::LazyLock;

use wasm_bindgen::UnwrapThrowExt;

pub static WINDOW: LazyLock<web_sys::Window> =
    LazyLock::new(|| web_sys::window().expect_throw("Window is not available"));

/// Remove the inline `#boot-loader` element that each `index.html`
/// renders before any WASM has run. Apps call this at the top of their
/// `main()` so the spinner disappears the moment the Rust entry point
/// starts executing — i.e. once WASM download + compile + bindgen-init
/// has finished, which is the part that produces the "blank screen for
/// a while" gap on cold loads.
pub fn remove_boot_loader() {
    let Some(doc) = WINDOW.document() else { return };
    if let Some(el) = doc.get_element_by_id("boot-loader") {
        el.remove();
    }
}
