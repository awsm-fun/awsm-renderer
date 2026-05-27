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

use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::stream::FuturesUnordered;
use futures::{Future, Stream};
use slotmap::SlotMap;

use crate::error::AwsmError;

/// Per-material state stored in the scheduler.
pub struct MaterialState {
    /// Original submission definition (snapshot-pinned).
    pub def: MaterialDef,
    /// Current readiness state.
    pub status: PipelineGroupStatus,
    /// Generation marker — increments each time this material's pipelines
    /// are resubmitted (e.g. after a config flip). Used to discard stale
    /// compile-future resolutions when a newer generation has been
    /// submitted in the meantime.
    pub generation: u32,
}

/// Per-pass state stored in the scheduler.
pub struct PassState {
    /// Original submission definition for this pass group.
    pub def: PassDef,
    /// Current readiness state.
    pub status: PipelineGroupStatus,
    /// Generation marker — bumped on every resubmission so stale compile
    /// resolutions can be dropped silently.
    pub generation: u32,
}

/// Resolution of a single compile future. Carries enough information
/// for the scheduler to find the right slot and decide whether to
/// commit the transition (or drop it as stale).
pub struct CompileResolution {
    /// Id of the group that this resolution applies to.
    pub id: PipelineGroupId,
    /// Generation marker captured when the future was queued; compared
    /// against the slot's current generation to detect staleness.
    pub generation: u32,
    /// Outcome of the compile.
    pub result: Result<(), AwsmError>,
}

type PendingFuture = Pin<Box<dyn Future<Output = CompileResolution> + 'static>>;

/// Install target for a per-pipeline compile resolution (Block D.1
/// PART 2). Carried by [`PipelineCompileResolution`] so
/// `apply_compile_resolution` on the renderer knows which per-pass
/// cache to install the resolved [`web_sys::GpuComputePipeline`] /
/// [`web_sys::GpuRenderPipeline`] into.
///
/// Each variant ties a resolved pipeline back to its identity in the
/// per-pass cache layer (`material_classify.dynamic_pipeline_cache`,
/// `material_opaque.pipelines.per_shader_id`, etc.).
#[derive(Clone)]
pub enum CompileInstallTarget {
    /// Compute: dynamic-material classify pipeline (per dispatch_hash + msaa).
    ClassifyDynamic {
        /// Classify dispatch_hash at submission.
        dispatch_hash: u64,
        /// MSAA sample count (`Some(4)` or `None`).
        msaa: Option<u32>,
    },
    /// Compute: dynamic-material opaque pipeline (per shader_id × msaa × mipmaps).
    OpaqueDynamic {
        /// Shader-id of the dynamic material.
        shader_id: awsm_materials::MaterialShaderId,
        /// MSAA sample count.
        msaa: Option<u32>,
        /// Mipmap-gradient variant on or off.
        mipmaps: bool,
    },
}

/// Resolution of one sub-pipeline within a `PipelineGroupId`'s
/// compile (Block D.1 PART 2). One scheduler material can fan out to
/// multiple sub-pipeline resolutions (classify ×2 MSAA + opaque ×4
/// (msaa × mipmaps) for a Blend dynamic material = 6 sub-pipelines).
/// When the last sub-pipeline lands, the scheduler flips the
/// MaterialId to `Ready`.
pub struct PipelineCompileResolution {
    /// Owning scheduler id.
    pub id: PipelineGroupId,
    /// Generation captured when the compile was kicked off.
    pub generation: u32,
    /// Per-pass install target.
    pub target: CompileInstallTarget,
    /// Original cache key — used by `pipelines.compute.cache.insert`
    /// after the slotmap installs the resolved pipeline.
    pub cache_key: crate::pipelines::compute_pipeline::ComputePipelineCacheKey,
    /// Resolved `GpuComputePipeline` from `create_compute_pipeline_async`
    /// or the JS-side rejection value (carries the shader-compile
    /// diagnostic when the underlying shader fails to compile).
    pub result: std::result::Result<web_sys::GpuComputePipeline, wasm_bindgen::JsValue>,
}

type PipelineCompileFuture =
    Pin<Box<dyn Future<Output = PipelineCompileResolution> + 'static>>;

