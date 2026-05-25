//! [`WorkerPool`] — N background workers sharing the consumer's
//! compiled `WebAssembly.Module`.
//!
//! ### Why explicit registration over `linkme`
//!
//! `linkme`'s `distributed_slice!` relies on linker-section magic
//! that does not survive wasm32 linking reliably (the section either
//! gets stripped or merged with adjacent sections in
//! `wasm-bindgen --target web` builds). The portable shape is for
//! the consumer to call `pool.register::<MyJob>()` once per `WorkerJob`
//! impl at init time. The dispatch path then routes by `J::NAME` via
//! the pool's internal `HashMap`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::channel::oneshot;
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::js_sys::{Array, Function, Object, Reflect};
use web_sys::{MessageEvent, Worker, WorkerOptions, WorkerType};

use crate::workers::blob::{
    awsm_bundle_url, current_wasm_module, new_worker_from_js, WORKER_BOOTSTRAP_JS,
};

/// Errors out of [`WorkerPool`].
#[derive(Debug, Error)]
pub enum WorkerPoolError {
    #[error("worker bootstrap failed: {0}")]
    Bootstrap(String),
    #[error("worker postMessage failed: {0}")]
    PostMessage(String),
    #[error("worker job failed: {0}")]
    JobFailed(String),
    #[error("worker job not registered: {0}")]
    UnknownJob(&'static str),
    #[error("worker serialization error: {0}")]
    Serde(String),
    #[error("worker channel dropped before result")]
    ChannelDropped,
}

impl WorkerPoolError {
    /// Extract a human-readable message from a JS error value, falling
    /// back to the `Debug` format when neither `.toString()` nor a
    /// `.message` property is present.
    fn js_message(err: JsValue) -> String {
        err.as_string()
            .or_else(|| {
                Reflect::get(&err, &JsValue::from_str("message"))
                    .ok()
                    .and_then(|v| v.as_string())
            })
            .unwrap_or_else(|| format!("{err:?}"))
    }

    /// Build a `Bootstrap` variant from a JS error raised during pool
    /// construction (worker spawn, custom factory, init postMessage,
    /// shared-module discovery).
    fn bootstrap_from_js(prefix: &'static str, err: JsValue) -> Self {
        WorkerPoolError::Bootstrap(format!("{prefix}: {}", Self::js_message(err)))
    }

    /// Build a `PostMessage` variant from a JS error raised during a
    /// live `dispatch_*` call. Distinct from `Bootstrap` so callers
    /// can distinguish a worker that failed to spawn from one that
    /// dropped a per-frame message.
    fn post_message_from_js(prefix: &'static str, err: JsValue) -> Self {
        WorkerPoolError::PostMessage(format!("{prefix}: {}", Self::js_message(err)))
    }
}

/// A CPU-only job runnable on a [`WorkerPool`] worker.
///
/// Stateless by design — implementations only act on `input`. State
/// the job needs (e.g. a shared cache) goes inside the input.
///
/// `execute` is async so jobs can fetch network resources, await
/// `mapAsync` resolutions, etc. without resorting to a deadlock-prone
/// `block_on`. The worker-side dispatcher (`awsm_worker_entry`) drives
/// the future via `wasm_bindgen_futures::spawn_local` and posts the
/// serialised result back to the main thread when it resolves.
pub trait WorkerJob: 'static {
    /// Unique string identifier; used in the postMessage dispatch.
    const NAME: &'static str;

    type Input: Serialize + DeserializeOwned + 'static;
    type Output: Serialize + DeserializeOwned + 'static;

