//! Worker-side dispatcher.
//!
//! The worker's wasm-bindgen JS glue (loaded by
//! [`crate::workers::blob::WORKER_BOOTSTRAP_JS`]) calls
//! [`awsm_worker_entry`] once the shared `WebAssembly.Module` is
//! initialised. This installs the `self.onmessage` listener that
//! routes incoming `awsm-job` payloads by `J::NAME` to the matching
//! registered handler.

use std::cell::RefCell;
use std::collections::HashMap;

use web_sys::js_sys::{Object, Reflect};
use serde::{de::DeserializeOwned, Serialize};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::MessageEvent;

use crate::workers::pool::WorkerJob;

type HandlerFn = Box<dyn Fn(JsValue) -> Result<JsValue, String> + 'static>;

thread_local! {
    /// Job-name → execute closure. Populated by `register::<J>()`.
    /// On the worker thread this is the dispatch table; on the main
    /// thread it's the registration sanity-check table.
    static REGISTRY: RefCell<HashMap<&'static str, HandlerFn>> = RefCell::new(HashMap::new());

    /// Holds the `self.onmessage` closure for the worker context.
    /// Kept alive for the worker's lifetime.
    static ONMESSAGE_HOLDER: RefCell<Option<Closure<dyn FnMut(MessageEvent)>>> = RefCell::new(None);
}

/// Register a `WorkerJob` impl. Idempotent — duplicate registrations
/// for the same `NAME` are no-ops with a debug log.
pub(crate) fn register<J: WorkerJob>() {
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        if r.contains_key(J::NAME) {
            tracing::debug!("workers: job {} already registered; ignoring", J::NAME);
            return;
        }
        let handler: HandlerFn = Box::new(|input_js: JsValue| {
            let input: J::Input = serde_wasm_bindgen::from_value(input_js)
                .map_err(|e| format!("deserialize input: {e}"))?;
            let output = J::execute(input);
            serde_wasm_bindgen::to_value(&output).map_err(|e| format!("serialize output: {e}"))
        });
        r.insert(J::NAME, handler);
    });
}

/// Check whether a job name was registered. Used by
/// [`crate::workers::pool::WorkerPool::dispatch`] for an early
/// "unknown job" error.
pub(crate) fn is_registered(name: &str) -> bool {
    REGISTRY.with(|r| r.borrow().contains_key(name))
}

/// Worker-side entry. Called by the bootstrap JS after the shared
/// `WebAssembly.Module` finishes initialising. Installs the message
/// listener that dispatches incoming `awsm-job` messages.
#[wasm_bindgen]
pub fn awsm_worker_entry() {
    // Grab the worker global scope; if we're on the main thread (which
    // shouldn't happen — the bootstrap JS only fires this in workers
    // — bail rather than crash).
    let global = web_sys::js_sys::global();
    let worker_scope = match global.dyn_into::<web_sys::DedicatedWorkerGlobalScope>() {
        Ok(s) => s,
        Err(_) => {
            tracing::warn!("awsm_worker_entry called outside a worker; no-op");
            return;
        }
    };

    let worker_scope_clone = worker_scope.clone();
    let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
        let data = e.data();
        let kind = Reflect::get(&data, &JsValue::from_str("kind"))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        if kind != "awsm-job" {
            return;
        }
        let id = Reflect::get(&data, &JsValue::from_str("id"))
            .ok()
            .and_then(|v| v.as_f64())
            .map(|f| f as u64)
            .unwrap_or(0);
        let name = Reflect::get(&data, &JsValue::from_str("name"))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        let input = Reflect::get(&data, &JsValue::from_str("input"))
            .unwrap_or(JsValue::UNDEFINED);

        let result_kind;
        let payload_or_msg: JsValue;
        let outcome = REGISTRY.with(|r| {
            let r = r.borrow();
            match r.get(name.as_str()) {
                Some(handler) => (handler)(input),
                None => Err(format!("unknown job: {name}")),
            }
        });
        match outcome {
            Ok(payload) => {
                result_kind = "awsm-result";
                payload_or_msg = payload;
            }
            Err(msg) => {
                result_kind = "awsm-error";
                payload_or_msg = JsValue::from_str(&msg);
            }
        }

        let response = Object::new();
        let _ = Reflect::set(
            &response,
            &JsValue::from_str("kind"),
            &JsValue::from_str(result_kind),
        );
        let _ = Reflect::set(&response, &JsValue::from_str("id"), &JsValue::from_f64(id as f64));
        if result_kind == "awsm-result" {
            let _ = Reflect::set(&response, &JsValue::from_str("payload"), &payload_or_msg);
        } else {
            let _ = Reflect::set(&response, &JsValue::from_str("message"), &payload_or_msg);
        }
        if let Err(err) = worker_scope_clone.post_message(&response) {
            tracing::warn!("worker post_message failed: {err:?}");
        }
    });
    worker_scope.set_onmessage(Some(onmessage.as_ref().unchecked_ref::<web_sys::js_sys::Function>()));
    // Keep the closure alive for the worker's lifetime.
    ONMESSAGE_HOLDER.with(|h| {
        *h.borrow_mut() = Some(onmessage);
    });
}

/// A trivial `WorkerJob` used by the round-trip smoke test in
/// `tests/`. Lives next to the dispatcher so consumers can flip the
/// feature on for diagnostics.
pub struct EchoJob;

#[derive(Clone, Debug, Serialize, serde::Deserialize)]
pub struct EchoInput {
    pub message: String,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize)]
pub struct EchoOutput {
    pub message: String,
}

impl WorkerJob for EchoJob {
    const NAME: &'static str = "echo";
    type Input = EchoInput;
    type Output = EchoOutput;

    fn execute(input: Self::Input) -> Self::Output {
        EchoOutput {
            message: format!("echo: {}", input.message),
        }
    }
}

// Keep `DeserializeOwned` referenced so the auto-bound is exercised.
#[allow(dead_code)]
fn _enforce_bounds<T: Serialize + DeserializeOwned>() {}