/// Status-stream event surface for frontends.
#[derive(Debug)]
pub struct StatusEvent {
    /// Id of the group whose status changed.
    pub id: PipelineGroupId,
    /// New status for the group.
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
    /// Legacy inflight queue for whole-batch `CompileResolution`s (the
    /// `(id, generation, Result<(), AwsmError>)` shape). Polled by
    /// [`Self::poll_resolved`] from the render-loop pre-frame phase.
    /// Currently driven by the A.1 bridge via explicit `mark_ready` /
    /// `mark_failed`; no real futures pushed to it.
    inflight: FuturesUnordered<PendingFuture>,
    /// Block D.1 PART 2 inflight: real per-pipeline compile promises
    /// pushed by `AwsmRenderer::launch_dynamic_material_compile`.
    /// Each resolves to a [`PipelineCompileResolution`] carrying the
    /// `GpuComputePipeline` JsValue + install target; the renderer's
    /// `apply_compile_resolution` drives the install at poll time.
    /// `pub(crate)` so `AwsmRenderer::wait_for_pipelines_ready` can
    /// await the next resolution directly via `Stream::next` for the
    /// proper async-yield semantics.
    pub(crate) inflight_compile: FuturesUnordered<PipelineCompileFuture>,
    /// Per-material sub-pipeline countdown (Block D.1 PART 2). Each
    /// time `submit_pipeline_group_batch_async` issues a sub-pipeline
    /// for a material, the count increments. `apply_compile_resolution`
    /// decrements on each successful install; when the count hits 0
    /// the material's `Pending → Ready` transition fires.
    pending_subcompiles: HashMap<MaterialId, u32>,
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
            inflight_compile: FuturesUnordered::new(),
            pending_subcompiles: HashMap::new(),
            events: Vec::new(),
        }
    }

    /// Push a real compile future onto the inflight queue (Block D.1
    /// PART 2). The future will be drained by
    /// [`AwsmRenderer::poll_pipeline_scheduler`] at the next frame's
    /// pre-frame phase. Bumps the per-material sub-compile counter
    /// when the `id` is a `Material` so `apply_compile_resolution`
    /// knows when to mark Ready.
    pub fn push_compile_future(
        &mut self,
        id: PipelineGroupId,
        future: PipelineCompileFuture,
    ) {
        if let PipelineGroupId::Material(mid) = id {
            *self.pending_subcompiles.entry(mid).or_insert(0) += 1;
        }
        self.inflight_compile.push(future);
    }

    /// Drain ONE resolved compile future from `inflight_compile`, if
    /// any is ready. Returns `None` when the queue is empty or the
    /// next future is still pending. Caller (the renderer) consumes
    /// the resolution + does the install.
    pub fn next_compile_resolution(&mut self) -> Option<PipelineCompileResolution> {
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        match Pin::new(&mut self.inflight_compile).poll_next(&mut cx) {
            Poll::Ready(Some(r)) => Some(r),
            _ => None,
        }
    }

    /// Decrement the sub-compile counter for a material; if it hits
    /// zero, transition the material to `Ready` and emit the status
    /// event. Called by `apply_compile_resolution` after each
    /// successful install. Returns `true` when the material has just
    /// flipped to Ready.
    pub fn note_subcompile_complete(&mut self, mid: MaterialId) -> bool {
        let count = self.pending_subcompiles.entry(mid).or_insert(0);
        if *count > 0 {
            *count -= 1;
        }
        if *count == 0 {
            self.pending_subcompiles.remove(&mid);
            // Only mark Ready if the material is still in the scheduler
            // and still Pending (drop_material_group races, stale
            // generations, etc. all silently no-op).
            if let Some(state) = self.materials.get_mut(mid) {
                if state.status.is_pending() {
                    state.status = PipelineGroupStatus::Ready;
                    self.events.push(StatusEvent {
                        id: PipelineGroupId::Material(mid),
                        status: PipelineGroupStatus::Ready,
                    });
                    tracing::info!(
                        target: "awsm_renderer::pipeline_readiness",
                        "subcompile-complete: material({:?}) -> Ready",
                        mid
                    );
                    return true;
                }
            }
        }
        false
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

    /// Iterate every currently-submitted [`MaterialId`] whose
    /// `MaterialDef.config_snapshot` differs from `expected`. Used by
    /// the `set_anti_aliasing` config-flip path (Block D.3): when the
    /// active config drifts, the scheduler iterates here, flips each
    /// stale entry back to `Pending`, drives recompile, then marks
    /// each `Ready` on success.
    pub fn materials_with_stale_snapshot(
        &self,
        expected: &PipelineConfigSnapshot,
    ) -> Vec<MaterialId> {
        self.materials
            .iter()
            .filter(|(_, state)| state.status.is_ready() && state.def.config_snapshot != *expected)
            .map(|(id, _)| id)
            .collect()
    }

    /// Flip a Ready material back to `Pending` (Block D.3 config-flip
    /// reset). Bumps the generation marker so any in-flight stale
    /// compile resolutions get discarded. Updates `config_snapshot`
    /// to the new active config so subsequent status queries see the
    /// right one. No-op if the id doesn't exist or isn't a material.
    pub fn mark_material_pending(
        &mut self,
        id: PipelineGroupId,
        new_snapshot: PipelineConfigSnapshot,
    ) {
        let PipelineGroupId::Material(mid) = id else {
            return;
        };
        let Some(state) = self.materials.get_mut(mid) else {
            return;
        };
        state.status = PipelineGroupStatus::Pending;
        state.generation = state.generation.wrapping_add(1);
        state.def.config_snapshot = new_snapshot;
        self.events.push(StatusEvent {
            id,
            status: PipelineGroupStatus::Pending,
        });
        tracing::info!(
            target: "awsm_renderer::pipeline_readiness",
            "mark_material_pending: material({:?}) -> Pending (config-flip)",
            mid
        );
    }

    /// Returns the current generation marker for a material id, or
    /// `None` if the id isn't in the scheduler. Used by the literal-
    /// push-futures launch path (Block D.1 PART 2) to capture the
    /// generation at submit time so apply_compile_resolution can
    /// detect stale-config resolutions.
    pub fn material_generation(&self, mid: MaterialId) -> Option<u32> {
        self.materials.get(mid).map(|s| s.generation)
    }

    /// Find the [`MaterialId`] in the scheduler whose `MaterialDef`
    /// matches the given `MaterialShaderId`. Returns `None` if no
    /// submitted material has this shader_id.
    ///
    /// Used by the bridge between the legacy `prewarm_pipelines`
    /// compile path and the new scheduler state: after
    /// `prewarm_dynamic_pipelines` finishes compiling pipelines for a
    /// registered material, the renderer calls this to find the
    /// matching scheduler entry and then calls
    /// [`Self::mark_ready`]. O(N) scan over registered materials —
    /// N is small (typically <16 dynamic materials at runtime).
    pub fn find_material_by_shader_id(
        &self,
        shader_id: awsm_materials::MaterialShaderId,
    ) -> Option<MaterialId> {
        for (mid, state) in &self.materials {
            if state.def.shader_id == shader_id {
                return Some(mid);
            }
        }
        None
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
