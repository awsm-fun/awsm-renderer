//! Pipeline-readiness scheduler.
//!
//! Wraps the existing `Shaders::ensure_keys` +
//! `{Render,Compute}Pipelines::ensure_keys` batch primitives into a
//! unified async readiness state machine. Per the architecture in
//! [`docs/plans/more-optimizations.md`](../../../docs/plans/more-optimizations.md):
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
//!   `FuturesUnordered`; `submit_pipeline_group_batch` is implemented
//!   skeleton-only — it allocates ids and emits Pending status, but the
//!   compile futures it builds are `async { Ok(()) }` stubs. **Wiring each
//!   `PipelineGroupDef` variant to the actual compile path
//!   (`Shaders::ensure_keys` + `{Render,Compute}Pipelines::ensure_keys`)
//!   is the next step** (Stage 1 follow-up commits). Integration with
//!   `AwsmRendererBuilder::build` and the call-site migrations are
//!   downstream of that.

pub mod types;

pub use types::*;

use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::stream::FuturesUnordered;
use futures::{Future, Stream};
use slotmap::SlotMap;

use crate::error::AwsmError;

/// Per-material state stored in the scheduler.
pub struct MaterialState {
    pub def: MaterialDef,
    pub status: PipelineGroupStatus,
    /// Generation marker — increments each time this material's pipelines
    /// are resubmitted (e.g. after a config flip). Used to discard stale
    /// compile-future resolutions when a newer generation has been
    /// submitted in the meantime.
    pub generation: u32,
}

/// Per-pass state stored in the scheduler.
pub struct PassState {
    pub def: PassDef,
    pub status: PipelineGroupStatus,
    pub generation: u32,
}

/// Resolution of a single compile future. Carries enough information
/// for the scheduler to find the right slot and decide whether to
/// commit the transition (or drop it as stale).
pub struct CompileResolution {
    pub id: PipelineGroupId,
    pub generation: u32,
    pub result: Result<(), AwsmError>,
}

type PendingFuture = Pin<Box<dyn Future<Output = CompileResolution> + 'static>>;

/// Status-stream event surface for frontends.
#[derive(Debug)]
pub struct StatusEvent {
    pub id: PipelineGroupId,
    pub status: PipelineGroupStatus,
}

/// Pipeline-readiness scheduler.
///
/// Owns:
/// - `materials`: SlotMap of all material groups (Pending / Ready /
///   Failed). Removed via `drop_material_group`.
/// - `passes`: map from `PassKind` → state. Pass groups are singletons;
///   resubmission overwrites the previous state in place (the inner
///   generation marker discriminates stale-vs-fresh compile resolutions).
/// - `inflight`: `FuturesUnordered<PendingFuture>` driving compile
///   futures concurrently. Polled by `poll_resolved` from the render
///   loop's pre-frame phase.
/// - `events`: queue of `StatusEvent`s awaiting drain by subscribers.
///
/// Skeleton-only at this commit — `submit_pipeline_group_batch`
/// allocates ids and emits Pending, but the futures it queues are
/// `async { Ok(()) }` stubs. Future commits wire each `PipelineGroupDef`
/// variant to the actual compile path.
pub struct PipelineScheduler {
    materials: SlotMap<MaterialId, MaterialState>,
    passes: HashMap<PassKind, PassState>,
    inflight: FuturesUnordered<PendingFuture>,
    events: Vec<StatusEvent>,
}

impl Default for PipelineScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl PipelineScheduler {
    /// Creates an empty scheduler.
    pub fn new() -> Self {
        Self {
            materials: SlotMap::with_key(),
            passes: HashMap::new(),
            inflight: FuturesUnordered::new(),
            events: Vec::new(),
        }
    }

