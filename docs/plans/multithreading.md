# Plan: multithreading the renderer (worker-hosted + shared-memory sim state)

**Status: committed direction, decisions locked, structured for autonomous
execution.** A multi-PR effort delivered as checkpointed milestones (M0–M7), each
with a self-verifiable gate (Rust tests + Chrome DevTools MCP) and a human-review
pause. The single-threaded build is untouched throughout — the editor and
model-viewer keep working exactly as today.

This is the single source of truth for multithreading. It supersedes the old
`docs/multithreading-prep.md` (its still-relevant parts are folded in below; the
editor `Send`/`Sync` sweep it described is now explicitly **out of scope** — see
[§ Platform / toolchain](#platform--toolchain)).

---

## Why

Two motivations, same direction.

### 1. Main-thread responsiveness (immediate, concrete)

Both shipping apps run `AwsmRenderer` **on the main thread** (editor:
`Arc<xutex::AsyncMutex<AwsmRenderer>>`; model-tests:
`Arc<futures::lock::Mutex<AwsmRenderer>>`). Any heavy *synchronous* renderer work
blocks layout/paint/input. Two symptoms:

1. **The loading UI can't paint its fast phases.** `commit_load`'s geometry phase
   is synchronous — it sets progress then runs into the texture phase without
   yielding, so the browser never gets a frame to paint that line (retained-mode
   DOM: the tree updates synchronously but *paint* only runs when the task yields —
   not a dominator issue, not fixable app-side). Only the `.await` phases paint, and
   only when work spans frames. CPU throttle doesn't help — the blocker is
   *yielding*, not speed.
2. **Live editor edits jank the UI.** Every edit runs `commit_load` on the main
   thread, stalling input + paint for its duration.

The wrong fix is yielding inside `commit_load` (adds latency to every commit for a
cosmetic paint, and relaxes the deliberately-atomic commit invariant). The right
fix is architectural: move the renderer off-main, so progress crosses the boundary
as discrete event-loop tasks that paint for free, and edits stop janking.

### 2. Parallel simulation (the larger payoff)

With the renderer off-main, a **physics/sim worker** runs concurrently and shares
transform/instance state with the renderer through **shared linear memory** — no
per-frame `postMessage` (which can be >1ms). The sim worker does the expensive
transform-hierarchy + integration work; the renderer keeps the GPU; the DOM/main
thread is free. This is the "lots of content, smooth" target.

---

## End-state: three threads

1. **Main thread** — DOM/UI (dominator), input capture, app/HUD logic. Thin.
2. **Render worker** — `AwsmRenderer` + render loop, against a transferred
   `OffscreenCanvas`. Single-threaded internally (WebGPU is thread-affine).
3. **Physics/sim worker** — simulation; writes sim state into shared linear memory.

Render worker and physics worker share one `WebAssembly.Memory`; the renderer's
sim-owned buffers live in that shared region. Hot-path coordination is native wasm
atomics (seqlock + dirty bitmap), not `postMessage`. The main thread talks to the
render worker via a typed command/event protocol over `postMessage`.

### The hard constraint (shapes everything) — boundaries that won't move

- **WebGPU is thread-affine.** `GpuCommandEncoder.beginRenderPass(…)` returns a
  pass encoder bound to the encoder; both `!Send`. Render passes run on the thread
  holding the encoder.
- **Every `web_sys::*` GPU handle is `!Send`.** `GpuBuffer`/`GpuTexture`/
  `GpuQueue`/`GpuBindGroup` are `JsValue` underneath; `Materials`/`Meshes`/`Lights`/
  `Shadows` are therefore `!Send` too.

**Implication:** the renderer cannot be parallelized across threads, and the physics
worker can never touch renderer objects or call `queue.writeBuffer`. Shared memory
shares *bytes*, not the ability to call methods. So the renderer stays
single-threaded on the render worker; the physics worker only reads/writes the
shared *data* arena; the GPU upload stays render-worker-only. CPU-side physics/ECS
work runs on the other thread; the shared arena is the bridge.

---

## Locked decisions

| # | Decision | Choice & rationale |
|---|---|---|
| D1 | **Memory model** | **Shared linear memory** (real wasm threads: `+atomics,+bulk-memory` + `build-std`, native atomics). Opt-in threaded build profile; the editor/model-viewer keep the stable single-threaded build untouched. Official pattern (`wasm-bindgen` `raytrace-parallel`, `wasm-bindgen-rayon`). Most performant publish path: native stores + native atomics, zero-copy on the sim side. (Assessed against an explicit-`SharedArrayBuffer` model — that model's only advantage was avoiding the toolchain step; shared linear memory wins on perf and is a documented recipe, and the renderer itself stays single-threaded either way so neither unlocks render parallelism.) |
| D2 | **SAB carries semantic sim state** | The physics worker writes **world `Mat4`** values; the render worker packs to GPU layout (model + inverse-transpose normal matrix) on its dirty descent. Never exposes the raw GPU byte layout; keeps the sim worker ignorant of normal-matrix/alignment details. |
| D3 | **Deployment is opt-in per app** | Single-threaded stays first-class and continuously validated (editor/model-viewer run it daily). Games opt into worker-hosting (Layer 1) and/or shared-memory sim state (Layer 2). |
| D4 | **Layer 1 = full remote-renderer protocol** | A typed command/event protocol so a main-thread driver can fully control the worker renderer (lifecycle, loading, scene mutation, queries). |
| D5 | **Sim-state v1 schema** | **Node transforms + instance transforms + instance attributes.** Transforms first (fixed-slot arena), then instances/attributes (variable-length, buddy path). |
| D6 | **`Send`/`Sync` scope** | Only the **shared-arena boundary types** need `Send + Sync`. The renderer stays `!Send` on the render worker; the editor never goes multithreaded, so the old `Rc→Arc`/`RefCell→Mutex`/`SendMutable` editor sweep and the pipeline-scheduler parallelization are **out of scope**. |
| D7 | **Execution** | Checkpointed milestones (M0–M7), each with an autonomous verification gate and a human-review pause. |

---

## What opting into multithreading changes (the integration delta)

Single-threaded apps keep **one data path**: call a renderer method → it mutates a
local `Vec<u8>` mirror → the render loop uploads. Nothing below applies to them.

| Aspect | Single-threaded (default) | Multithreaded (opt-in) |
|---|---|---|
| **Where the renderer lives** | Main thread, in-process | Render worker; `AwsmRenderer` is `!Send`, never on main |
| **How the app talks to it** | Direct `lock().await` + method calls | Command/event protocol over `postMessage` — no shared object to lock |
| **Canvas** | `HtmlCanvasElement` via `…Builder::new(...)` | `OffscreenCanvas` via `…Builder::new_with_offscreen_canvas(...)` |
| **Sim-owned buffers** (transforms, instance transforms/attrs) | Local `Vec<u8>` mirror | Chunked arena in **shared linear memory** — foreign-writable |
| **All other buffers** (materials, pipelines, GPU handles) | Local / render-private | **Unchanged** — stay render-worker-private |
| **Loading** | Direct calls | `RenderCommand`s; worker runs the load off-main, streams `LoadingStats`/status/errors back as `RenderEvent`s |
| **Edits** (material/env/light) | Direct calls | `RenderCommand`s |
| **Queries** (pick, bounds, screenshot) | Direct / already-async | Async request→reply round-trip |
| **Per-frame sim writes** (transforms/instances) | Direct mutation | Native writes into shared memory + seqlock — **zero `postMessage`** |
| **Spawn / despawn** (topology) | Direct | Owner-thread transaction via command channel: one round-trip at spawn, then lock-free writes |
| **Input** (pointer/wheel/keyboard/resize) | Direct DOM listeners | Captured on main, forwarded as `WorkerInputEvent` |
| **Coordination** | `AsyncMutex` (single executor) | Native wasm atomics (seqlock) in shared memory + `postMessage` for commands |
| **Build / serving** | Stable wasm, no special headers | Nightly `+atomics,+bulk-memory` + `build-std`; COOP/COEP headers |

Three things to internalize:

1. **Three data paths, not one.** SAB + atomics for the per-frame hot path
   (transforms, instances); the `postMessage` *command* channel for imperative/rare
   ops (loading, edits, spawn/despawn); the `postMessage` *event* channel for results
   (progress, status, errors, query replies). The discipline is routing each op to
   the right path.
2. **Shared memory is opt-in per buffer.** Only the sim-owned set becomes
   foreign-writable. Materials, pipeline state, GPU handles stay exactly as they
   are. Opting in doesn't "convert your buffers" — it adds a capability-gated shared
   region for the hot sim state.
3. **Loading becomes fire-command + observe-events.** `commit_load` runs off-main;
   the worker streams per-phase `LoadingStats` events → the DOM paints each phase for
   free. That's the responsiveness win.

---

## Layer 1 — full remote-renderer protocol (D4)

A typed command/event protocol so a main-thread driver controls the worker
renderer. Much plumbing exists: OffscreenCanvas transfer + `is_worker_scope` +
single bundle + `WorkerInputEvent` (`packages/examples/render-worker`); `WorkerPool`
+ blob bootstrap + shared `WebAssembly.Module` + `serde_wasm_bindgen` +
`post_message_with_transfer` (`workers/{pool,blob,entry}.rs`).

**`RenderCommand` (main → worker)** — `serde` / `serde_wasm_bindgen`:
- Lifecycle: `Init { offscreen, … }`, `Start`, `Stop`, `Resize { w, h }`.
- Load transaction: `BeginLoad`, `RegisterGeometry { bytes (Transferable) }`,
  `AddMesh { geometry, material, transform, opts }`, `CommitLoad`.
- Scene mutation: `SetLocal { key, transform }`, `SetMeshMaterial`, `UpdateCamera`,
  light/decal/env updates.
- Queries (request→reply): `Pick { x, y }`, `Bounds { keys }`, `Screenshot`.

**`RenderEvent` (worker → main)** — all small/`Copy`/serializable:
- `Loading(LoadingStats)` (already `Copy`: phase + counts), `PipelineStatus`,
  `Error(String)`, `PickResult` (already `Copy`), query replies
  (`Bounds(Aabb…)`, `Screenshot(bytes)`).

**Transfer rules** (from the API audit):
- Handles (`MeshKey`/`GeometryKey`/`MaterialKey`/`TransformKey`) are `SlotMap`
  keys — pass by value, never serialize internals.
- `LoadingStats`/`Transform`/`CameraMatrices`/`Aabb`/`AddMeshOpts`/`Decal`/
  `PickResult` are plain data → safe to `postMessage`.
- Geometry/texture payloads (`Vec<f32>`/`Vec<u8>`) → **Transferable `Uint8Array`**
  (zero-copy), reconstructed worker-side. Never send `GeometrySource`/`Material`/
  `Skybox`/`Ibl` directly (they hold `web_sys` GPU objects).
- A game's own logic may instead run *inside* the render worker and call the
  renderer in-process; the remote protocol is for main-thread drivers (HUD/UI,
  or an editor-style host) that need it.

---

## Layer 2 — shared-memory sim state (architecture)

Five load-bearing decisions, forced by "general-purpose renderer, lots of content."
A "cheap hack" (full re-upload each frame, flat per-slot dirty scan, ad-hoc
topology) is rejected — it doesn't scale to millions of mostly-static objects.

### A. Semantic values, render-side pack (D2)
Sim writes world `Mat4`; render packs 64B → 112B (model + derived normal) inline
during its dirty descent and hands `(offset,len)` ranges to the **existing**
uploader. Pack work ∝ dirty count — same shape the renderer already has, just
sourced from another thread. For sim-owned nodes the render worker skips its own
`update_world` hierarchy walk.

### B. Stable addressing — growth never moves data
Today the mirror is one growable `Vec<u8>` (`resize()` reallocs, base pointer
moves) — fatal for a foreign writer holding an offset. Replace with a **chunked
arena** in shared memory: fixed-size chunks, slots never move once assigned, growth
appends a chunk. A slot→(chunk, offset) binding is valid forever. (Slot *indices*
are already stable across the current free-list resize in `dynamic_uniform.rs`;
this makes the *addresses* stable too, which shared memory requires.)

### C. Topology is owner-only; foreign threads write values only
`update_with` in `buffer/dynamic_uniform.rs` couples three mutations: (a) value
bytes, (b) `mark_dirty_range`, (c) slot/free-list/`resize` allocation. Split them:
foreign (physics) threads get (a)+(b) on already-allocated slots; the owner (render
worker) keeps (c) behind a command channel. A body requests a slot binding at spawn
(one round-trip), then writes lock-free every frame. Matches the existing "loading
is ONE transaction" law: spawn/despawn is a transaction; motion is not. The hot
path touches zero topology.

### D. Seqlock = dirty + publication; tiered dirty scales with *changes*
- **Per-slot version (native `AtomicU32` seqlock):** writer bumps odd → write →
  even (release/acquire). Reader: "version ≠ last-seen" = dirty; "odd or unstable
  across the read" = torn → reuse last frame's value (one-frame staleness,
  self-heals). One atomic per slot solves tearing **and** dirty.
- **Coarse chunk dirty bitmap (`AtomicU32` words):** writer sets its chunk's bit
  (atomic-or). Render descends only dirty chunks → scan cost ∝ touched chunks, not
  total slots. A million-object scene with 200 movers costs ~200 movers of work.
  Overflow-free (unlike a fixed dirty-index ring); coalesces into `(offset,len)`.

### E. Downstream GPU path untouched
The render worker turns descended dirty slots into the same `(offset,len)` ranges
`MappedUploader::write_dirty_ranges` / `mapped_staging_ring` already consume. Only
the *front* of the pipe changes (where bytes live + where dirty originates).
Evidence this is an evolution of the buffer primitives, not a renderer rewrite.

### v1 sim-state schema (D5) — exact layout

Three foreign-writable buffers, all in shared memory. Each region =
`[value region]` + `[version region: u32/slot]` + `[chunk dirty bitmap]`, with a
header `{ stride, slot_count, chunk_size, capacity }`.

| Buffer | Source | Value stride (semantic) | Allocation | Existing write path to mirror |
|---|---|---|---|---|
| **Node transforms** | `transforms.rs` (`DynamicUniformBuffer<TransformKey>`) | 64B world `Mat4` (render packs to 112B model+normal) | Fixed-slot arena | `set_local` → `update_world` → pack → `write_gpu` |
| **Instance transforms** | `instances.rs` (`DynamicStorageBuffer<TransformKey>`) | 64B `Mat4` per instance (no derived data) | Variable extent (buddy); count = topology | `transform_write_all` / `transform_update` (zero-alloc steady-state) |
| **Instance attributes** | `instances.rs` (`InstanceAttr`, `repr(C)`) | 16B (`color_packed:u32, size:f32, alpha:f32, _pad`) | Variable extent (buddy) | `attribute_write_all` / `attribute_update` |

Deferred from the foreign-writable set: **lights** (today repacked densely each
frame — no stable slot; needs a refactor first), **morph weights / skin joints**
(animation, not physics). Everything else (materials, pipeline state, GPU handles)
stays render-worker-private — opt-in per buffer.

The existing **particle simulator**
(`packages/frontend/editor/src/engine/bridge/particles.rs`) is the proof-pattern
for instances: it already does per-frame `transform_write_all` + `attribute_write_all`
zero-alloc; the physics worker mirrors that, sourced from shared memory.

---

## Platform / toolchain (D1, D6)

Shared linear memory needs a distinct **threaded build profile** (the
single-threaded build is unchanged):

- **Toolchain:** pinned nightly (a `rust-toolchain.toml` scoped to the threaded
  game/worker packages, or nightly invoked explicitly for them).
- **Flags:** `RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals"`,
  build with `-Z build-std=std,panic_abort`. (Today `.cargo/config.toml` sets only
  `--cfg=web_sys_unstable_apis` + the getrandom backend; the threaded profile adds
  the above on top.)
- **wasm-bindgen:** emit shared memory; workers attach to the **same**
  `WebAssembly.Memory` by calling the glue `init(module, memory)` (the
  `raytrace-parallel` / `wasm-bindgen-rayon` bootstrap). `wasm-bindgen 0.2.118` is
  compatible. Extend the existing blob-worker + shared-`Module` pattern in
  `workers/blob.rs` to also pass `memory`.
- **Worker roles:** explicit `render_main(offscreen)` and `physics_main()` entry
  points (named role workers, not a rayon pool). `web_global` already abstracts
  main-vs-worker `request_animation_frame`/`navigator_gpu`/`performance`.
- **Headers:** dev + prod must send `Cross-Origin-Opener-Policy: same-origin` and
  `Cross-Origin-Embedder-Policy: require-corp` (required for `crossOriginIsolated`).

**`Send`/`Sync` (scoped, D6):** only the shared-arena handle + types crossing the
render↔physics boundary need `Send + Sync`. The renderer stays `!Send`. **Explicitly
NOT in scope** (the old prep-doc sweep): the editor `Rc→Arc`/`RefCell→Mutex`/
`SendMutable` migration (editor stays main-thread single-threaded) and the
pipeline-scheduler parallelization (the renderer stays single-threaded on the render
worker; compiles happen there as today). `CoverageReadbackState` /
`EdgeOverflowReadbackState` are already `Arc<Mutex<…>>` write-through-Arc — the
reference shape for any shared state that does arise.

**Empirical unknowns to settle early:**
- Does current Chrome's `queue.writeBuffer` accept a shared-memory-backed
  `TypedArray`? If not, the render side copies dirty chunks to a regular
  `ArrayBuffer` before upload (proportional to movers — cheap). Verify in M2.
- Confirm the exact nightly + `build-std` invocation that links cleanly for this
  workspace. Settle in M0.

---

## Implementation plan — checkpointed milestones (D7)

Execution model: for each milestone the agent (1) does the work, (2) runs
`cargo test` + the threaded build, (3) serves with COOP/COEP, (4) drives the
**Chrome DevTools MCP** to verify the gate, (5) commits on a branch at the gate,
(6) **pauses for human review**. Gates are pass/fail; do not advance on a red gate.

> **Chrome DevTools MCP toolkit:** `navigate_page`, `evaluate_script` (read
> `crossOriginIsolated`, assertion flags), `list_console_messages`,
> `take_screenshot` (visual / before-after motion), `list_network_requests` (prove
> no per-frame postMessage on the hot path), `performance_start_trace` /
> `performance_stop_trace` (main-thread responsiveness).

### M0 — Threaded build + cross-origin isolation *(gating unknown)*
**Relocate `packages/examples/` → root `examples/`** (update the `Cargo.toml`
workspace member at line 20 and any `Taskfile.yml`/taskfile references) and scaffold
a **standalone multithreaded reference app** there — its own `Trunk.toml`,
`index.html`, and a dev-serve config that sends COOP/COEP (this is the reference
consumers copy from). This example is the living artifact every later milestone
extends. Add the nightly threaded build profile (flags + `build-std`). Minimal
2-worker smoke in the example: both workers attach to one `WebAssembly.Memory`;
worker A increments an `AtomicU32` in shared linear memory, worker B observes it.
- **Gate:** `evaluate_script` → `crossOriginIsolated === true` and
  `typeof SharedArrayBuffer !== 'undefined'`; console shows B observing A's
  increments across the thread boundary. **Commit + review.**

### M1 — Shared arena + seqlock primitive *(Rust, in shared memory)*
New `packages/crates/renderer/src/buffer/shared_arena.rs`: chunked stable-address
arena over shared memory behind a backing trait; per-slot `AtomicU32` seqlock;
coarse chunk dirty bitmap; `write_value` (foreign) vs `allocate`/`free`/`resize`
(owner); reader descends dirty chunks → `(offset,len)` ranges with torn-read
detection.
- **Gate:** `cargo test` for pure logic (seqlock odd/even, torn detection under
  simulated interleave, dirty coalescing, stable addressing across grow). Browser
  2-worker test: physics worker writes a known ramp at high rate; render worker
  reads, asserts zero torn values + dirty set matches; `evaluate_script` reads a
  pass flag, console confirms. **Commit + review.**

### M2 — Re-base node transforms onto the arena *(no physics yet)*
Feature-gate `DynamicUniformBuffer<TransformKey>` to back its mirror with the shared
arena (semantic 64B `Mat4`); single-threaded build keeps the `Vec<u8>` path.
Render-worker dirty descent packs 64B → 112B (model + inverse-transpose normal) into
the existing staging path; `mapped_uploader` untouched. Settle the
`writeBuffer`-from-shared-memory question here (copy-to-regular fallback if needed).
- **Gate:** `cargo test` proving packed bytes equal current packing. Browser:
  render worker hosts the renderer, populates the transform arena itself (no
  physics), scene renders **identically** to single-threaded — `take_screenshot`
  visual match, console clean. **Commit + review.**

### M3 — Physics worker writes transforms → objects move *(hot-path proof)*
Physics-stub worker integrates simple motion for N bodies, writes world `Mat4` into
arena slots + seqlock bump + chunk dirty bit; slot bindings via the topology command
channel at spawn. Render worker reads dirty → packs → uploads. **Zero postMessage on
the hot path.**
- **Gate:** `take_screenshot` at t0/t1 shows objects moved; `list_network_requests`
  /console shows no per-frame postMessage (only atomics); a `?stress=N` run shows
  dirty-scan cost tracking movers, not total. **Commit + review.**

### M4 — Instance transforms + attributes *(variable-length buddy path)*
Extend the arena/schema for the two instance buffers: count change = topology
(owner-side), per-instance value writes = foreign. Mirror `transform_write_all` /
`attribute_write_all` from the physics worker (the particle-sim pattern).
- **Gate:** a crowd/particle stress scene driven by the physics worker — screenshot
  shows the instanced motion; `?stress=N` bench holds. **Commit + review.**

### M5 — Full Layer 1 remote-renderer protocol
Implement `RenderCommand`/`RenderEvent` with `serde_wasm_bindgen` + Transferable
geometry/texture bytes; reuse `workers/blob.rs` + `post_message_with_transfer`. A
main-thread DOM driver loads a glTF via commands; the worker streams
`Loading(LoadingStats)`; `Pick` round-trips.
- **Gate:** main-thread driver loads a model into the worker renderer; a progress
  bar paints from `Loading` events (`take_screenshot` mid-load shows phases); final
  screenshot shows the model; a `Pick` returns a hit. **Commit + review.**

### M6 — Input forwarding + responsiveness
Wire all `WorkerInputEvent` variants main→worker + `ResizeObserver`. Confirm the
main thread stays responsive during a heavy worker-side load/compile.
- **Gate:** `performance_start_trace`/`stop_trace` shows main-thread frames keep
  painting during a cold load in the worker (no long tasks on main). **Commit +
  review.**

### M7 — Hardening + docs + reference example
Confirm the backing-trait isolation; document the threaded build profile; capture
the `writeBuffer`-from-shared-memory result; finalize `?stress` benches. Prove the
editor + model-viewer still build/run on the **stable single-threaded** profile
unchanged, alongside the threaded game build. Then finalize the two user-facing
deliverables:
- **Standalone reference example** (root `examples/`, started in M0): a complete,
  copyable multithreaded app — Trunk + `index.html` + COOP/COEP serve config +
  render worker + physics worker + the shared-memory sim-state hand-off — that a
  consumer can run as-is and learn the pattern from.
- **`docs/PLAYER-GUIDE.md` usage section** (the guide already exists — extend it):
  how to opt a game into multithreading — the threaded build profile, COOP/COEP
  headers, spawning the render + physics workers, the `RenderCommand`/`RenderEvent`
  protocol, and binding bodies to sim-state slots. Link the reference example.
- **Gate:** editor (single-threaded) and the game example (threaded) both run and
  screenshot correctly from the same source tree; `PLAYER-GUIDE.md` documents the
  flow and points at the runnable example. **Commit + review. Done.**

---

## Autonomous `/loop` prompt

Paste this as the `/loop` task (self-paced; it pauses at each gate):

> Implement `docs/plans/multithreading.md` one milestone at a time, in order
> (M0→M7). For the current milestone: do the code/build work; run `cargo test` and
> the threaded build; start the dev server with COOP/COEP headers; then use the
> **chrome-devtools MCP** to verify that milestone's gate exactly as written
> (navigate, `evaluate_script` for `crossOriginIsolated`/assertion flags,
> screenshots for visual/motion proof, network/console for "no per-frame
> postMessage", performance traces for responsiveness). If the gate is RED, iterate
> and re-verify — do not advance. When the gate is GREEN, commit on a branch with a
> milestone-tagged message, then STOP and summarize for human review before the next
> milestone. Never skip a gate or advance past a red one. Keep the single-threaded
> editor/model-viewer build working at every step.

---

## What already exists vs. must be built

**Exists (reuse):** OffscreenCanvas transfer + `is_worker_scope` + single bundle +
`WorkerInputEvent` (`examples/render-worker`); `WorkerPool` + blob bootstrap +
shared `WebAssembly.Module` + `serde_wasm_bindgen` + `post_message_with_transfer`
(`workers/{pool,blob,entry}.rs`); `web_global` main-vs-worker dispatch;
`new_with_offscreen_canvas` builder; the dirty-range uploader + staging ring;
`LoadingStats`/`PickResult` already `Copy`; the particle-sim zero-alloc instance
write pattern.

**Must build:** relocate `packages/examples/` → root `examples/` + a standalone
multithreaded reference app (Trunk + COOP/COEP serve) (M0); threaded build profile
(M0); shared arena + seqlock primitive (M1); arena-backed transforms with
render-side pack (M2); physics worker + topology command channel (M3); instance/attr
buddy arena path (M4); the full `RenderCommand`/`RenderEvent` protocol + Transferable
payloads (M5); full input forwarding + responsiveness proof (M6); profile hardening +
a `docs/PLAYER-GUIDE.md` usage section + the finalized reference example (M7).

**Note:** `experiments/` is gitignored local scratch (parity-baseline PNGs) — not
part of this work and not to be committed.

## Reference files

- `packages/examples/render-worker/{src/lib.rs,src/worker.rs,index.html}` — worker
  bootstrap, `WorkerInputEvent`, OffscreenCanvas transfer, `render_worker_start`.
- `packages/crates/renderer/src/workers/{pool,blob,entry}.rs` — `WorkerPool`,
  `WORKER_BOOTSTRAP_JS`, `WorkerJob`, transfer-list messaging.
- `packages/crates/renderer/src/web_global.rs` (+ renderer-core mirror) — main-vs-
  worker globals.
- `packages/crates/renderer/src/buffer/dynamic_uniform.rs` — `update_with`
  (value/dirty/alloc coupling, decision C), `take_dirty_ranges`, `raw_slice`.
- `packages/crates/renderer/src/buffer/dynamic_storage.rs` — buddy allocator
  (instances/attrs, M4).
- `packages/crates/renderer/src/buffer/{mapped_uploader.rs,mapped_staging_ring.rs}`
  — the untouched downstream upload path (decision E).
- `packages/crates/renderer/src/transforms.rs` — `set_local`/`update_world`/
  `write_gpu`, 112B pack (decision A, M2).
- `packages/crates/renderer/src/instances.rs` — `transform_write_all`/
  `attribute_write_all`, `InstanceAttr` `repr(C)` (M4).
- `packages/frontend/editor/src/engine/bridge/particles.rs` — the per-frame
  zero-alloc instance write pattern (M4 proof-pattern).
- `packages/crates/renderer/src/{loading.rs,renderer.rs,picker.rs}` —
  `LoadingStats`/`LoadPhase`, `begin_load`/`register_geometry`/`add_mesh`/
  `commit_load`, `pick`/`PickResult` (Layer 1 protocol, M5).
- `.cargo/config.toml` — current single-threaded RUSTFLAGS (threaded profile builds
  on top, M0).
- `docs/DEPLOYMENT_MODES.md` — main-thread vs. OffscreenCanvas worker modes.
- External: `wasm-bindgen` `raytrace-parallel`, `wasm-bindgen-rayon` — the
  shared-memory bootstrap recipe (M0).

## Relationship to other plans

Orthogonal to the "one geometry flow" epic (`docs/plans/todo.md`) — that
consolidates *what* geometry is; this moves *where the renderer runs* and *who may
write its sim state*. Independent, same "one obvious way" goal.
