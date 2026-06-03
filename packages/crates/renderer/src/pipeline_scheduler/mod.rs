//! Pipeline-readiness scheduler.
//!
//! Wraps the existing `Shaders::ensure_keys` +
//! `{Render,Compute}Pipelines::ensure_keys` batch primitives into a
//! unified async readiness state machine. Per the architecture in
//! [`https://github.com/dakom/awsm-renderer/pull/99`](../../../https://github.com/dakom/awsm-renderer/pull/99):
//!
//! - Public API: `submit_pipeline_group_batch(defs) -> Vec<PipelineGroupId>` —
//!   returns immediately with handles in `Pending` state. Compiles are queued
//!   in a main-thread `FuturesUnordered`; transitions surface on the status
//!   stream.
//! - Hot path: never asks for status. Pass dispatch sites consult typed
//!   `Option<PipelineKey>` accessors (None → warn + skip via the render-frame
//!   preamble); materials are filtered out of `bucket_entries_cached()` until
//!   their group transitions Ready.
//! - Config flips (`set_anti_aliasing`, `set_post_processing`): every
//!   currently-Ready material with a stale `PipelineConfigSnapshot`
//!   transitions back to Pending; pass-level groups for the new config are
//!   submitted in the same batch.
//!
//! **Stage 1 status (commit history of this file):**
//!
//! - The type surface (`PipelineGroupId`, `PipelineGroupStatus`,
//!   `PipelineGroupDef`, `MaterialDef`, `MaterialDefKind`, `PassDef`,
//!   `PassKind`, `PipelineConfigSnapshot`, `MaterialId`) is complete in
//!   [`types`].
//! - The [`PipelineScheduler`] struct holds the slot maps and the
//!   `FuturesUnordered`. Real compile is driven via the Block A.1
//!   bridge inside `prewarm_dynamic_pipelines` (in
//!   `crates/renderer/src/lib.rs`): each scheduler entry is marked
//!   `Ready` once the existing batched compile path resolves. The
//!   literal "push real compile futures onto `inflight`" direction is
//!   preserved as a Block D follow-up.

#![warn(missing_docs)]

pub mod launch;
pub mod types;

pub use types::*;

mod scheduler;
pub use scheduler::*;