    /// Submit a batch of pipeline groups. Returns ids in the same order as
    /// the input. Materials get fresh SlotMap keys; passes use their
    /// `PassKind` (resubmission overwrites the existing entry's state and
    /// bumps the generation marker).
    ///
    /// **Compile-binding model**: submitted groups start in `Pending`.
    /// They transition to `Ready` / `Failed` via explicit
    /// [`Self::mark_ready`] / [`Self::mark_failed`] calls from the
    /// caller that actually drives compile (today: the existing
    /// `prewarm_pipelines` path; Stage 1.8 fully: a scheduler-internal
    /// driver). The scheduler does **not** auto-resolve groups — that
    /// would lie about readiness state. Pending groups stay Pending
    /// indefinitely until explicitly marked.
    pub fn submit_pipeline_group_batch(
        &mut self,
        defs: Vec<PipelineGroupDef>,
    ) -> Vec<PipelineGroupId> {
        let mut ids = Vec::with_capacity(defs.len());

        for def in defs {
            let id = match def {
                PipelineGroupDef::Material(mdef) => {
                    let mat_id = self.materials.insert(MaterialState {
                        def: mdef,
                        status: PipelineGroupStatus::Pending,
                        generation: 0,
                    });
                    let id = PipelineGroupId::Material(mat_id);
                    self.events.push(StatusEvent {
                        id,
                        status: PipelineGroupStatus::Pending,
                    });
                    id
                }
                PipelineGroupDef::Pass(pdef) => {
                    let kind = pdef.kind();
                    let generation = self
                        .passes
                        .get(&kind)
                        .map(|s| s.generation.wrapping_add(1))
                        .unwrap_or(0);
                    self.passes.insert(
                        kind,
                        PassState {
                            def: pdef,
                            status: PipelineGroupStatus::Pending,
                            generation,
                        },
                    );
                    let id = PipelineGroupId::Pass(kind);
                    self.events.push(StatusEvent {
                        id,
                        status: PipelineGroupStatus::Pending,
                    });
                    id
                }
            };
            ids.push(id);
        }

        tracing::info!(
            target: "awsm_renderer::pipeline_readiness",
            "submit_pipeline_group_batch: {} groups submitted",
            ids.len()
        );

        ids
    }

    /// Mark a pipeline group as `Ready`. Called by the path that
    /// actually drives compile (today: legacy `prewarm_pipelines`;
    /// Stage 1.8 fully: scheduler-internal driver). No-op if the id
    /// doesn't exist or is already `Ready`. Emits a status event.
    pub fn mark_ready(&mut self, id: PipelineGroupId) {
        let label;
        match id {
            PipelineGroupId::Material(mid) => {
                let Some(state) = self.materials.get_mut(mid) else {
                    return;
                };
                if state.status.is_ready() {
                    return;
                }
                state.status = PipelineGroupStatus::Ready;
                label = format!("material:{:?}", mid);
            }
            PipelineGroupId::Pass(kind) => {
                let Some(state) = self.passes.get_mut(&kind) else {
                    return;
                };
                if state.status.is_ready() {
                    return;
                }
                state.status = PipelineGroupStatus::Ready;
                label = format!("pass:{:?}", kind);
            }
        }
        self.events.push(StatusEvent {
            id,
            status: PipelineGroupStatus::Ready,
        });
        tracing::info!(
            target: "awsm_renderer::pipeline_readiness",
            "mark_ready: {} -> Ready",
            label
        );
    }

    /// Mark a pipeline group as `Failed`. Same contract as
    /// [`Self::mark_ready`]. Emits a status event with the
    /// `PipelineVariantNotCompiled` placeholder error — consumers
    /// query [`Self::pipeline_group_status`] for the full error.
    pub fn mark_failed(&mut self, id: PipelineGroupId, error: AwsmError) {
        let label;
        match id {
            PipelineGroupId::Material(mid) => {
                let Some(state) = self.materials.get_mut(mid) else {
                    return;
                };
                state.status = PipelineGroupStatus::Failed { error };
                label = format!("material:{:?}", mid);
            }
            PipelineGroupId::Pass(kind) => {
                let Some(state) = self.passes.get_mut(&kind) else {
                    return;
                };
                state.status = PipelineGroupStatus::Failed { error };
                label = format!("pass:{:?}", kind);
            }
        }
        self.events.push(StatusEvent {
            id,
            status: PipelineGroupStatus::Failed {
                error: AwsmError::PipelineVariantNotCompiled("see scheduler state"),
            },
        });
        tracing::warn!(
            target: "awsm_renderer::pipeline_readiness",
            "mark_failed: {} -> Failed",
            label
        );
    }

