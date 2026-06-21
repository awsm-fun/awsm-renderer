//! Worker spawning + the shared-memory bootstrap JS.
//!
//! Spawning a worker that shares this thread's `WebAssembly.Memory` is
//! the whole game for real wasm threads. The recipe (from
//! `wasm-bindgen`'s `raytrace-parallel` / `wasm-bindgen-rayon`):
//!
//! 1. Build a `Worker` from an inline blob URL (no separate `worker.js`
//!    — the source travels inside the wasm bundle).
//! 2. Post it `{ wasm_module, memory, glue_url, role, … }`.
//!    - `wasm_module` = [`wasm_bindgen::module`] — the *compiled*
//!      artifact, structured-cloneable, so each worker skips re-compiling
//!      the multi-MB binary.
//!    - `memory` = [`wasm_bindgen::memory`] — the **shared**
//!      `WebAssembly.Memory` (shared because the bundle is built with
//!      `+atomics`). Passing it makes the worker attach to the same
//!      linear memory instead of allocating its own.
//! 3. Worker side: `await init({ module_or_path: wasm_module, memory })`,
//!    then call the role entry point.

use wasm_bindgen::prelude::*;
use web_sys::js_sys;
use web_sys::{Blob, BlobPropertyBag, Url, Worker, WorkerOptions, WorkerType};

/// Spawn a worker that shares this thread's wasm module + linear memory,
/// tagged with `role` (read back by the bootstrap JS to pick an entry
/// point). `on_message` is installed as the worker's `onmessage` so the
/// main thread can observe what the worker posts back.
pub fn spawn_shared_worker(role: &str, on_message: &js_sys::Function) -> Result<Worker, JsValue> {
    let blob_options = BlobPropertyBag::new();
    blob_options.set_type("application/javascript");
    let parts = js_sys::Array::new_with_length(1);
    parts.set(0, JsValue::from_str(WORKER_BOOTSTRAP_JS));
    let blob = Blob::new_with_str_sequence_and_options(&parts.into(), &blob_options)?;
    let blob_url = Url::create_object_url_with_blob(&blob)?;

    let opts = WorkerOptions::new();
    opts.set_type(WorkerType::Module);
    let worker = Worker::new_with_options(&blob_url, &opts)?;
    let _ = Url::revoke_object_url(&blob_url);

    worker.set_onmessage(Some(on_message));
    let onerror = Closure::<dyn FnMut(JsValue)>::new(|err: JsValue| {
        web_sys::console::error_2(&JsValue::from_str("worker error:"), &err);
    });
    worker.set_onerror(Some(onerror.as_ref().unchecked_ref::<js_sys::Function>()));
    onerror.forget();

    let init_msg = js_sys::Object::new();
    set(&init_msg, "kind", &JsValue::from_str("awsm-mt-init"));
    set(&init_msg, "wasm_module", &wasm_bindgen::module());
    set(&init_msg, "memory", &wasm_bindgen::memory());
    set(&init_msg, "glue_url", &JsValue::from_str(&bundle_url()));
    set(&init_msg, "role", &JsValue::from_str(role));
    worker.post_message(&init_msg)?;

    Ok(worker)
}

fn set(obj: &js_sys::Object, key: &str, value: &JsValue) {
    let _ = js_sys::Reflect::set(obj, &JsValue::from_str(key), value);
}

/// Worker bootstrap JS. Attaches the worker to the **shared**
/// `WebAssembly.Memory` posted by the main thread, then dispatches to a
/// role entry point.
///
/// `init({ module_or_path, memory })` is the `wasm-bindgen` `--target
/// web` default export's options form (supported since 0.2.93). Passing
/// `memory` is what makes the worker share linear memory rather than
/// instantiate a fresh one.
pub const WORKER_BOOTSTRAP_JS: &str = r#"
self.onmessage = async (e) => {
    const d = e.data;
    if (!d || d.kind !== "awsm-mt-init") return;
    const { wasm_module, memory, glue_url, role } = d;
    try {
        const wbg = await import(glue_url);
        await wbg.default({ module_or_path: wasm_module, memory });
        // boot() ran during init (worker scope → no-op). Now trigger the
        // role-specific work directly (a worker can't postMessage itself).
        wbg.mt_worker_start(role);
    } catch (err) {
        self.postMessage({ kind: "awsm-mt-init-error", message: (err && err.message) ? err.message : String(err) });
    }
};
"#;

/// The worker-side entry point the bootstrap JS calls after init.
/// Dispatches on `role`.
#[wasm_bindgen]
pub fn mt_worker_start(role: String) -> Result<(), JsValue> {
    crate::install_tracing();
    crate::smoke::worker_dispatch(&role)
}

/// Recover the JS-glue bundle URL from the page (Trunk hashes the
/// filename in release builds, so it can't be hard-coded). Falls back to
/// `import.meta.url` outside a DOM context.
#[wasm_bindgen(inline_js = r#"
export function awsm_mt_bundle_url() {
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
    fn awsm_mt_bundle_url() -> String;
}

/// The resolved JS-glue bundle URL (see [`awsm_mt_bundle_url`]).
pub fn bundle_url() -> String {
    awsm_mt_bundle_url()
}
