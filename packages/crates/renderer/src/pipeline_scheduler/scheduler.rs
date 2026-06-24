//! `PipelineScheduler` state machine + compile-resolution plumbing.
//! See the [`crate::pipeline_scheduler`] module docs for the architecture.

use super::types::*;
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
        shader_id: awsm_renderer_materials::MaterialShaderId,
        /// MSAA sample count.
        msaa: Option<u32>,
        /// Mipmap-gradient variant on or off.
        mipmaps: bool,
    },
    /// Compute: unified-edge (U1) per-bucket `cs_shade` pipeline. One per
    /// bucket (incl SKYBOX), keyed `(shader_id, mipmaps)`. Same module as
    /// the bucket's opaque pipeline (`cs_shade` entry), bound to the
    /// shade-extended group(3) layout. Dispatched over the bucket's tile
    /// list when the build-time unified-edge toggle is on.
    EdgeResolveShade {
        /// Shader-id of the bucket whose merged interior+edge shading this
        /// pipeline performs.
        shader_id: awsm_renderer_materials::MaterialShaderId,
        /// Mipmap-gradient variant.
        mipmaps: bool,
    },
    /// Compute: global `final_blend` pipeline (the post-resolve
    /// compositor that reads up-to-4 accumulator slots per edge
    /// pixel + writes the weighted average to `opaque_tex`). Keyed
    /// on `bucket_entries` + the runtime color format.
    EdgeResolveFinalBlend,
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
    /// or the JS-side rejection value.
    pub result: std::result::Result<web_sys::GpuComputePipeline, wasm_bindgen::JsValue>,
    /// On a failed compile, the human-readable WGSL diagnostic pulled from
    /// the shader module's `getCompilationInfo` (line/column + message),
    /// resolved asynchronously alongside the pipeline promise. `None` when
    /// the compile succeeded, when no shader module was captured (edge
    /// pipelines), or when the module reported no error messages — in which
    /// case the apply site falls back to the raw rejection value.
    pub compile_error: Option<String>,
}

type PipelineCompileFuture = Pin<Box<dyn Future<Output = PipelineCompileResolution> + 'static>>;

/// Status-stream event surface for frontends.
#[derive(Debug)]
pub struct StatusEvent {
    /// Id of the group whose status changed.
    pub id: PipelineGroupId,
    /// New status for the group.
    pub status: PipelineGroupStatus,
}

/// Aggregate compile-progress snapshot (the pull-based counterpart to the
/// push [`StatusEvent`] stream). Lets a frontend drive a loading bar /
/// "compiling N materials…" UI without re-deriving counts from the raw
/// event stream. Cheap to compute (a linear scan of the material table);
/// call it once per frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompileProgress {
    /// Submitted materials still compiling (`status == Pending`).
    pub materials_pending: usize,
    /// Materials whose full pipeline set has resolved (`status == Ready`).
    pub materials_ready: usize,
    /// Materials whose compile failed (`status == Failed`).
    pub materials_failed: usize,
    /// Total in-flight sub-pipeline compiles summed across pending
    /// materials — the granular "N pipelines left" number behind a
    /// progress bar (each material fans out to several opaque / classify
    /// / edge sub-pipelines).
    pub in_flight_subcompiles: u32,
}

impl CompileProgress {
    /// True when nothing is compiling: no pending materials and no
    /// in-flight sub-pipeline compiles. Every submitted material is then
    /// `Ready` or `Failed`.
    pub fn is_idle(&self) -> bool {
        self.materials_pending == 0 && self.in_flight_subcompiles == 0
    }

