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
use std::future::Future;
use std::pin::Pin;

use serde::{de::DeserializeOwned, Serialize};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::spawn_local;
use web_sys::js_sys::{Object, Reflect};
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

use crate::workers::pool::WorkerJob;

/// Async handler signature: given the deserialised JsValue input,
/// return a future that resolves to either `(payload, transfer_list)`
/// (the worker→main response) or an error string. The future is
/// driven via `spawn_local` so the worker thread stays responsive to
/// the next incoming message.
///
/// The `transfer_list` lets the job hand off ownership of JS handles
/// (`ImageBitmap`, `ArrayBuffer`, …) instead of structured-cloning
/// them. Pure-data jobs return an empty array; `GltfParseJob`'s
/// `into_response_message` override fills it with the decoded image
/// bitmaps and the worker dispatcher routes through
/// `postMessage(_, transfer)` so the main thread receives the
/// handles in O(1).
type HandlerFn = Box<
    dyn Fn(
            JsValue,
        )
            -> Pin<Box<dyn Future<Output = Result<(JsValue, web_sys::js_sys::Array), String>>>>
        + 'static,
>;

thread_local! {
    /// Job-name → handler closure. Populated by `register::<J>()`.
    /// On the worker thread this is the dispatch table; on the main
    /// thread it's the registration sanity-check table.
    static REGISTRY: RefCell<HashMap<&'static str, HandlerFn>> = RefCell::new(HashMap::new());

    /// Holds the `self.onmessage` closure for the worker context.
    /// Kept alive for the worker's lifetime.
    static ONMESSAGE_HOLDER: RefCell<Option<Closure<dyn FnMut(MessageEvent)>>> = RefCell::new(None);
}

/// Register a `WorkerJob` impl in the *current* thread's registry.
///
/// Each worker spawned by [`crate::workers::WorkerPool`] has its own
/// wasm linear memory and therefore its own thread-local registry.
/// The pool's `register::<J>()` only populates the main-thread
/// registry; for the worker side to recognise the job, *both* sides
/// have to call `register_job::<J>()`. The recommended pattern is to
/// extract registration into a free fn the consumer's wasm
/// entry-point (the `pub fn main()` Trunk wires up) calls
/// unconditionally — the same wasm bundle then registers the same
/// jobs on both main thread and pool workers without per-side
/// branching.
///
/// Idempotent — duplicate registrations for the same `NAME` are
/// no-ops with a debug log.
pub fn register_job<J: WorkerJob>() {
    register::<J>()
}

pub(crate) fn register<J: WorkerJob>() {
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        if r.contains_key(J::NAME) {
            tracing::debug!("workers: job {} already registered; ignoring", J::NAME);
            return;
        }
        let handler: HandlerFn = Box::new(|input_js: JsValue| {
            Box::pin(async move {
                let input: J::Input = serde_wasm_bindgen::from_value(input_js)
                    .map_err(|e| format!("deserialize input: {e}"))?;
                let output = J::execute(input)
                    .await
                    .map_err(|e| format!("execute: {e}"))?;
                // Trait hook: jobs that produce JS handles (ImageBitmap,
                // ArrayBuffer, …) override this to attach them to the
                // payload object AND collect them into the transfer
                // list so `post_message_with_transfer` lifts them
                // across the worker boundary in O(1).
                J::into_response_message(output)
            })
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
    let global = web_sys::js_sys::global();
    let worker_scope = match global.dyn_into::<DedicatedWorkerGlobalScope>() {
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
        // String-encoded job id (full u64 precision; see the
        // matching dispatch site in `pool.rs::dispatch_inner` for
        // the rationale — JS Number tops out at 2^53). A malformed
        // / missing id falls back to 0 so the worker still emits a
        // response with *some* id — the main side's `parse_job_id`
        // will just fail to find the entry and log instead.
        let id = crate::workers::pool::parse_job_id(&data).unwrap_or(0);
        let name = Reflect::get(&data, &JsValue::from_str("name"))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        let input = Reflect::get(&data, &JsValue::from_str("input")).unwrap_or(JsValue::UNDEFINED);

        // Resolve the handler synchronously (cheap registry lookup),
        // then drive its future with `spawn_local` so the dispatch
        // loop returns immediately and can process the next message.
        let handler_future = REGISTRY.with(|r| {
            let r = r.borrow();
            r.get(name.as_str()).map(|handler| (handler)(input))
        });

        let scope = worker_scope_clone.clone();
        match handler_future {
            Some(fut) => {
                spawn_local(async move {
                    let outcome = fut.await;
                    let response = Object::new();
                    let _ = Reflect::set(
                        &response,
                        &JsValue::from_str("id"),
                        &JsValue::from_str(&id.to_string()),
                    );
                    let mut transfer_list: Option<web_sys::js_sys::Array> = None;
                    match outcome {
                        Ok((payload, transfer)) => {
                            let _ = Reflect::set(
                                &response,
                                &JsValue::from_str("kind"),
                                &JsValue::from_str("awsm-result"),
                            );
                            let _ =
                                Reflect::set(&response, &JsValue::from_str("payload"), &payload);
                            if transfer.length() > 0 {
                                transfer_list = Some(transfer);
                            }
                        }
                        Err(msg) => {
                            let _ = Reflect::set(
                                &response,
                                &JsValue::from_str("kind"),
                                &JsValue::from_str("awsm-error"),
                            );
                            let _ = Reflect::set(
                                &response,
                                &JsValue::from_str("message"),
                                &JsValue::from_str(&msg),
                            );
                        }
                    }
                    let post_result = match transfer_list {
                        Some(arr) => scope.post_message_with_transfer(&response, &arr),
                        None => scope.post_message(&response),
                    };
                    if let Err(err) = post_result {
                        tracing::warn!("worker post_message failed: {err:?}");
                    }
                });
            }
            None => {
                let response = Object::new();
                let _ = Reflect::set(
                    &response,
                    &JsValue::from_str("kind"),
                    &JsValue::from_str("awsm-error"),
                );
                let _ = Reflect::set(
                    &response,
                    &JsValue::from_str("id"),
                    &JsValue::from_str(&id.to_string()),
                );
                let _ = Reflect::set(
                    &response,
                    &JsValue::from_str("message"),
                    &JsValue::from_str(&format!("unknown job: {name}")),
                );
                if let Err(err) = scope.post_message(&response) {
                    tracing::warn!("worker post_message failed: {err:?}");
                }
            }
        }
    });
    worker_scope.set_onmessage(Some(
        onmessage
            .as_ref()
            .unchecked_ref::<web_sys::js_sys::Function>(),
    ));
    ONMESSAGE_HOLDER.with(|h| {
        *h.borrow_mut() = Some(onmessage);
    });
}

/// A trivial `WorkerJob` used by the round-trip smoke test.
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

    fn execute(input: Self::Input) -> Pin<Box<dyn Future<Output = anyhow::Result<Self::Output>>>> {
        Box::pin(async move {
            Ok(EchoOutput {
                message: format!("echo: {}", input.message),
            })
        })
    }
}

#[allow(dead_code)]
fn _enforce_bounds<T: Serialize + DeserializeOwned>() {}
