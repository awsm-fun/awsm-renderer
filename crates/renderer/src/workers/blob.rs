//! Blob-URL helper for creating Workers from an inline JS string.
//!
//! The library ships as a Rust crate; consumers may use Trunk,
//! webpack, Vite, or no bundler at all — we cannot assume a
//! separate `worker.js` file lives at a known path. Building the
//! worker's bootstrap JS from a `Blob` URL means the worker source
//! travels inside the wasm bundle and the consumer's build pipeline
//! sees a single artifact.

use wasm_bindgen::{JsCast, JsValue};
use web_sys::js_sys::Array;
use web_sys::{Blob, BlobPropertyBag, Url, Worker, WorkerOptions};

/// Build a `Worker` from inline JS source. The returned worker owns
/// the blob URL; we revoke immediately after spawn (the worker has
/// already loaded the source by then).
pub fn new_worker_from_js(js: &str, options: Option<WorkerOptions>) -> Result<Worker, JsValue> {
    let blob_options = BlobPropertyBag::new();
    blob_options.set_type("application/javascript");
    let blob_parts = Array::new_with_length(1);
    blob_parts.set(0, JsValue::from_str(js));
    let blob = Blob::new_with_str_sequence_and_options(&blob_parts.into(), &blob_options)?;
    let blob_url = Url::create_object_url_with_blob(&blob)?;
    let worker = match options {
        Some(options) => Worker::new_with_options(&blob_url, &options)?,
        None => Worker::new(&blob_url)?,
    };
    Url::revoke_object_url(&blob_url)?;
    Ok(worker)
}

/// Coerce a `JsValue` to a `WebAssembly::Module` (the compiled
/// artifact, not the linear-memory Instance — safe to share across
/// workers via structured clone).
pub fn current_wasm_module() -> Result<JsValue, JsValue> {
    // `wasm_bindgen::module()` returns the WebAssembly.Module the
    // host runtime is currently executing. The cast is a no-op on
    // wasm32 — there is exactly one module at runtime.
    let module = wasm_bindgen::module();
    // Sanity: ensure it's not undefined (some test harnesses don't
    // populate this).
    if module.is_undefined() || module.is_null() {
        return Err(JsValue::from_str(
            "wasm_bindgen::module() returned undefined; worker bootstrap requires the shared module",
        ));
    }
    Ok(module)
}

/// Inline JS — embedded via `#[wasm_bindgen(inline_js = ...)]`
/// elsewhere — that exposes `import.meta.url` from inside the
/// wasm-bindgen JS glue. The library author can't predict the
/// consumer's bundle filename (Trunk hashes release builds, Vite
/// chunks ESM, etc.), so we read the resolved URL at runtime.
#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
export function awsm_bundle_url() {
    if (typeof document !== "undefined") {
        const scripts = document.querySelectorAll("script[type=module]");
        for (const s of scripts) {
            const t = s.textContent || "";
            const m = t.match(/from\s+['"]([^'"]+\.js)['"]/);
            if (m) return new URL(m[1], location.href).href;
        }
    }
    return import.meta.url;
}
"#)]
extern "C" {
    pub fn awsm_bundle_url() -> String;
}

/// The worker-side bootstrap JS. Waits for the main thread to post
/// the pre-compiled `WebAssembly.Module` and the JS glue URL, then
/// initialises the module with a shared compiled artifact (avoids
/// re-compiling the multi-MB Rust binary in every worker). After
/// init, the listener installed by [`awsm_worker_entry`] takes over
/// for subsequent job-dispatch messages.
pub const WORKER_BOOTSTRAP_JS: &str = r#"
self.onmessage = async (e) => {
    if (e.data && e.data.kind === "awsm-init") {
        const { wasm_module, glue_url } = e.data;
        try {
            const wbg = await import(glue_url);
            // wasm-bindgen's `--target web` default export accepts a
            // pre-compiled Module and skips re-compile.
            await wbg.default(wasm_module);
            wbg.awsm_worker_entry();
            self.postMessage({ kind: "awsm-ready" });
        } catch (err) {
            self.postMessage({
                kind: "awsm-init-error",
                message: (err && err.message) ? err.message : String(err),
            });
        }
        return;
    }
    // Subsequent messages handled by the listener installed by
    // awsm_worker_entry().
};
"#;

/// Cast a `JsValue` to `Worker` for `Drop::terminate` plumbing.
#[allow(dead_code)]
pub fn coerce_worker(value: JsValue) -> Option<Worker> {
    value.dyn_into().ok()
}