    /// Per-group status query — O(1) lookup. Returns `None` if the id
    /// doesn't exist in the scheduler (dropped or never submitted).
    pub fn pipeline_group_status(&self, id: PipelineGroupId) -> Option<&PipelineGroupStatus> {
        match id {
            PipelineGroupId::Material(mid) => self.materials.get(mid).map(|s| &s.status),
            PipelineGroupId::Pass(kind) => self.passes.get(&kind).map(|s| &s.status),
        }
    }

    /// Drain pending status events. Frontends call this from their
    /// per-frame poll loop (or via a subscription wrapper that converts
    /// the drain into a stream of events). Each event represents a
    /// single Pending↔Ready/Failed transition.
    pub fn drain_status_events(&mut self) -> Vec<StatusEvent> {
        std::mem::take(&mut self.events)
    }

    /// Drop a material group. Used by the editor's hot-reload cleanup
    /// per the generation-marker-per-slot pattern documented in the
    /// plan. No-op if the id isn't in the map.
    pub fn drop_material_group(&mut self, id: MaterialId) {
        self.materials.remove(id);
        // Note: in-flight compile futures for this id will still
        // resolve and try to commit their result. The commit path
        // checks the generation marker (and material existence) and
        // discards stale resolutions silently.
    }

    /// Poll the in-flight `FuturesUnordered` for resolved compiles,
    /// applying their transitions to the material/pass state and
    /// emitting status events. Called from the render loop's pre-frame
    /// phase.
    ///
    /// Returns the number of transitions applied this poll (useful for
    /// the boot-timing logs).
    ///
    /// **Today**: the scheduler doesn't currently push futures on
    /// submit — readiness is signalled via explicit
    /// [`Self::mark_ready`] / [`Self::mark_failed`] from the path that
    /// drives compile. This method drains any futures that **are**
    /// pushed (Stage 1.8 fully will start pushing real compile futures
    /// here); for now it's almost always a no-op.
    pub fn poll_resolved(&mut self) -> usize {
        let mut applied = 0;
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);

        while let Poll::Ready(Some(resolution)) = Pin::new(&mut self.inflight).poll_next(&mut cx) {
            self.apply_resolution(resolution);
            applied += 1;
        }

        applied
    }

    /// Apply a single resolved compile to scheduler state. Stale
    /// resolutions (generation mismatch or removed id) are dropped.
    fn apply_resolution(&mut self, r: CompileResolution) {
        let new_status = match r.result {
            Ok(()) => PipelineGroupStatus::Ready,
            Err(e) => PipelineGroupStatus::Failed { error: e },
        };

        match r.id {
            PipelineGroupId::Material(mid) => {
                let Some(state) = self.materials.get_mut(mid) else {
                    return;
                };
                if state.generation != r.generation {
                    return;
                }
                let label = format!("{:?}", mid);
                state.status = match &new_status {
                    PipelineGroupStatus::Ready => PipelineGroupStatus::Ready,
                    PipelineGroupStatus::Failed { error: _ } => match new_status {
                        PipelineGroupStatus::Failed { error } => {
                            PipelineGroupStatus::Failed { error }
                        }
                        _ => unreachable!(),
                    },
                    PipelineGroupStatus::Pending => PipelineGroupStatus::Pending,
                };
                tracing::info!(
                    target: "awsm_renderer::pipeline_readiness",
                    "transition: material {} -> {}",
                    label,
                    status_label(&state.status),
                );
            }
            PipelineGroupId::Pass(kind) => {
                let Some(state) = self.passes.get_mut(&kind) else {
                    return;
                };
                if state.generation != r.generation {
                    return;
                }
                let label = format!("{:?}", kind);
                state.status = new_status;
                tracing::info!(
                    target: "awsm_renderer::pipeline_readiness",
                    "transition: pass {} -> {}",
                    label,
                    status_label(&state.status),
                );
            }
        }

        // Emit the post-commit status as a stream event.
        let final_status = match r.id {
            PipelineGroupId::Material(mid) => self.materials.get(mid).map(|s| match &s.status {
                PipelineGroupStatus::Pending => StatusEvent {
                    id: r.id,
                    status: PipelineGroupStatus::Pending,
                },
                PipelineGroupStatus::Ready => StatusEvent {
                    id: r.id,
                    status: PipelineGroupStatus::Ready,
                },
                PipelineGroupStatus::Failed { error: _ } => StatusEvent {
                    id: r.id,
                    // Can't clone AwsmError; consumers should query
                    // pipeline_group_status for the full error.
                    status: PipelineGroupStatus::Failed {
                        error: AwsmError::PipelineVariantNotCompiled("see scheduler state"),
                    },
                },
            }),
            PipelineGroupId::Pass(kind) => self.passes.get(&kind).map(|s| match &s.status {
                PipelineGroupStatus::Pending => StatusEvent {
                    id: r.id,
                    status: PipelineGroupStatus::Pending,
                },
                PipelineGroupStatus::Ready => StatusEvent {
                    id: r.id,
                    status: PipelineGroupStatus::Ready,
                },
                PipelineGroupStatus::Failed { error: _ } => StatusEvent {
                    id: r.id,
                    status: PipelineGroupStatus::Failed {
                        error: AwsmError::PipelineVariantNotCompiled("see scheduler state"),
                    },
                },
            }),
        };
        if let Some(ev) = final_status {
            self.events.push(ev);
        }
    }
}

