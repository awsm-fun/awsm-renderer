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
    fn from_js(prefix: &'static str, err: JsValue) -> Self {
        let msg = err
            .as_string()
            .or_else(|| {
                Reflect::get(&err, &JsValue::from_str("message"))
                    .ok()
                    .and_then(|v| v.as_string())
            })
            .unwrap_or_else(|| format!("{err:?}"));
        WorkerPoolError::Bootstrap(format!("{prefix}: {msg}"))
    }
}

/// A CPU-only job runnable on a [`WorkerPool`] worker.
///
/// Stateless by design — implementations only act on `input`. State
/// the job needs (e.g. a shared cache) goes inside the input.
pub trait WorkerJob: 'static {
    /// Unique string identifier; used in the postMessage dispatch.
    const NAME: &'static str;

    type Input: Serialize + DeserializeOwned;
    type Output: Serialize + DeserializeOwned;

    /// Runs on the worker thread.
    fn execute(input: Self::Input) -> Self::Output;
}

/// Bundle-URL discovery strategy. Default is `Auto` (read
/// `import.meta.url` from the wasm-bindgen JS glue at runtime).
pub enum WorkerPoolBootstrap {
    /// Auto-discover via `import.meta.url`. Works for any
    /// wasm-bindgen `--target web` consumer regardless of bundler.
    Auto,
    /// Explicit bundle URL — for the rare consumer whose build setup
    /// doesn't expose `import.meta.url`.
    ModuleUrl { bundle_url: String },
    /// Escape hatch — consumer constructs the `Worker` themselves;
    /// the pool drives the postMessage protocol on the handle.
    Custom(Box<dyn Fn() -> Result<Worker, JsValue> + 'static>),
}

impl Default for WorkerPoolBootstrap {
    fn default() -> Self {
        WorkerPoolBootstrap::Auto
    }
}

/// Telemetry counters.
#[derive(Debug, Default, Clone, Copy)]
pub struct WorkerPoolStats {
    pub workers_alive: usize,
    pub jobs_dispatched: u64,
    pub jobs_completed: u64,
    pub jobs_failed: u64,
    /// Total wall-clock time jobs spent queued before a worker
    /// picked them up. (Tracked at dispatch site; the simple
    /// round-robin scheduler picks a worker synchronously, so this
    /// is the cross-thread postMessage latency rather than real
    /// queue wait. Kept for future load-balancing instrumentation.)
    pub queue_wait_ms: f64,
}

/// Pending job sender — keyed by `JobId` in the pool's pending map.
type PendingSender = oneshot::Sender<Result<JsValue, JsValue>>;

/// Pool of N workers sharing the consumer's compiled
/// `WebAssembly.Module`.
pub struct WorkerPool {
    workers: Vec<WorkerSlot>,
    next_worker: RefCell<usize>,
    next_job_id: AtomicU64,
    pending: Rc<RefCell<HashMap<u64, PendingSender>>>,
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
            .map_err(|err| WorkerPoolError::from_js("current_wasm_module", err))?;

        let pending: Rc<RefCell<HashMap<u64, PendingSender>>> =
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
                        .map_err(|err| WorkerPoolError::from_js("worker spawn", err))?
                }
                WorkerPoolBootstrap::Custom(factory) => factory()
                    .map_err(|err| WorkerPoolError::from_js("custom worker factory", err))?,
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
                        let id = Reflect::get(&data, &JsValue::from_str("id"))
                            .ok()
                            .and_then(|v| v.as_f64())
                            .map(|f| f as u64);
                        let payload = Reflect::get(&data, &JsValue::from_str("payload"))
                            .unwrap_or(JsValue::UNDEFINED);
                        if let Some(id) = id {
                            if let Some(sender) = pending_clone.borrow_mut().remove(&id) {
                                let _ = sender.send(Ok(payload));
                                stats_clone.borrow_mut().jobs_completed += 1;
                            }
                        }
                    }
                    "awsm-error" => {
                        let id = Reflect::get(&data, &JsValue::from_str("id"))
                            .ok()
                            .and_then(|v| v.as_f64())
                            .map(|f| f as u64);
                        let msg = Reflect::get(&data, &JsValue::from_str("message"))
                            .ok()
                            .and_then(|v| v.as_string())
                            .unwrap_or_else(|| "unknown job error".to_string());
                        if let Some(id) = id {
                            if let Some(sender) = pending_clone.borrow_mut().remove(&id) {
                                let _ = sender.send(Err(JsValue::from_str(&msg)));
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
            let onerror = Closure::<dyn FnMut(JsValue)>::new(move |err: JsValue| {
                tracing::warn!("{onerror_label}: worker onerror: {err:?}");
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
            .map_err(|err| WorkerPoolError::from_js("init msg", err))?;
            Reflect::set(&init_msg, &JsValue::from_str("wasm_module"), &wasm_module)
                .map_err(|err| WorkerPoolError::from_js("init msg", err))?;
            Reflect::set(
                &init_msg,
                &JsValue::from_str("glue_url"),
                &JsValue::from_str(&glue_url),
            )
            .map_err(|err| WorkerPoolError::from_js("init msg", err))?;
            worker
                .post_message(&init_msg)
                .map_err(|err| WorkerPoolError::from_js("init postMessage", err))?;

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
        self.pending.borrow_mut().insert(id, tx);
        self.stats.borrow_mut().jobs_dispatched += 1;

        // Pick a worker round-robin.
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
        let _ = Reflect::set(
            &msg,
            &JsValue::from_str("id"),
            &JsValue::from_f64(id as f64),
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
        post_result.map_err(|err| WorkerPoolError::from_js("dispatch postMessage", err))?;

        match rx.await {
            Ok(Ok(payload)) => serde_wasm_bindgen::from_value(payload)
                .map_err(|err| WorkerPoolError::Serde(format!("output: {err}"))),
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

fn default_worker_count() -> usize {
    let n = web_sys::window()
        .map(|w| w.navigator().hardware_concurrency() as usize)
        .unwrap_or(4);
    n.clamp(1, 4)
}
