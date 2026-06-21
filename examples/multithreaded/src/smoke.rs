//! M0 — 2-worker shared-memory smoke.
//!
//! Worker **A** increments [`SHARED_COUNTER`] (a native `AtomicU32`
//! living in the shared `WebAssembly.Memory`). Worker **B** *only reads*
//! it. Because both workers attached to the *same* linear memory, B sees
//! A's increments cross the thread boundary with **zero `postMessage`**
//! on the shared-state path — the foundation every later milestone
//! builds on. Each worker also posts its current value to the main
//! thread purely so the page (and the Chrome DevTools MCP gate) can
//! observe the cross-thread progression from JS.

use std::sync::atomic::{AtomicU32, Ordering};

use wasm_bindgen::prelude::*;
use web_sys::js_sys;

/// The shared counter. A single static in wasm linear memory; when two
/// worker instances share one `WebAssembly.Memory`, they reference the
/// *same* address, so A's writes are visible to B's reads. Only worker A
/// ever writes; worker B only reads.
static SHARED_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Main-thread side: spawn worker A (incrementer) and worker B
/// (observer), each sharing this thread's wasm memory. Record what each
/// posts back into a `globalThis.__mt_smoke` object so the gate can read
/// the cross-thread progression.
pub fn start_main() -> Result<(), JsValue> {
    let global = js_sys::global();
    let state = js_sys::Object::new();
    set(&state, "a", &JsValue::from_f64(0.0));
    set(&state, "b", &JsValue::from_f64(0.0));
    set(&state, "b_observations", &js_sys::Array::new());
    let _ = js_sys::Reflect::set(&global, &JsValue::from_str("__mt_smoke"), &state);

    let on_a = make_main_listener("a");
    let on_b = make_main_listener("b");
    crate::bootstrap::spawn_shared_worker("a", on_a.as_ref().unchecked_ref())?;
    crate::bootstrap::spawn_shared_worker("b", on_b.as_ref().unchecked_ref())?;
    on_a.forget();
    on_b.forget();

    tracing::info!("smoke: spawned worker A (increment) + worker B (observe)");
    Ok(())
}

/// Build the main-thread `onmessage` listener for a worker. It records
/// the reported value into `globalThis.__mt_smoke[role]` and, for B,
/// appends to `b_observations` so the gate can confirm a monotonic
/// progression sourced from A across the thread boundary.
fn make_main_listener(role: &str) -> Closure<dyn FnMut(web_sys::MessageEvent)> {
    let role = role.to_string();
    Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
        let data = e.data();
        let value = js_sys::Reflect::get(&data, &JsValue::from_str("value"))
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let global = js_sys::global();
        if let Ok(state) = js_sys::Reflect::get(&global, &JsValue::from_str("__mt_smoke")) {
            let state: js_sys::Object = state.unchecked_into();
            set(&state, &role, &JsValue::from_f64(value));
            if role == "b" {
                if let Ok(arr) = js_sys::Reflect::get(&state, &JsValue::from_str("b_observations"))
                {
                    let arr: js_sys::Array = arr.unchecked_into();
                    arr.push(&JsValue::from_f64(value));
                }
            }
        }
        tracing::info!("main: worker {role} reported {value}");
    })
}

/// Worker-side role dispatch (called via
/// [`crate::bootstrap::mt_worker_start`]).
pub fn worker_dispatch(role: &str) -> Result<(), JsValue> {
    match role {
        "a" => start_incrementer(),
        "b" => start_observer(),
        other => {
            tracing::warn!("smoke: unknown worker role {other:?}");
            Ok(())
        }
    }
}

/// Worker A: increment the shared counter every ~100ms and post the new
/// value to the main thread.
fn start_incrementer() -> Result<(), JsValue> {
    tracing::info!("smoke: worker A (incrementer) started");
    repeat_every(100, || {
        let v = SHARED_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
        post_to_main("a", v);
        tracing::info!("A: incremented shared counter to {v}");
    })
}

/// Worker B: read (never write) the shared counter every ~130ms and post
/// the observed value to the main thread. Seeing it climb proves B reads
/// the same memory A writes.
fn start_observer() -> Result<(), JsValue> {
    tracing::info!("smoke: worker B (observer) started");
    repeat_every(130, || {
        let v = SHARED_COUNTER.load(Ordering::Relaxed);
        post_to_main("b", v);
        tracing::info!(
            "B: observed shared counter = {v} (written by A, across the thread boundary)"
        );
    })
}

/// Post `{ kind: "smoke", role, value }` from a worker to the main thread.
fn post_to_main(role: &str, value: u32) {
    let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
    let msg = js_sys::Object::new();
    set(&msg, "kind", &JsValue::from_str("smoke"));
    set(&msg, "role", &JsValue::from_str(role));
    set(&msg, "value", &JsValue::from_f64(value as f64));
    let _ = scope.post_message(&msg);
}

/// Schedule `f` to run every `ms` milliseconds via the worker scope's
/// `setTimeout` (self-rescheduling so each tick re-arms the next). The
/// closure is leaked for the lifetime of the worker.
fn repeat_every<F: FnMut() + 'static>(ms: i32, mut f: F) -> Result<(), JsValue> {
    let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
    let holder: std::rc::Rc<std::cell::RefCell<Option<Closure<dyn FnMut()>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let holder_run = holder.clone();
    let scope_run = scope.clone();
    *holder.borrow_mut() = Some(Closure::new(move || {
        f();
        if let Some(cb) = holder_run.borrow().as_ref() {
            let _ = scope_run.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                ms,
            );
        }
    }));
    if let Some(cb) = holder.borrow().as_ref() {
        scope.set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            ms,
        )?;
    }
    std::mem::forget(holder);
    Ok(())
}

fn set(obj: &js_sys::Object, key: &str, value: &JsValue) {
    let _ = js_sys::Reflect::set(obj, &JsValue::from_str(key), value);
}