    /// Runs on the worker thread. Returns a result so transient
    /// failures (network, parsing) can flow back to the main thread
    /// as `WorkerPoolError::JobFailed` rather than a worker panic.
    fn execute(
        input: Self::Input,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Self::Output>>>>;

    /// Worker-side hook: turn the resolved `Output` into the
    /// `(payload, transfer_list)` pair the worker postMessages back.
    /// Default behaviour is `serde_wasm_bindgen::to_value(&output)`
    /// with an empty transfer list — the simple structured-clone
    /// path that suits jobs whose Output is pure-data (no JS handles).
    ///
    /// Override for jobs whose Output carries JS-side handles
    /// (`ImageBitmap`, `ArrayBuffer`, `MessagePort`, …) that must
    /// be *transferred* rather than structured-cloned. The override
    /// can:
    ///   1. Attach the handles to the response payload as named
    ///      properties (e.g. `payload.bitmaps = [ib0, ib1, …]`).
    ///   2. Push the same handles into the returned `js_sys::Array`
    ///      so the worker's `post_message_with_transfer` lifts them
    ///      across the thread boundary in O(1) instead of cloning.
    ///
    /// `from_response_message` then stitches the handles back into
    /// the typed Output on the main side.
    fn into_response_message(
        output: Self::Output,
    ) -> Result<(JsValue, web_sys::js_sys::Array), String> {
        let payload = serde_wasm_bindgen::to_value(&output)
            .map_err(|err| format!("serialize output: {err}"))?;
        Ok((payload, web_sys::js_sys::Array::new()))
    }

    /// Main-thread inverse of `into_response_message`. Default:
    /// `serde_wasm_bindgen::from_value`. Override when the worker
    /// attached transferred handles that need to be merged back into
    /// the Output. The hook receives the *full* response message
    /// (including any properties the worker attached on top of the
    /// serialised payload) — pull whatever it needs and combine with
    /// the standard deserialised body to reconstruct the Output.
    fn from_response_message(payload: JsValue) -> Result<Self::Output, String> {
        serde_wasm_bindgen::from_value(payload).map_err(|err| format!("deserialize output: {err}"))
    }
}

/// Bundle-URL discovery strategy. Default is `Auto` (sniff the glue
/// URL out of the host page's `<script type=module>` tags at runtime).
#[derive(Default)]
pub enum WorkerPoolBootstrap {
    /// Auto-discover the wasm-bindgen `--target web` glue URL.
    ///
    /// **Requires a main-thread / DOM context.** The implementation in
    /// [`crate::workers::blob::awsm_bundle_url`] walks
    /// `document.querySelectorAll("script[type=module]")` to find the
    /// glue `import` statement; if `document` is undefined (i.e. the
    /// pool is being constructed *from inside another worker*), it
    /// falls back to `import.meta.url`, which on a worker resolves to
    /// the inline-snippet module URL rather than the wasm-bindgen
    /// glue — and that won't have the `default` export the worker
    /// bootstrap expects. Spawning a pool from within a worker
    /// therefore needs [`Self::ModuleUrl`] (pass the glue URL
    /// explicitly) or [`Self::Custom`] (construct the `Worker`
    /// yourself).
    #[default]
    Auto,
    /// Explicit bundle URL — for consumers whose build setup doesn't
    /// expose a discoverable script tag, OR for pools being
    /// constructed from a worker context (where `Auto` cannot reach
    /// the DOM).
    ModuleUrl { bundle_url: String },
    /// Escape hatch — consumer constructs the `Worker` themselves;
    /// the pool drives the postMessage protocol on the handle.
    Custom(Box<dyn Fn() -> Result<Worker, JsValue> + 'static>),
}

/// Telemetry counters.
#[derive(Debug, Default, Clone, Copy)]
pub struct WorkerPoolStats {
    pub workers_alive: usize,
    pub jobs_dispatched: u64,
    pub jobs_completed: u64,
    pub jobs_failed: u64,
    /// Accumulated round-trip wall-clock for completed jobs —
    /// from `dispatch_inner` entry (just before the worker `postMessage`)
    /// to the `oneshot::Receiver::await` resolving on the main thread.
    /// With the current round-robin scheduler there's no per-worker
    /// queue (workers are picked synchronously, postMessage is
    /// fire-and-forget), so this is the end-to-end job latency
    /// including worker execution. Renamed from the old `queue_wait_ms`
    /// (which was never actually incremented). Only successful results
    /// are folded in; failures + dropped channels are excluded so the
    /// number stays comparable across runs.
    pub job_round_trip_ms: f64,
}

/// Pending job entry — the result-delivery channel paired with the
/// worker index the job was dispatched to. The worker index lets
/// `onerror` fail *only* the jobs that were sent to the dead worker,
/// instead of spuriously failing in-flight jobs on still-healthy
/// peers when one worker crashes.
struct PendingEntry {
    sender: oneshot::Sender<Result<JsValue, JsValue>>,
    worker_idx: usize,
}

/// Pool of N workers sharing the consumer's compiled
/// `WebAssembly.Module`.
pub struct WorkerPool {
    workers: Vec<WorkerSlot>,
    next_worker: RefCell<usize>,
    next_job_id: AtomicU64,
    pending: Rc<RefCell<HashMap<u64, PendingEntry>>>,
    stats: Rc<RefCell<WorkerPoolStats>>,
    /// Closures that own the `Worker.onmessage` handlers; kept alive
    /// for the lifetime of the pool so the JS callbacks don't free
    /// underneath us.
    _onmessage_closures: Vec<Closure<dyn FnMut(MessageEvent)>>,
    _onerror_closures: Vec<Closure<dyn FnMut(JsValue)>>,
}

struct WorkerSlot {
    worker: Worker,
}

impl WorkerPool {
    /// Most common shape: `WorkerPool::with_workers(None).await?`.
    /// Defaults `worker_count` to `min(navigator.hardwareConcurrency, 4)`.
    pub async fn with_workers(worker_count: Option<usize>) -> Result<Self, WorkerPoolError> {
        let count = worker_count.unwrap_or_else(default_worker_count);
        Self::new(WorkerPoolBootstrap::default(), count).await
    }

