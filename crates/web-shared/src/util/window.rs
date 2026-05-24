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
/// renders before any WASM has run. Apps call this once everything
/// that needs to happen behind the boot-loader (renderer init,
/// shader pre-warm, asset preload, …) is *actually* done — leaving
/// the boot-loader up while real work runs is the whole reason its
/// message can be updated via [`set_boot_loader_message`].
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

/// Update the `.word` text inside the inline `#boot-loader` HTML
/// element. Consumers call this as they advance through init phases
/// (renderer creation → shader pre-warm → editor asset preload → …)
/// so the user sees specific labels rather than a vague "Loading"
/// for the multi-second cold-start window.
///
/// No-op when the boot-loader has already been removed (i.e. after
/// `remove_boot_loader` ran) or when called in a worker context.
/// The previous text is replaced atomically — set the new label
/// *before* awaiting the work the label describes.
///
/// Recommended phase labels (consumer-side convention, not enforced):
///
///   - `"Initializing renderer"` — between `main()` start and the
///     async `AwsmRenderer::new` future resolving. The heaviest
///     compile cost lives here (12+ pipelines × shader_id).
///   - `"Compiling shaders"` — wrapping `AwsmRenderer::prewarm_pipelines()`
///     if the consumer chooses to call it.
///   - `"Loading assets"` — for gltf / texture / scene-data fetches.
///   - `"Populating scene"` — for the `populate_gltf` flush.
///
/// The HTML side keeps the spinner running; only the text changes.
pub fn set_boot_loader_message(message: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(doc) = window.document() else {
        return;
    };
    let Some(boot_loader) = doc.get_element_by_id("boot-loader") else {
        return;
    };
    // The static HTML wraps the label in `<div class="word">…</div>`.
    // Update its `textContent` so the spinner sibling is left alone.
    if let Some(word) = boot_loader.query_selector(".word").ok().flatten() {
        word.set_text_content(Some(message));
    }
}