fn status_label(s: &PipelineGroupStatus) -> &'static str {
    match s {
        PipelineGroupStatus::Pending => "Pending",
        PipelineGroupStatus::Ready => "Ready",
        PipelineGroupStatus::Failed { .. } => "Failed",
    }
}

// ─────────────────────────────────────────────────────────────────
// Render-frame warn-and-skip safety net
// ─────────────────────────────────────────────────────────────────

use std::sync::Mutex;

/// Once-per-session warn-skip log helper.
///
/// Render-frame dispatch sites that find a pipeline variant not yet
/// compiled (None from their typed `Option<PipelineKey>` accessor)
/// call this helper to surface a `tracing::warn!` exactly once per
/// `(location, identifier)` pair per session, then `return` from the
/// dispatch. Per [§ Render-frame preamble safety net] in
/// `docs/plans/more-optimizations.md`.
///
/// `location` is a stable string like `"opaque_pass"` or
/// `"shadow_gen"`; `id` is whichever identifier disambiguates the
/// missing variant within that location (e.g. a shader_id formatted
/// as `"{:?}"`).
pub fn warn_pipeline_not_compiled(location: &'static str, id: &str) {
    static SEEN: Mutex<Option<std::collections::HashSet<(&'static str, String)>>> =
        Mutex::new(None);

    let mut guard = SEEN.lock().unwrap_or_else(|p| p.into_inner());
    let set = guard.get_or_insert_with(std::collections::HashSet::new);
    let key = (location, id.to_string());
    if set.insert(key.clone()) {
        tracing::warn!(
            target: "awsm_renderer::pipeline_readiness",
            "render-frame preamble: pipeline not compiled at {} (id={}) — skipping. \
             First occurrence — subsequent occurrences for this (location, id) are \
             suppressed for the rest of the session.",
            location,
            id,
        );
    }
}

/// Placeholder helper kept for future Stage 1.8-fully integration:
/// the scheduler will eventually push real compile futures via
/// `submit_pipeline_group_batch`'s internal driver, at which point
/// `poll_resolved` does the actual transition work. Today the driver
/// is external (callers explicitly invoke `mark_ready` /
/// `mark_failed`) and this builder is unused — kept here as a
/// reference for the future shape.
#[allow(dead_code)]
fn stub_compile_future(id: PipelineGroupId, generation: u32) -> PendingFuture {
    Box::pin(async move {
        CompileResolution {
            id,
            generation,
            result: Ok(()),
        }
    })
}