    /// Spawn the pool. Blocks (well, await) until every worker has
    /// reported `awsm-ready`.
    pub async fn new(
        bootstrap: WorkerPoolBootstrap,
        worker_count: usize,
    ) -> Result<Self, WorkerPoolError> {
        let worker_count = worker_count.max(1);
        let glue_url = match &bootstrap {
            WorkerPoolBootstrap::Auto => awsm_bundle_url(),
            WorkerPoolBootstrap::ModuleUrl { bundle_url } => bundle_url.clone(),
            WorkerPoolBootstrap::Custom(_) => String::new(),
        };
        let wasm_module = current_wasm_module()
            .map_err(|err| WorkerPoolError::bootstrap_from_js("current_wasm_module", err))?;

        let pending: Rc<RefCell<HashMap<u64, PendingEntry>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let stats = Rc::new(RefCell::new(WorkerPoolStats::default()));

        let mut workers = Vec::with_capacity(worker_count);
        let mut onmessage_closures = Vec::with_capacity(worker_count);
        let mut onerror_closures = Vec::with_capacity(worker_count);
        let mut ready_futures = Vec::with_capacity(worker_count);

        for i in 0..worker_count {
            let worker = match &bootstrap {
                WorkerPoolBootstrap::Auto | WorkerPoolBootstrap::ModuleUrl { .. } => {
                    let opts = WorkerOptions::new();
                    opts.set_type(WorkerType::Module);
                    new_worker_from_js(WORKER_BOOTSTRAP_JS, Some(opts))
                        .map_err(|err| WorkerPoolError::bootstrap_from_js("worker spawn", err))?
                }
                WorkerPoolBootstrap::Custom(factory) => factory().map_err(|err| {
                    WorkerPoolError::bootstrap_from_js("custom worker factory", err)
                })?,
            };

            // Ready future — resolved by the first `awsm-ready` event.
            let (ready_tx, ready_rx) = oneshot::channel::<Result<(), String>>();
            let ready_cell: Rc<RefCell<Option<oneshot::Sender<Result<(), String>>>>> =
                Rc::new(RefCell::new(Some(ready_tx)));

            // Job-result router — when a regular dispatch comes back,
            // pop the JobId out of `pending` and resolve its sender.
            let pending_clone = pending.clone();
            let stats_clone = stats.clone();
            let ready_cell_clone = ready_cell.clone();
            let label = format!("awsm-worker-{i}");

            let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
                let data = e.data();
                let kind = Reflect::get(&data, &JsValue::from_str("kind"))
                    .ok()
                    .and_then(|v| v.as_string())
                    .unwrap_or_default();
                match kind.as_str() {
                    "awsm-ready" => {
                        if let Some(tx) = ready_cell_clone.borrow_mut().take() {
                            let _ = tx.send(Ok(()));
                        }
                    }
                    "awsm-init-error" => {
                        let msg = Reflect::get(&data, &JsValue::from_str("message"))
                            .ok()
                            .and_then(|v| v.as_string())
                            .unwrap_or_else(|| "unknown init error".to_string());
                        if let Some(tx) = ready_cell_clone.borrow_mut().take() {
                            let _ = tx.send(Err(msg));
                        }
                    }
                    "awsm-result" => {
                        let id = parse_job_id(&data);
                        let payload = Reflect::get(&data, &JsValue::from_str("payload"))
                            .unwrap_or(JsValue::UNDEFINED);
                        if let Some(id) = id {
                            if let Some(entry) = pending_clone.borrow_mut().remove(&id) {
                                let _ = entry.sender.send(Ok(payload));
                                stats_clone.borrow_mut().jobs_completed += 1;
                            }
                        }
                    }
                    "awsm-error" => {
                        let id = parse_job_id(&data);
                        let msg = Reflect::get(&data, &JsValue::from_str("message"))
                            .ok()
                            .and_then(|v| v.as_string())
                            .unwrap_or_else(|| "unknown job error".to_string());
                        if let Some(id) = id {
                            if let Some(entry) = pending_clone.borrow_mut().remove(&id) {
                                let _ = entry.sender.send(Err(JsValue::from_str(&msg)));
                                stats_clone.borrow_mut().jobs_failed += 1;
                            }
                        } else {
                            tracing::warn!("{label}: worker error without id: {msg}");
                        }
                    }
                    other => {
                        tracing::debug!("{label}: unknown worker message kind: {other:?}");
                    }
                }
            });
            worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref::<Function>()));
            onmessage_closures.push(onmessage);