    /// Total materials the scheduler knows about (pending + ready + failed).
    pub fn materials_total(&self) -> usize {
        self.materials_pending + self.materials_ready + self.materials_failed
    }
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
    /// Inflight: real per-pipeline compile promises pushed by
    /// `AwsmRenderer::ensure_scene_pipelines` / `launch_edge_resolve_compile`.
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
    /// **Cross-call in-flight waiter map** (compute pipelines).
    ///
    /// The launch path (`ensure_scene_pipelines` via
    /// `ensure_bucket_pipelines`, plus `launch_edge_resolve_compile`)
    /// installs resolved pipelines
    /// into the SAME shared `ComputePipelines.cache`. When two
    /// launches in the same outer loop want the same cache key
    /// (e.g. the classify variant — keyed on
    /// `(msaa, bucket_entries, emit_edge_data)`, NOT shader_id — or
    /// any of the edge-chain variants that iterate over
    /// `bucket_entries`), the first launch issues
    /// `createComputePipelineAsync`, and the second launch's
    /// `cache_lookup` misses (the cache only gets populated at
    /// resolution time, not at promise-issuance time) → it issues a
    /// duplicate promise.
    ///
    /// **Waiter tracking**: when a later launch skips a cache key
    /// because it's already in flight, we append the later
    /// material's id to the waiter list AND bump its
    /// `pending_subcompiles` counter — that way the late material's
    /// Ready transition still waits on the shared compile resolving.
    /// When the promise resolves, every waiter material's counter
    /// decrements via `note_subcompile_complete`. Otherwise the late
    /// materials could fire Ready while
    /// `render_edge_resolve` is still warn-skipping their bucket.
    ///
    /// Tracks compute pipelines only — render pipelines don't have
    /// the same cross-call pattern today (per-mesh batches go through
    /// `set_render_pipeline_keys_batched`'s within-batch dedup).
    inflight_compute_cache_waiters:
        HashMap<crate::pipelines::compute_pipeline::ComputePipelineCacheKey, Vec<MaterialId>>,
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
            inflight_compute_cache_waiters: HashMap::new(),
            events: Vec::new(),
        }
    }

    /// Register material `mid` as a waiter for the compute pipeline
    /// compile of `cache_key`. Bumps `mid`'s `pending_subcompiles`
    /// counter so its Ready transition waits on this compile.
    ///
    /// Returns `true` if this is the FIRST waiter (caller should
    /// push a new compile promise into `inflight_compile`).
    /// Returns `false` if another launch already pushed the promise
    /// (caller skips the duplicate push; the existing promise's
    /// resolution will install for every waiter via
    /// `take_compute_compile_waiters`).
    ///
    /// Either way, `mid`'s subcompile counter is incremented — the
    /// resolution path decrements it via `note_subcompile_complete`
    /// for every waiter, which is what blocks the late material's
    /// Ready transition until the shared compile lands.
    pub fn register_compute_compile_waiter(
        &mut self,
        cache_key: crate::pipelines::compute_pipeline::ComputePipelineCacheKey,
        mid: MaterialId,
    ) -> bool {
        *self.pending_subcompiles.entry(mid).or_insert(0) += 1;
        let was_first = !self.inflight_compute_cache_waiters.contains_key(&cache_key);
        self.inflight_compute_cache_waiters
            .entry(cache_key)
            .or_default()
            .push(mid);
        was_first
    }

    /// Remove and return every material that registered as a waiter
    /// for `cache_key`. Called from the install path
    /// (`apply_compile_resolution_inline`) on every compile
    /// resolution (success OR failure) — for each waiter, the caller
    /// invokes `note_subcompile_complete` to decrement the counter
    /// and fire the Ready transition when the count hits zero.
    ///
    /// After the take, a fresh launch for the same cache_key (e.g.
    /// a later relaunch loop) will be treated as a NEW first waiter
    /// — by then the cache should be populated and the launch site's
    /// `cache_lookup` path will short-circuit before reaching this
    /// API.
    pub fn take_compute_compile_waiters(
        &mut self,
        cache_key: &crate::pipelines::compute_pipeline::ComputePipelineCacheKey,
    ) -> Vec<MaterialId> {
        self.inflight_compute_cache_waiters
            .remove(cache_key)
            .unwrap_or_default()
    }

    /// Returns `true` if there is at least one waiter for
    /// `cache_key`. Used by launch sites that want to combine the
    /// cache-hit check plus in-flight check into a single decision.
    pub fn has_compute_compile_waiter(
        &self,
        cache_key: &crate::pipelines::compute_pipeline::ComputePipelineCacheKey,
    ) -> bool {
        self.inflight_compute_cache_waiters.contains_key(cache_key)
    }

    /// Push a raw compile future onto the inflight queue WITHOUT
    /// bumping any material's pending-subcompile counter.
    ///
    /// Used by launch sites paired with
    /// `register_compute_compile_waiter` — the latter takes care of
    /// the per-waiter counter bumps (including the first waiter),
    /// so the push step only needs to drive the future onto the
    /// FuturesUnordered queue.
    pub(crate) fn push_compile_future_no_count(&mut self, future: PipelineCompileFuture) {
        self.inflight_compile.push(future);
    }

    /// Push a real compile future onto the inflight queue (Block D.1
    /// PART 2). The future will be drained by
    /// `AwsmRenderer::poll_pipeline_scheduler` at the next frame's
    /// pre-frame phase. Bumps the per-material sub-compile counter
    /// when the `id` is a `Material` so `apply_compile_resolution`
    /// knows when to mark Ready.
    pub fn push_compile_future(&mut self, id: PipelineGroupId, future: PipelineCompileFuture) {
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

    /// Aggregate compile-progress snapshot over the material table —
    /// material status counts plus the total in-flight sub-pipeline
    /// compiles. See [`CompileProgress`].
    pub fn compile_progress(&self) -> CompileProgress {
        let mut progress = CompileProgress::default();
        for state in self.materials.values() {
            match &state.status {
                PipelineGroupStatus::Pending => progress.materials_pending += 1,
                PipelineGroupStatus::Ready => progress.materials_ready += 1,
                PipelineGroupStatus::Failed { .. } => progress.materials_failed += 1,
            }
        }
        progress.in_flight_subcompiles = self.pending_subcompiles.values().copied().sum();
        progress
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

    /// Returns the current generation marker for a material id, or
    /// `None` if the id isn't in the scheduler. Used by the literal-
    /// push-futures launch path (Block D.1 PART 2) to capture the
    /// generation at submit time so apply_compile_resolution can
    /// detect stale-config resolutions.
    pub fn material_generation(&self, mid: MaterialId) -> Option<u32> {
        self.materials.get(mid).map(|s| s.generation)
    }

    /// Transition material `mid` to `Pending` and bump its generation,
    /// preserving the existing `config_snapshot`. Used by the
    /// bucket-grow + texture-pool-grow relaunch paths
    /// ([`crate::AwsmRenderer::register_material`] +
    /// [`crate::AwsmRenderer::finalize_gpu_textures`]) which need to
    /// invalidate the in-flight pipeline state for every registered
    /// material — those events don't change the renderer-wide config
    /// snapshot (no AA / mipmap flip), but they DO invalidate every
    /// previously-compiled pipeline whose cache key embeds
    /// `bucket_entries` or `texture_pool_arrays_len`.
    ///
    /// Generation bump is what makes the existing apply_compile_resolution
    /// stale-generation gate discard old in-flight resolutions
    /// (compiled against the previous bucket / pool shape) instead of
    /// letting them install into the freshly-cleared typed cache.
    ///
    /// No-op if `id` isn't a Material or the material isn't tracked.
    /// Idempotent on already-Pending materials (still bumps generation
    /// + emits a Pending status event).
    pub fn mark_material_pending_for_relaunch(&mut self, id: PipelineGroupId) {
        let PipelineGroupId::Material(mid) = id else {
            return;
        };
        let Some(state) = self.materials.get_mut(mid) else {
            return;
        };
        state.status = PipelineGroupStatus::Pending;
        state.generation = state.generation.wrapping_add(1);
        // config_snapshot intentionally NOT updated — this transition
        // is driven by bucket / texture-pool layout changes, not a
        // config flip, so the snapshot stays accurate.
        self.events.push(StatusEvent {
            id,
            status: PipelineGroupStatus::Pending,
        });
        tracing::info!(
            target: "awsm_renderer::pipeline_readiness",
            "mark_material_pending_for_relaunch: material({:?}) -> Pending (bucket/pool relaunch)",
            mid
        );
    }

    /// Returns the number of in-flight sub-pipeline compiles charged
    /// to this material's group. Used by the launch path to decide
    /// whether to call [`Self::mark_ready`] inline (count == 0, all
    /// cache hits) vs. defer Ready until the last sub-pipeline
    /// resolves via [`Self::note_subcompile_complete`].
    pub fn pending_subcompile_count(&self, mid: MaterialId) -> u32 {
        self.pending_subcompiles.get(&mid).copied().unwrap_or(0)
    }

    /// Find the [`MaterialId`] in the scheduler whose `MaterialDef`
    /// matches the given `MaterialShaderId`. Returns `None` if no
    /// submitted material has this shader_id.
    ///
    /// Used by `ensure_scene_pipelines` / `ensure_bucket_pipelines` to
    /// find the scheduler group a bucket's compile should be charged to,
    /// and by `dynamic_material_compile_status` to look up a material's
    /// status by shader id. O(N) scan over registered materials — N is
    /// small (typically <16 dynamic materials at runtime).
    pub fn find_material_by_shader_id(
        &self,
        shader_id: awsm_renderer_materials::MaterialShaderId,
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
/// `https://github.com/dakom/awsm-renderer/pull/99`.
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
