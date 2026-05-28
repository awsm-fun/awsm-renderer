# Multithreading-readiness audit

**Status:** the renderer is single-threaded today (wasm32 runs the
whole `AwsmRenderer` on the page's main thread). This document
inventories the places that would need to change when `wasm32` gains
shared-memory threads (the `wasm-bindgen-rayon` / `SharedArrayBuffer`
path that's currently behind nightly Rust + COOP/COEP headers on the
serving page), focused on the new pipeline-scheduler surface that
landed in PR #99.

The intent is **not** to make the renderer thread-safe in this pass.
It's to make the next session that picks this up able to answer
"what would I have to touch?" in 10 minutes instead of a day.

---

## Single-thread invariants the codebase relies on

Several pieces of the codebase silently rely on "one thread, ever":

### 1. `Rc` + `RefCell` in frontends

The material-editor's `RendererHandle = Rc<RefCell<Option<RendererHost>>>`
is the load-bearing single-thread assumption. Multi-thread access would
need `Arc<Mutex<...>>` everywhere. The pattern is documented inline in
`crates/frontend/material-editor/src/host.rs` and is intentional — the
clippy `await_holding_refcell_ref` lint is silenced with a comment
explaining wasm32's single-thread model.

scene-editor uses the same pattern (`renderer_bridge` holds an
`Rc<RefCell<…>>` over the live renderer).

**Migration shape:** swap to `Arc<Mutex<…>>` per crate, lift the
clippy allow comments, audit every long `borrow_mut()` for places
where we should switch to `try_lock()` (e.g. the RAF render-loop
skips a frame today if the host is busy with `prewarm_pipelines`;
the same shape under a Mutex works as `try_lock().ok().and_then(...)`).

### 2. `Mutable<…>` is not `Send`

`futures_signals::signal::Mutable<T>` is `!Send` per its docs. Every
piece of `EditState` / scene-editor's app state is built on
`Arc<Mutable<…>>`, which is `Send` *only* because of `Arc` — the
inner `Mutable` would still need synchronization across threads,
and `futures-signals` doesn't currently expose a `SendMutable`
variant.

**Migration shape:** futures-signals would need a thread-safe
alternative, OR every UI thread keeps its own
`tokio::sync::watch::Sender/Receiver` (which is `Send + Sync`) and
the renderer thread holds one end. This is a UI-architecture-scale
change, not a per-call patch.

### 3. `pipeline_scheduler::FuturesUnordered`

The scheduler holds two `FuturesUnordered<…>` queues
(`inflight` + `inflight_compile`) on `PipelineScheduler`. These are
`Send`-able when their inner futures are — but
`PendingFuture` / `PipelineCompileFuture` wrap promises returned by
`GpuDevice::createComputePipelineAsync`, which are `!Send` because
`wasm_bindgen::JsValue` (the inner promise handle) is `!Send`.

This is the **most material** single-thread tie in the new scheduler
surface. A future multi-thread story would need to either:

- Pin the scheduler to a single thread (single-thread island,
  message-passing in/out) — pragmatic and matches the JS event loop
  reality on wasm32.
- Move the GPU device handle behind a worker thread that runs the
  scheduler exclusively, with `MessageChannel`-style cross-thread
  RPCs to the UI thread for status events.

**Recommended approach:** the single-thread island. WebGPU + JS
promises are inherently main-thread on wasm32 anyway; the scheduler
just shouldn't pretend otherwise.

### 4. Per-render-frame `Mutex` locks for readback state

`CoverageReadbackState` and the new `EdgeOverflowReadbackState` already
use `std::sync::Arc<std::sync::Mutex<…>>` — they're forward-compatible
with multi-thread out of the box because the `mapAsync` resolution
runs in a `spawn_local`-detached future that needs the
write-from-anywhere shape.

These two structures are the **template** the rest of the renderer
should follow when it goes multi-thread: small lock surface, no
nested locks, write-through-Arc.

---

## Boundaries that won't move

A few things are inherently single-thread on the platform and the
scheduler shouldn't try to abstract over them:

### WebGPU command-encoder ownership

`GpuCommandEncoder.beginRenderPass(…)` returns a pass encoder bound
to the encoder; both are `!Send`. Render passes have to run on the
thread that holds the encoder. Multi-thread render-frame parallelism
would require multiple encoders + a serializing barrier — possible
but adds complexity an order of magnitude beyond "use Arc<Mutex>".

### `web_sys::*` JsValue ownership

Every `web_sys::*` handle (`GpuBuffer`, `GpuTexture`, `GpuBindGroup`,
etc.) is `JsValue` underneath, and `JsValue` is `!Send` by design.
The renderer's `Materials`, `Meshes`, `Lights`, `Shadows`, and every
other GPU-resource-holding struct is therefore `!Send` too.

**Implication:** the renderer's hot data structures stay on one
thread. CPU-side ECS / spatial / physics work can run on other
threads, but the bridge to the renderer is the boundary.

---

## Concrete migration checklist (for the future-session that picks this up)

Run through these in order; each is independent enough to land on
its own.

- [ ] **Audit `Rc` → `Arc`** in the editor frontends. Mechanical,
      clippy-driven. ~50 sites across material-editor + scene-editor.
- [ ] **Audit `RefCell` → `Mutex`**. Same crates. The borrow_mut +
      await pattern needs explicit `try_lock()` migration; see the
      pre-existing `#[allow(clippy::await_holding_refcell_ref)]`
      comments for the high-risk sites.
- [ ] **Decide on a `SendMutable` story**. Either build one over
      `futures_signals`'s `Mutable` (Arc + RwLock wrap), or pick a
      different signal lib that ships `Send + Sync` natively (e.g.
      `dioxus-signals`, `leptos::ReadSignal`).
- [ ] **Single-thread island for the pipeline scheduler**. Add a
      message-passing layer in front of `PipelineScheduler` so it
      can stay `!Send` while the rest of the renderer state moves
      multi-thread.
- [ ] **Verify readback states still work**. `CoverageReadbackState`
      and `EdgeOverflowReadbackState` should need zero changes —
      they're already `Send` and use proper locking. Use them as
      the canonical "this is what right looks like" template when
      refactoring other state.
- [ ] **CI build under `--cfg=web_sys_unstable_apis` + nightly**.
      Confirm the wasm-bindgen toolchain even supports
      `SharedArrayBuffer` on the target version. Currently the
      project pins `wasm-bindgen = "0.2.118"` (workspace) — confirm
      compatibility before any of the above.

---

## What you do NOT have to migrate

- The visibility buffer + compute kernels — those are GPU-side,
  thread-irrelevant.
- The Materials / Meshes / Lights / etc. GPU-resource-holding
  structures — they stay on the render thread.
- The `FuturesUnordered` patterns themselves — they work fine on a
  single thread; only their `!Send` inner future types are the
  constraint.

---

## Cross-references

- Pipeline-readiness architecture: [`crates/renderer/src/pipeline_scheduler/mod.rs`](../crates/renderer/src/pipeline_scheduler/mod.rs)
  (module-level doc comments cover the single-thread invariants
  inline).
- Readback-state pattern (the multi-thread-ready template):
  `CoverageReadbackState` in [`crates/renderer/src/lib.rs`](../crates/renderer/src/lib.rs)
  and `EdgeOverflowReadbackState` next to it.
- Renderer-host clippy escape: see the long comment block above
  `prewarm_holding_borrow` in
  [`crates/frontend/material-editor/src/host.rs`](../crates/frontend/material-editor/src/host.rs).
