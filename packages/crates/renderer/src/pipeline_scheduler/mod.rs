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
//! Real compile is driven by the single render-driven operation
//! [`crate::AwsmRenderer::ensure_scene_pipelines`] (in
//! [`launch`]): it pushes per-sub-pipeline compile promises onto the
//! [`PipelineScheduler`]'s `inflight_compile` queue, charged to each
//! bucket's material group, and the renderer's `poll_pipeline_scheduler`
//! drains + installs them, flipping each material `Pending → Ready` when
//! its last sub-pipeline resolves.
//!
//! The type surface (`PipelineGroupId`, `PipelineGroupStatus`,
//! `PipelineGroupDef`, `MaterialDef`, `MaterialDefKind`, `PassDef`,
//! `PassKind`, `PipelineConfigSnapshot`, `MaterialId`) lives in
//! [`types`].

#![warn(missing_docs)]

pub mod launch;
pub mod types;

pub use types::*;

mod scheduler;
pub use scheduler::*;