            let onerror_label = format!("awsm-worker-{i}");
            let onerror_worker_idx = i;
            let onerror_ready_cell = ready_cell.clone();
            let onerror_pending = pending.clone();
            let onerror_stats = stats.clone();
            let onerror = Closure::<dyn FnMut(JsValue)>::new(move |err: JsValue| {
                let msg = WorkerPoolError::js_message(err);
                tracing::warn!("{onerror_label}: worker onerror: {msg}");
                // Fail the bootstrap ready channel if init never
                // reported either `awsm-ready` or `awsm-init-error`
                // — otherwise `WorkerPool::new` awaits forever.
                if let Some(tx) = onerror_ready_cell.borrow_mut().take() {
                    let _ = tx.send(Err(format!("worker onerror during init: {msg}")));
                }
                // Drain only the in-flight jobs that were dispatched
                // to *this* worker — peers stay healthy and their
                // results still route through `onmessage` normally.
                // `PendingEntry.worker_idx` is set at `dispatch_inner`
                // time so we can filter by ownership here.
                let mut pending = onerror_pending.borrow_mut();
                let mut drained_count: u64 = 0;
                pending.retain(|_id, entry| {
                    if entry.worker_idx == onerror_worker_idx {
                        // `take` the sender out by swapping in a dummy
                        // (`oneshot::channel` discard side) — the
                        // entry is being removed so the worker_idx
                        // becomes irrelevant. `retain`'s closure has
                        // `&mut entry`, so we drain by moving the
                        // sender via a placeholder swap before
                        // returning `false`.
                        let (placeholder_tx, _) = oneshot::channel::<Result<JsValue, JsValue>>();
                        let sender = std::mem::replace(&mut entry.sender, placeholder_tx);
                        let _ = sender.send(Err(JsValue::from_str(&format!(
                            "{onerror_label}: worker errored: {msg}"
                        ))));
                        drained_count += 1;
                        false
                    } else {
                        true
                    }
                });
                if drained_count > 0 {
                    onerror_stats.borrow_mut().jobs_failed += drained_count;
                }
            });
            worker.set_onerror(Some(onerror.as_ref().unchecked_ref::<Function>()));
            onerror_closures.push(onerror);

            // Kick init: post the shared module + glue URL.
            let init_msg = Object::new();
            Reflect::set(
                &init_msg,
                &JsValue::from_str("kind"),
                &JsValue::from_str("awsm-init"),
            )
            .map_err(|err| WorkerPoolError::bootstrap_from_js("init msg", err))?;
            Reflect::set(&init_msg, &JsValue::from_str("wasm_module"), &wasm_module)
                .map_err(|err| WorkerPoolError::bootstrap_from_js("init msg", err))?;
            Reflect::set(
                &init_msg,
                &JsValue::from_str("glue_url"),
                &JsValue::from_str(&glue_url),
            )
            .map_err(|err| WorkerPoolError::bootstrap_from_js("init msg", err))?;
            worker
                .post_message(&init_msg)
                .map_err(|err| WorkerPoolError::bootstrap_from_js("init postMessage", err))?;

