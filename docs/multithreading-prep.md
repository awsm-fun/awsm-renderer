# Multithreading prep — `pipeline_scheduler` + frontend boundaries

> **Note (pre-editor-unification).** Written when the editor was split into
> `scene-editor` + `material-editor`; those are now the single
> `packages/frontend/editor`, whose renderer handle lives in
> `packages/frontend/editor/src/engine/context.rs`. The concurrency *shape* below
> still holds, but verify specific paths/types against the unified editor.

Audit notes for a future wasm32-multithread migration. The scheduler
work landed in [PR #99](https://github.com/dakom/awsm-renderer/pull/99);
this doc captures every place in the renderer + editor frontends that
silently relies on "one thread, ever" so the next session that picks
this up can answer "what would I have to touch?" in 10 minutes
instead of a day.

The intent is **not** to make the renderer thread-safe in this pass.

---

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

## Open questions for a future migration (scheduler-local)

- **Worker fan-out for shader templating.** The askama template rendering inside shader-compile is currently single-threaded. A worker pool could parallelize the templating step (which can be the long pole for large dynamic-material counts). Out of scope for `pipeline_scheduler`; lives in `shaders.rs`.
- **`Shaders::ensure_keys` borrow shape.** Today takes `&mut self`; a worker-pool version would need `&Self` + interior mutability or a re-entrant insert primitive. The same `ensure_keys` factoring landing in Block D (sync-descriptor / sync-promise-collection / parallel-await) already moves the API toward something more re-entrancy-friendly.

---

## Editor-frontend invariants

The editors layer extra single-thread assumptions on top of the renderer. These would all need to change before any worker-pool arrangement is meaningful end-to-end:

### `Rc` + `RefCell` in frontends

The material-editor's `RendererHandle = Rc<RefCell<Option<RendererHost>>>` is the load-bearing single-thread assumption. Multi-thread access would need `Arc<Mutex<...>>` everywhere. The pattern is documented inline in `packages/frontend/editor/src/engine/context.rs` (the unified editor; this pattern came from the old material-editor `host.rs`) and is intentional — the clippy `await_holding_refcell_ref` lint is silenced with a comment explaining wasm32's single-thread model.

scene-editor uses the same pattern (`renderer_bridge` holds an `Rc<RefCell<…>>` over the live renderer).

**Migration shape:** swap to `Arc<Mutex<…>>` per crate, lift the clippy allow comments, audit every long `borrow_mut()` for places where we should switch to `try_lock()` (e.g. the RAF render-loop skips a frame today if the host is busy with `prewarm_pipelines`; the same shape under a Mutex works as `try_lock().ok().and_then(...)`).

### `Mutable<…>` is not `Send`

`futures_signals::signal::Mutable<T>` is `!Send` per its docs. Every piece of `EditState` / scene-editor's app state is built on `Arc<Mutable<…>>`, which is `Send` *only* because of `Arc` — the inner `Mutable` would still need synchronization across threads, and `futures-signals` doesn't currently expose a `SendMutable` variant.

**Migration shape:** futures-signals would need a thread-safe alternative, OR every UI thread keeps its own `tokio::sync::watch::Sender/Receiver` (which is `Send + Sync`) and the renderer thread holds one end. This is a UI-architecture-scale change, not a per-call patch.

---

## Readback-state pattern (the multi-thread-ready template)

`CoverageReadbackState` and `EdgeOverflowReadbackState` already use `std::sync::Arc<std::sync::Mutex<…>>` — they're forward-compatible with multi-thread out of the box because the `mapAsync` resolution runs in a `spawn_local`-detached future that needs the write-from-anywhere shape.

These two structures are the **template** the rest of the renderer should follow when it goes multi-thread: small lock surface, no nested locks, write-through-Arc.

---

## Boundaries that won't move

A few things are inherently single-thread on the platform:

### WebGPU command-encoder ownership

`GpuCommandEncoder.beginRenderPass(…)` returns a pass encoder bound to the encoder; both are `!Send`. Render passes have to run on the thread that holds the encoder. Multi-thread render-frame parallelism would require multiple encoders + a serializing barrier — possible but adds complexity an order of magnitude beyond "use Arc<Mutex>".

### `web_sys::*` JsValue ownership

Every `web_sys::*` handle (`GpuBuffer`, `GpuTexture`, `GpuBindGroup`, etc.) is `JsValue` underneath, and `JsValue` is `!Send` by design. The renderer's `Materials`, `Meshes`, `Lights`, `Shadows`, and every other GPU-resource-holding struct is therefore `!Send` too.

**Implication:** the renderer's hot data structures stay on one thread. CPU-side ECS / spatial / physics work can run on other threads, but the bridge to the renderer is the boundary.

---

## Concrete migration checklist

Each item is independent enough to land on its own.

- [ ] **Audit `Rc` → `Arc`** in the editor frontends. Mechanical, clippy-driven. ~50 sites across material-editor + scene-editor.
- [ ] **Audit `RefCell` → `Mutex`**. Same crates. The borrow_mut + await pattern needs explicit `try_lock()` migration; see the pre-existing `#[allow(clippy::await_holding_refcell_ref)]` comments for the high-risk sites.
- [ ] **Decide on a `SendMutable` story**. Either build one over `futures_signals`'s `Mutable` (Arc + RwLock wrap), or pick a different signal lib that ships `Send + Sync` natively (e.g. `dioxus-signals`, `leptos::ReadSignal`).
- [ ] **Single-thread island for the pipeline scheduler**. Add a message-passing layer in front of `PipelineScheduler` so it can stay `!Send` while the rest of the renderer state moves multi-thread.
- [ ] **Scheduler internal locks.** Apply the changes listed under [§ What changes if/when wasm32-multithread lands](#what-changes-ifwhen-wasm32-multithread-lands).
- [ ] **Verify readback states still work.** `CoverageReadbackState` and `EdgeOverflowReadbackState` should need zero changes — they're already `Send` and use proper locking. Use them as the canonical "this is what right looks like" template when refactoring other state.
- [ ] **CI build under `--cfg=web_sys_unstable_apis` + nightly**. Confirm the wasm-bindgen toolchain even supports `SharedArrayBuffer` on the target version. Currently the project pins `wasm-bindgen = "0.2.118"` (workspace) — confirm compatibility before any of the above.

---

## What you do NOT have to migrate

- The visibility buffer + compute kernels — those are GPU-side, thread-irrelevant.
- The Materials / Meshes / Lights / etc. GPU-resource-holding structures — they stay on the render thread.
- The `FuturesUnordered` patterns themselves — they work fine on a single thread; only their `!Send` inner future types are the constraint.

---

## TL;DR

The module is structurally single-threaded today but every boundary that would need changing under multithreading is on the same handful of fields:

- Three `Mutex`/`HashMap` wraps in `pipeline_scheduler`,
- A channel for `StatusEvent`s,
- `Rc → Arc` + `RefCell → Mutex` sweep in the editor frontends,
- A `Send`-able replacement for `Mutable<T>`.

No deep architectural lock-in.

---

## Cross-references

- Pipeline-readiness architecture: [`packages/crates/renderer/src/pipeline_scheduler/mod.rs`](../packages/crates/renderer/src/pipeline_scheduler/mod.rs) (module-level doc comments cover the single-thread invariants inline).
- Readback-state pattern (the multi-thread-ready template): `CoverageReadbackState` in [`packages/crates/renderer/src/lib.rs`](../packages/crates/renderer/src/lib.rs) and `EdgeOverflowReadbackState` next to it.
- Renderer-host clippy escape: the `prewarm_holding_borrow` / host-borrow pattern (from the old material-editor) now lives in the unified editor at [`packages/frontend/editor/src/engine/context.rs`](../packages/frontend/editor/src/engine/context.rs).
