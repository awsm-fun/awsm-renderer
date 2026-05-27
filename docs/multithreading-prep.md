# Multithreading prep — `pipeline_scheduler` boundaries

Audit notes for Block E.2 of [`docs/plans/more-optimizations.md`](plans/more-optimizations.md). Captures the existing single-threaded invariants in the `pipeline_scheduler` module so a future wasm32-multithread migration knows exactly what to revisit.

## Today's threading model

The renderer runs on wasm32-unknown-unknown's single-threaded JS event loop. No `Send` / `Sync` guarantees are needed because nothing crosses a thread boundary. `wasm-bindgen-futures::spawn_local` queues onto the same microtask queue that the RAF tick uses.

Consequences `pipeline_scheduler` relies on:

- `FuturesUnordered<PendingFuture>` where `PendingFuture = Pin<Box<dyn Future<Output = CompileResolution> + 'static>>` — **not `Send`**. The future captures Dawn promises (`wasm_bindgen_futures::JsFuture`) which are `!Send` by construction.
- `Mutex<Option<HashSet<...>>>` for the `warn_pipeline_not_compiled` once-per-session guard. `std::sync::Mutex` works on wasm32 today because contention never happens (single-threaded); a real multithread story would either keep this single-shot or move to `OnceLock`.
- `SlotMap<MaterialId, MaterialState>` and `HashMap<PassKind, PassState>` are accessed `&mut self`; no interior mutability needed.
- Status events drain via `std::mem::take(&mut self.events)` — a single owning move, also single-threaded.

## What changes if/when wasm32-multithread lands

The renderer's render loop will likely stay single-threaded (WebGPU's command-encoder lifetime is per-thread), but a worker pool could drive compiles concurrently. The boundaries `pipeline_scheduler` would have to negotiate:

1. **`PendingFuture` `Send` requirement.** If compile futures live on a worker thread, they need `Send`. Dawn's pipeline-creation promises return `web_sys::GpuComputePipeline` / `GpuRenderPipeline` which are `!Send` — those would have to round-trip through `SharedArrayBuffer` + `postMessage` to cross threads, or stay on the main thread and only the *work-orchestration* parallelizes. Cleanest path: compile-orchestrator stays on the main thread (where the GPU device lives); the *frontend* worker uses `submit_pipeline_group_batch` via a message-passing bridge.

2. **`SlotMap` + `HashMap` access.** If concurrent threads can submit batches, swap the bare collections for `parking_lot::Mutex<...>` (zero overhead under contention; `RwLock` doesn't help because `SlotMap::insert` needs exclusive). Per-pass `generation` markers already discriminate stale resolutions, so the lock-window stays small.

3. **`Vec<StatusEvent>` drain.** Replace with `tokio::sync::mpsc::unbounded_channel` (or equivalent wasm-friendly channel) so multiple producers can emit events from different worker contexts. The drain side stays single-consumer on the render thread.

4. **`warn_pipeline_not_compiled` HashSet guard.** Replace `Mutex<Option<HashSet>>` with `dashmap::DashSet` or `parking_lot::Mutex<HashSet>`. The guard is hit at most once per session per (location, id) pair, so any choice works.

5. **Frontend `drain_pipeline_status_events` subscriber.** Today drains directly from the renderer's `Vec<StatusEvent>`. If a frontend wants to subscribe from a worker, the bridge needs to forward events across the thread boundary — typically via `postMessage` of serialized events. `PipelineGroupId` is `Copy + Hash + Eq` (the `MaterialId` slotmap key + `PassKind` enum) so it's trivially serializable; `PipelineGroupStatus::Failed { error: AwsmError }` is the only `!Serializable` payload — the bridge would carry only a tagged string error message, not the `AwsmError` value.

## Hot-path invariants the multithread story must preserve

- **Compile resolutions still apply between frames.** `poll_resolved` is called from the render loop's pre-frame phase. Even if compiles run on a worker, their resolutions need to coalesce on the main thread before the frame's classify pass reads the per-pass typed pipeline accessors. Channels with a per-frame drain phase preserve this.
- **No per-mesh status query on the render hot path.** Per-pass typed `Option<PipelineKey>` accessors stay; the bucket-entries cache only contains `Ready` materials. Both remain correct under multithreading without lock contention (they're rebuilt between frames from scheduler events).
- **Render-frame preamble warn-and-skip stays once-per-session.** Don't make the HashSet per-thread (a multithreaded application doesn't want N redundant warn lines per pipeline failure); keep a single global guard.

## Open questions for a future migration

- **Worker fan-out for shader templating.** The askama template rendering inside shader-compile is currently single-threaded. A worker pool could parallelize the templating step (which can be the long pole for large dynamic-material counts). Out of scope for `pipeline_scheduler`; lives in `shaders.rs`.
- **`Shaders::ensure_keys` borrow shape.** Today takes `&mut self`; a worker-pool version would need `&Self` + interior mutability or a re-entrant insert primitive. The same `ensure_keys` factoring landing in Block D (sync-descriptor / sync-promise-collection / parallel-await) already moves the API toward something more re-entrancy-friendly.

## TL;DR

The module is structurally single-threaded today but every boundary that would need changing under multithreading is on the same handful of fields. No deep architectural lock-in. A future migration would add `parking_lot::Mutex` wrappers around three fields and a channel for `StatusEvent`s.