            workers.push(WorkerSlot { worker });
            ready_futures.push(ready_rx);
        }

        // Wait for every worker to report ready.
        for (i, rx) in ready_futures.into_iter().enumerate() {
            match rx.await {
                Ok(Ok(())) => {}
                Ok(Err(msg)) => {
                    return Err(WorkerPoolError::Bootstrap(format!(
                        "worker #{i} init: {msg}"
                    )));
                }
                Err(_) => {
                    return Err(WorkerPoolError::Bootstrap(format!(
                        "worker #{i} ready channel dropped"
                    )));
                }
            }
        }

        stats.borrow_mut().workers_alive = workers.len();

        Ok(Self {
            workers,
            next_worker: RefCell::new(0),
            next_job_id: AtomicU64::new(1),
            pending,
            stats,
            _onmessage_closures: onmessage_closures,
            _onerror_closures: onerror_closures,
        })
    }

    /// Register a `WorkerJob` impl. Must be called *before* `dispatch`
    /// for that job type.
    ///
    /// Registration is consumer-side bookkeeping for the
    /// `awsm_worker_entry` worker-side dispatcher; the worker uses a
    /// matching call (also `pool.register::<J>()` via the wasm
    /// bundle the worker loads) to install its execution closure.
    /// In practice, both sides call `pool.register::<J>()` so the
    /// main thread can sanity-check that the job name is known
    /// before kicking dispatch.
    pub fn register<J: WorkerJob>(&self) {
        crate::workers::entry::register::<J>();
    }

    /// Dispatch a job. Round-robins workers.
    pub async fn dispatch<J: WorkerJob>(
        &self,
        input: J::Input,
    ) -> Result<J::Output, WorkerPoolError> {
        self.dispatch_inner::<J>(input, None).await
    }

    /// Zero-copy dispatch — `transfer` lists `ArrayBuffer`s to
    /// transfer ownership of across the thread boundary instead of
    /// structured-cloning. Critical for large payloads (the
    /// `GltfParseJob` 27 MB robot case).
    pub async fn dispatch_with_transfer<J: WorkerJob>(
        &self,
        input: J::Input,
        transfer: Array,
    ) -> Result<J::Output, WorkerPoolError> {
        self.dispatch_inner::<J>(input, Some(transfer)).await
    }

    async fn dispatch_inner<J: WorkerJob>(
        &self,
        input: J::Input,
        transfer: Option<Array>,
    ) -> Result<J::Output, WorkerPoolError> {
        if !crate::workers::entry::is_registered(J::NAME) {
            return Err(WorkerPoolError::UnknownJob(J::NAME));
        }

        let id = self.next_job_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel::<Result<JsValue, JsValue>>();

        // Pick a worker round-robin. Deliberately *don't* insert into
        // `pending` or bump `jobs_dispatched` yet — both happen after
        // `post_message*` succeeds below. A failed postMessage means
        // the worker never saw the job, so leaving a stale
        // `PendingEntry` in the map would (a) inflate the dispatched
        // counter and (b) leave a sender that nothing can ever
        // resolve (the matching `onerror`'s `worker_idx` filter would
        // still drain it eventually, but with a misleading "worker
        // errored" message rather than the actual postMessage
        // failure the caller is about to get back).
        let worker_idx = {
            let mut cursor = self.next_worker.borrow_mut();
            let idx = *cursor;
            *cursor = (*cursor + 1) % self.workers.len();
            idx
        };
        let worker = &self.workers[worker_idx].worker;

        let input_js = serde_wasm_bindgen::to_value(&input)
            .map_err(|err| WorkerPoolError::Serde(format!("input: {err}")))?;

        let msg = Object::new();
        let _ = Reflect::set(
            &msg,
            &JsValue::from_str("kind"),
            &JsValue::from_str("awsm-job"),
        );
        // Encode the job id as a string rather than a JS Number so
        // the routing key keeps full u64 precision. JS Numbers can
        // only exactly represent integers up to 2^53, so a long-running
        // session would eventually start misrouting results once
        // `next_job_id` clears that threshold. String encoding adds
        // a few bytes per message and is the same shape both sides
        // (main + worker) parse via `.as_string() → u64::from_str`.
        let _ = Reflect::set(
            &msg,
            &JsValue::from_str("id"),
            &JsValue::from_str(&id.to_string()),
        );
        let _ = Reflect::set(
            &msg,
            &JsValue::from_str("name"),
            &JsValue::from_str(J::NAME),
        );
        let _ = Reflect::set(&msg, &JsValue::from_str("input"), &input_js);

        let post_result = match transfer {
            Some(arr) => worker.post_message_with_transfer(&msg, &arr),
            None => worker.post_message(&msg),
        };
        post_result.map_err(|err| {
            // `tx` / `rx` were created locally and never registered —
            // both drop on the early return, no cleanup needed beyond
            // returning the typed error. `jobs_dispatched` was not
            // incremented yet either, so the counter stays honest.
            WorkerPoolError::post_message_from_js("dispatch postMessage", err)
        })?;

        // postMessage succeeded — now (and only now) commit to the
        // pending map and bump the dispatched counter. After this
        // point the worker has the job and will send back either an
        // `awsm-result` / `awsm-error` (consumed by `onmessage`) or
        // its `onerror` filter will drain this entry.
        self.pending.borrow_mut().insert(
            id,
            PendingEntry {
                sender: tx,
                worker_idx,
            },
        );
        self.stats.borrow_mut().jobs_dispatched += 1;

        // Capture the dispatch timestamp now (after the postMessage
        // succeeded) so the telemetry isn't polluted by failed
        // dispatches. The `await` below resolves once the worker's
        // `awsm-result` (or `awsm-error`) routes through `onmessage`
        // and the matching `pending` sender fires.
        let dispatched_at_ms = perf_now_ms();

        match rx.await {
            // `J::from_response_message` is the trait hook for jobs
            // whose worker→main payload carries attached JS handles
            // (e.g. `ImageBitmap`s transferred by GltfParseJob). The
            // default impl is a thin `serde_wasm_bindgen::from_value`
            // wrapper, identical to the prior behaviour for pure-data
            // jobs.
            Ok(Ok(payload)) => {
                if dispatched_at_ms > 0.0 {
                    let delta = (perf_now_ms() - dispatched_at_ms).max(0.0);
                    self.stats.borrow_mut().job_round_trip_ms += delta;
                }
                J::from_response_message(payload).map_err(WorkerPoolError::Serde)
            }
            Ok(Err(err)) => {
                let msg = err.as_string().unwrap_or_else(|| format!("{err:?}"));
                Err(WorkerPoolError::JobFailed(msg))
            }
            Err(_) => Err(WorkerPoolError::ChannelDropped),
        }
    }

    /// Snapshot the pool telemetry.
    pub fn stats(&self) -> WorkerPoolStats {
        *self.stats.borrow()
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        for slot in &self.workers {
            slot.worker.terminate();
        }
    }
}

/// `performance.now()` in milliseconds, routed through the renderer's
/// web-global helper so it works in both `Window` and
/// `DedicatedWorkerGlobalScope` contexts (the pool is constructed on
/// the main thread but the same helper applies regardless). Returns
/// `0.0` if `performance` isn't reachable — callers skip the
/// telemetry write so the counter stays well-defined.
fn perf_now_ms() -> f64 {
    crate::web_global::performance()
        .map(|p| p.now())
        .unwrap_or(0.0)
}

/// Decode the `"id"` field of an `awsm-result` / `awsm-error`
/// message. The wire format is the string repr of a `u64` (see
/// the dispatch site for why string instead of `Number`). Returns
/// `None` for missing / non-string / non-parseable ids — the caller
/// logs and moves on; a malformed id is never routable.
pub(crate) fn parse_job_id(data: &JsValue) -> Option<u64> {
    Reflect::get(data, &JsValue::from_str("id"))
        .ok()
        .and_then(|v| v.as_string())
        .and_then(|s| s.parse::<u64>().ok())
}

fn default_worker_count() -> usize {
    let n = web_sys::window()
        .map(|w| w.navigator().hardware_concurrency() as usize)
        .unwrap_or(4);
    n.clamp(1, 4)
}
