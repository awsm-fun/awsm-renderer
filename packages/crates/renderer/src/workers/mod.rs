//! Library-wide worker-job infrastructure.
//!
//! Public surface:
//!
//! - [`WorkerPool`] — N background workers sharing the consumer's
//!   compiled `WebAssembly.Module`.
//! - [`WorkerJob`] — trait for stateless, serializable CPU jobs.
//! - [`WorkerPoolBootstrap`] — bundle-URL discovery strategy
//!   (`Auto` by default — `import.meta.url` from an inline-JS shim
//!   that wasm-bindgen embeds into the consumer's glue).
//! - [`awsm_worker_entry`] — wasm export the worker-side shim calls
//!   after initialising the shared module; installs the dispatch
//!   listener.
//!
//! **Scope**: CPU-only work that produces `Vec<u8>` / parsed
//! structures, ingested by the main thread via
//! [`crate::buffer::mapped_uploader::MappedUploader::ingest_foreign`].
//! The WebGPU device cannot be shared across workers (the
//! `OffscreenCanvas` mode instead runs the entire renderer in a worker).

mod blob;
mod entry;
mod pool;

pub use entry::{awsm_worker_entry, register_job, EchoInput, EchoJob, EchoOutput};
pub use pool::{WorkerJob, WorkerPool, WorkerPoolBootstrap, WorkerPoolError, WorkerPoolStats};
