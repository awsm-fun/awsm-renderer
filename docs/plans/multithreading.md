# Plan: multithreading the renderer

**Status: direction, not scheduled.** A motivated architectural direction captured
from design discussion — not yet on `docs/plans/todo.md`, and a multi-PR effort,
not a feature flag. Deliberately kept out of the executable todo: the renderer
library should NOT add asynchronous jank (mid-operation yields) to work around an
application threading choice. The right fix is architectural — move the renderer
off the main thread, and let other threads (physics, sim) share state with it.

This is the single source of truth for multithreading. It supersedes the old
`docs/multithreading-prep.md` (the `Send`/`Sync` audit, now folded into
[§ Platform prerequisites](#platform-prerequisites)).

---

## Why

Two motivations, both pointing the same way.

### 1. Main-thread responsiveness (the immediate, concrete win)

Both shipping apps run the `AwsmRenderer` **on the main thread**:

- model-tests: `renderer: Arc<futures::lock::Mutex<AwsmRenderer>>`
- editor: `RendererHandle = Arc<xutex::AsyncMutex<AwsmRenderer>>` (its `WorkerPool`
  runs only `GltfParseJob` — parallel glTF *parsing* — not the renderer)

So any heavy *synchronous* renderer work blocks the main thread's layout/paint and
input. Two concrete symptoms:

1. **The loading UI can't paint its fast phases.** `commit_load`'s geometry phase
   (`resolve_geometry`) is synchronous — it sets `on_progress(UploadingGeometry
   X/Y)` then runs straight into the texture phase without yielding, so the browser
   never gets a frame to paint that line (the DOM is retained-mode: imperative
   `set_text_content` updates the tree synchronously but the *screen paint* still
   only runs when the task yields — so this is NOT a dominator/reactive issue and
   NOT fixable app-side). Only the phases that `.await` (texture finalize, pipeline
   compile) can paint, and only when the work spans real frames (cold compile →
   "Compiling pipelines (N)" shows; warm cache → nothing). 20× CPU throttle does
   not help — the blocker is *yielding*, not speed.
2. **Live editor edits jank the UI.** Every material/geometry/texture edit runs
   `commit_load` on the main thread, stalling input + paint for its duration.

The tempting-but-wrong fix is to make `commit_load` yield to the event loop after
each phase. That adds ~1 event-loop turn per phase (`setTimeout(0)` ≈ 4ms clamped,
rAF ≈ 16ms) to **every** commit — including every live edit — purely so a cosmetic
progress line can paint, and it relaxes the deliberately-atomic "commit_load stays
identical" invariant. The library has no reason to add that latency for an
application's threading decision.

### 2. Parallelism / "kinda smooth" (the larger payoff)

Move the renderer off-main and a second thread can run **physics / simulation**
concurrently, sharing transform + uniform state with the renderer *without* a
per-frame `postMessage` round-trip (which can itself be >1ms). The renderer keeps
the GPU; the sim thread does the expensive transform-hierarchy + integration work;
the DOM/main thread is free. This is where the architecture in
[§ Layer 2](#layer-2--sab-simulation-state-sharing) comes from.

---

## End-state: a two-thread model

1. **Renderer + game loop in an OffscreenCanvas worker** → frees the DOM/main
   thread. See [§ Layer 1](#layer-1--worker-hosted-renderer-offscreencanvas).
2. **Physics/sim in a second worker, sharing simulation state with the render
   worker via `SharedArrayBuffer`**, coordinated by `Atomics`. See
   [§ Layer 2](#layer-2--sab-simulation-state-sharing).

The two layers are independent and stack: Layer 1 alone fixes responsiveness;
Layer 2 adds true parallel simulation on top.

### Decided: single-threaded stays first-class; multithreading is opt-in per app

Multithreading is **opt-in per application, at a single builder call site** —
`AwsmRendererWebGpuBuilder::new(gpu, html_canvas)` (main-thread) vs
`new_with_offscreen_canvas(...)` (worker). The same source compiles both; the
canvas kind and the rAF source are dispatched at runtime via `web_global`.
Consequences, fixed as a decision:

- **The editor and model-viewer (model-tests) stay main-thread, unchanged.** They
  keep their in-process `renderer_handle().lock().await` surface (~49 sites);
  neither Layer 1 nor Layer 2 is forced on them. This is the guarantee they keep
  working exactly as today.
- **Single-threaded is a permanent, first-class, supported mode — not a legacy
  fallback.** Because the editor and model-viewer run that way every day, the
  single-threaded path is *continuously exercised and validated*. A game that
  gains nothing from parallelism (GPU-bound, or simple sim) can opt to stay
  single-threaded and rely on that same well-trodden path.
- **Games opt into worker hosting (Layer 1) and/or SAB sim-state (Layer 2)
  independently**, only where they benefit — typically physics-driven scenes with
  lots of dynamic content.
- **Moving the editor itself to a worker is out of scope for this plan.** It is
  *achievable* but a project in its own right (the renderer is `!Send`, so every
  closure that today holds the renderer guard *and* touches main-thread UI state
  would split into command→worker / await→reply / touch-UI-on-main — see
  [§ Open questions / risks](#open-questions--risks)). It is not a free consequence
  of landing the layers, and nothing here depends on it.

See also `docs/DEPLOYMENT_MODES.md` for the main-thread vs. worker mode shapes.

### What is and isn't possible (the hard constraint)

- **Direct GPU-buffer mutation from another thread — NOT possible.** Every
  `web_sys::GpuBuffer`/`GpuTexture`/`GpuQueue` is a `JsValue` and `!Send`; the
  WebGPU device is thread-affine. `queue.writeBuffer` runs only on the
  device-owning thread. (See [§ Boundaries that won't move](#boundaries-that-wont-move).)
- **SAB-backed CPU state — feasible, and is the smooth thing we want.** The dynamic
  buffers already keep an explicit CPU mirror (`raw_data: Vec<u8>` in
  `DynamicUniformBuffer`/`DynamicStorageBuffer`) that is dirty-range-uploaded each
  frame. That mirror is what becomes shared memory. The GPU upload stays
  render-thread-only; the cross-thread data-marshaling cost goes to zero.
- **postMessage is a red herring on the hot path.** Coordination is via `Atomics`
  on the SAB (sub-microsecond), not structured-clone messages (the >1ms cost).
  postMessage survives only for coarse, low-frequency input forwarding (the
  existing `WorkerInputEvent` enum) and the topology command channel.

---

## What opting into multithreading changes (the integration delta)

Single-threaded apps (editor, model-viewer, simple games) keep **one data path**:
call a renderer method → it mutates a local `Vec<u8>` mirror → the render loop
uploads. Nothing below applies to them.

Opting in changes that path. Concretely, side by side:

| Aspect | Single-threaded (default) | Multithreaded (opt-in: worker + SAB) |
|---|---|---|
| **Where the renderer lives** | Main thread, in-process | Dedicated worker; `AwsmRenderer` is `!Send`, never touches main |
| **How the app talks to it** | Direct `renderer_handle().lock().await` + method calls | Command/event protocol over `postMessage` — there is no shared object to lock |
| **Canvas** | `HtmlCanvasElement` via `…Builder::new(...)` | `OffscreenCanvas` transferred to worker via `…Builder::new_with_offscreen_canvas(...)` |
| **Sim-owned buffers** (transforms + defined sim uniforms) | Local `Vec<u8>` mirror | `SharedArrayBuffer`-backed chunked arena — foreign-writable |
| **All other buffers** (materials, pipeline state, GPU handles) | Local `Vec<u8>` / render-thread-private | **Unchanged** — stay local/private even in worker mode |
| **Loading** (`begin_load` → `register_geometry` → `add_mesh` → `commit_load`) | Direct calls on the main thread | Imperative commands serialized over `postMessage`; the worker runs the real load off-main and streams `LoadingStats`/compile-status/errors back as `postMessage` events |
| **Edits** (material / env / light) | Direct calls | `postMessage` commands |
| **Value-returning queries** (pick, world-AABB / bounds, screenshot) | Direct call / already-async readback | Async main↔worker round-trip (request → await reply) |
| **Per-frame sim writes** (transforms/uniforms from physics) | Direct mutation, same thread | Written straight into the SAB + `Atomics` seqlock — **zero `postMessage`** (the whole point) |
| **Spawn / despawn** (topology: alloc slot / free / resize) | Direct | Owner-thread transaction via command channel: one round-trip at spawn, then lock-free per-frame writes |
| **Input** (pointer / wheel / keyboard / resize) | Direct DOM listeners on the canvas | Captured on main, forwarded to the worker as `WorkerInputEvent` `postMessage`s |
| **Coordination primitive** | `AsyncMutex` (cooperative, single executor) | Cross-thread `Atomics` (seqlock) on the SAB + `postMessage`; the renderer's internal locks become real (`parking_lot::Mutex`) |
| **Build / serving** | Plain wasm, no special headers | `+atomics,+bulk-memory` wasm + COOP/COEP response headers required |

Three things to internalize from that table:

1. **Three data paths, not one.** Opting in splits operations across three paths by
   frequency and shape, and the art is putting each operation on the right one:
   - **SAB + `Atomics` (the hot path)** — high-frequency per-frame value writes from
     the sim/physics thread (transforms, sim uniforms). No serialization; this is
     what makes it smooth.
   - **`postMessage` command channel** — imperative, low-frequency, structured-clone
     operations: loading, edits, spawn/despawn topology. Pays a serialization cost
     (the >1ms-ish thing), but rare, so it doesn't touch the frame budget.
   - **`postMessage` event/query channel** — results flowing back: progress, compile
     status, errors, and async query replies (pick, bounds, screenshot).

2. **SAB is opt-in per buffer, not "everything is shared."** Only the defined
   sim-owned set (transforms + sim uniforms) becomes `SharedArrayBuffer`-backed and
   foreign-writable. Materials, pipeline state, and every GPU handle stay exactly as
   they are. Opting in does not "convert your buffers" — it adds a shared,
   capability-gated region for the hot sim state and leaves the rest alone.

3. **Loading specifically becomes fire-command-and-observe-events.** Today
   `commit_load` is a direct call that blocks whoever calls it. In multithreaded
   mode it is a command to the worker; the worker does the geometry upload / texture
   finalize / pipeline compile off-main and streams discrete progress + status
   events back. That indirection is *why* the loading UI paints its phases for free
   (Layer 1's responsiveness win) — load is no longer call-and-block, it's
   send-a-command + react-to-events.

---

## Readiness: what's specced vs. still open

This doc is **implementation-ready as a direction, not as a turnkey spec.** Before
writing code, know which parts are pinned and which still need decisions.

**Ready to start now:**
- **Platform prerequisites** — the `Send`/`Sync` sweep and the `+atomics`
  shared-memory wasm build are a concrete checklist (see
  [§ Platform prerequisites](#platform-prerequisites)). Independently useful and
  unblocks everything else.
- **Arena + seqlock primitive** — Layer 2 decisions 2 + 4 can be built and
  Rust-unit-tested in isolation (concurrent writer/reader, torn-read detection,
  dirty-version correctness) with no worker plumbing.

**Decided:**
- SAB carries semantic sim state, not GPU-packed bytes
  ([Layer 2 § 1](#1-the-sab-carries-semantic-sim-state-not-gpu-packed-bytes--decided)).
- Single-threaded stays first-class; multithreading is opt-in per app
  ([§ Decided: deployment model](#decided-single-threaded-stays-first-class-multithreading-is-opt-in-per-app)).

**Still open — must be pinned before the dependent layer is coded:**
- **The Layer 1 command/event protocol surface.** Only categories are listed; the
  actual typed commands, events, and async queries must be enumerated. That
  enumeration is the bulk of the worker-hosting work.
- **Scene-graph ownership** for any worker-hosted app: single-owner-in-worker vs a
  main-thread mirror for hit-testing / inspector reads.
- **The Layer 2 sim-state schema:** which buffers beyond transforms join the
  foreign-writable set, and their exact typed layout.

Crucially, **nothing in the "still open" list blocks the editor or model-viewer** —
neither moves off the main thread (see the deployment decision above), so those
open items only gate *games* that opt into worker hosting.

---

## Layer 1 — worker-hosted renderer (OffscreenCanvas)

Host the renderer in a **Web Worker** against an **`OffscreenCanvas`**
(`transferControlToOffscreen`):

- **Main thread** = UI / DOM (dominator), input, app state.
- **Worker thread** = the `AwsmRenderer` + its render loop.

Then a synchronous `commit_load` (or any heavy upload/compile) blocks the
**worker**, not main. The main thread stays free to lay out, paint, and handle
input throughout. Progress/events cross the boundary via `postMessage`, arriving
on the main thread as **discrete event-loop tasks** — so each `on_progress` update
(geometry X/Y → textures X/Y → compiling N) paints **for free**, no library yield,
no `resolve_geometry` change. The granular-loading paint nuance dissolves as a side
effect, and live edits stop janking.

### What already exists (the seed)

- **`packages/examples/render-worker`** — a working example: the page calls
  `transferControlToOffscreen()`, spawns a worker, the worker builds the renderer
  via `AwsmRendererWebGpuBuilder::new_with_offscreen_canvas` and drives its own
  `requestAnimationFrame` loop. A single wasm bundle serves both scopes (selected
  by `is_worker_scope`). It also sketches the input-forwarding wire shape
  (`WorkerInputEvent`).
- **Editor `WorkerPool`** infra (`WorkerPool` / `WorkerPoolBootstrap`, today
  running `GltfParseJob`) — a precedent for spawning + messaging workers from the
  app, and the place the Layer 2 topology command channel can build on.

### What it would take (rough shape)

- Move the `AwsmRenderer` instance into a worker; the main thread holds a thin
  **proxy / command + event protocol** instead of `renderer.lock().await` + direct
  calls.
- Convert every main-thread renderer interaction to message-passing:
  - **Commands → worker**: begin_load / register_geometry / add_mesh / commit_load
    / set_mesh_material / transforms / materials / env, etc.
  - **Events → main**: `LoadingStats` progress, compile status, errors (all small /
    `Copy` → cheap to serialize).
  - **Async round-trips** for anything that's a sync query today: picking,
    scene-capture/screenshot readback, bounds/AABB queries.
- Transfer the `OffscreenCanvas` to the worker; forward pointer / resize / keyboard
  input to it (`WorkerInputEvent`).
- Keep one wasm bundle serving both scopes (as the example does).

---

## Layer 2 — SAB simulation-state sharing

The architecture for a second thread (physics/sim) to manipulate transforms and
uniforms with zero per-frame postMessage. This is the part `docs/ROADMAP.md`
deferred as *"Dynamic/Uniform storages could be SharedArrayBuffer — requires more
design/thought (don't want to expose raw manipulation)."*

Design driver: this is a **general-purpose renderer that must work with lots of
content**. A "cheap hack" (full buffer re-upload every frame, flat per-slot dirty
scan, ad-hoc topology) is explicitly rejected — it doesn't scale to millions of
mostly-static objects. The five decisions below are each forced by that scale
requirement.

### 1. The SAB carries *semantic* sim state, not GPU-packed bytes — DECIDED

The foreign thread writes **world transforms** (and a defined sim-owned uniform
set) into the shared region as semantic values. The render thread keeps the cheap
pack step (world matrix → model + inverse-transpose normal matrix — the existing
`transforms.rs` packing) during its dirty descent.

Chosen over having the foreign thread write final packed bytes because it:
- keeps a clean typed contract and **never exposes the raw GPU byte layout** (the
  ROADMAP's stated concern) — you expose typed write-capabilities to specific
  slots, not raw buffers;
- moves the *expensive* work off the render thread (transform-hierarchy walk +
  physics integration) while keeping the *trivial* work (pack + upload) where the
  GPU device lives — for sim-owned nodes the render thread can skip its own
  `update_inner_recursively` walk entirely;
- keeps render-thread pack work proportional to dirty count — the same shape as
  today, just sourced from another thread.

Accepted trade-off: slightly more render-thread CPU per dirty slot than a
pre-packed contract. Implications now fixed:
- The SAB schema is a typed sim-state layout (e.g. world `Mat4` per slot), **not**
  the `transforms.rs` packed layout; the packed layout stays render-thread-private.
- The render thread's dirty descent (decision 4) does the pack inline as it walks
  dirtied slots, then hands `(offset, len)` ranges to the existing uploader.
- The sim thread never needs to know about normal matrices or GPU alignment.

### 2. Stable addressing — growth must never move existing data

Today the backing is one growable `Vec<u8>`; `resize()` reallocs and the base
pointer moves — fatal for a foreign writer holding an offset. Replace with a
**chunked arena**: fixed-size SAB segments, slots never move once assigned, growth
= append a new chunk, existing chunks never realloc. A slot→(chunk, offset) binding
is then valid forever. (Slot *indices* are already stable across the current
free-list resize in `dynamic_uniform.rs`; this makes the *addresses* stable too,
which SAB requires.)

### 3. Topology is an owner-thread transaction; foreign threads only write values

Allocate / free / resize / buddy-alloc (`DynamicStorageBuffer`) reassign or move
structure — they stay owner-side, behind a command channel. A physics body
requests a slot binding at spawn (one command-channel round-trip, *not* per frame),
then writes that fixed slot lock-free every frame. Fits the existing "loading is
ONE transaction" law: spawn/despawn is a transaction; steady-state motion is not.
The hot path touches zero topology.

This is the value-vs-topology split of `dynamic_uniform.rs`'s `update_with`, which
today couples three mutations: (a) value bytes, (b) `mark_dirty_range`, (c)
slot/free-list/`resize` allocation. Foreign threads get (a)+(b); the owner keeps
(c).

### 4. Dirty + publication are one mechanism; dirty collection scales with *changes*, not content

- **Per-slot version = seqlock.** Writer bumps odd → write bytes → bump even, with
  release/acquire fences. Render reads "version ≠ last-seen" = dirty; "odd or
  unstable across the read" = torn → reuse last frame's value for that slot
  (one-frame sub-frame staleness, self-heals). One atomic per slot solves tearing
  **and** dirty together.
- **Coarse chunk-level dirty bitmap** above the per-slot versions: a writer sets
  its chunk's bit (one extra atomic-or) when it dirties a slot. Render descends
  only into chunks whose coarse bit is set, so scan cost ∝ touched chunks, **not
  total slot count** — the dirty-pages trick. A million-object scene with 200
  movers costs ~200-movers of work, not a million-slot scan. Overflow-free (unlike
  a fixed dirty-index ring) and coalesces into the existing `(offset, len)` ranges.

### 5. The downstream GPU path is untouched

The render thread turns descended dirty slots into the same `(offset, len)` ranges
that `MappedUploader::write_dirty_ranges` / `mapped_staging_ring` already consume.
Only the *front* of the pipe changes (where bytes live + where dirty originates);
the staging-ring upload and bind-group reuse are unchanged. This is the evidence
it's an evolution of `DynamicUniformBuffer`/`DynamicStorageBuffer`, not a rewrite.

### Generality

Transforms (fixed-slot uniform), instances/storage (buddy-allocated, variable
extent), lights, materials — all sit on the same arena + version + tiered-dirty
substrate. Variable-length buffers obey the same rule: rewriting bytes of an
already-allocated fixed-extent region is a value write (foreign-allowed); changing
the *extent* is topology (owner-only, command channel). SAB-backing is **opt-in
per buffer** — only a defined sim-owned set (transforms + sim uniforms) is
foreign-writable; materials, pipeline state, and all GPU handles stay
render-thread-private.

---

## Platform prerequisites

The renderer is structurally single-threaded today and silently relies on "one
thread, ever" in a handful of places. None are deep architectural lock-in, but all
must change before any worker arrangement is meaningful. (This section is the
former `docs/multithreading-prep.md` audit; the scheduler work landed in
[PR #99](https://github.com/dakom/awsm-renderer/pull/99).)

### Today's threading model

wasm32-unknown-unknown's single-threaded JS event loop. No `Send`/`Sync` needed
because nothing crosses a thread boundary; `wasm_bindgen_futures::spawn_local`
queues onto the same microtask queue as the rAF tick. `pipeline_scheduler` relies
on this via: a non-`Send` `FuturesUnordered<PendingFuture>` (captures `!Send`
`JsFuture` Dawn promises); `Mutex<Option<HashSet>>` once-per-session guards;
`&mut self` `SlotMap`/`HashMap` access; `std::mem::take` event drains.

### What changes when wasm32-multithread lands

The render loop stays single-threaded (WebGPU's command-encoder lifetime is
per-thread), but a worker pool could drive compiles concurrently. Boundaries
`pipeline_scheduler` would negotiate:

1. **`PendingFuture` `Send` requirement.** Dawn pipeline-creation promises return
   `!Send` `GpuComputePipeline`/`GpuRenderPipeline`. Cleanest path: the
   compile-orchestrator stays on the main/render thread (where the device lives);
   the *frontend* worker uses `submit_pipeline_group_batch` via a message-passing
   bridge, so only work-orchestration parallelizes.
2. **`SlotMap` + `HashMap` access.** Swap bare collections for
   `parking_lot::Mutex<…>` (zero overhead uncontended; `RwLock` doesn't help —
   `SlotMap::insert` needs exclusive). Per-pass `generation` markers already keep
   the lock-window small.
3. **`Vec<StatusEvent>` drain.** Replace with an unbounded channel so multiple
   worker contexts can emit; the drain side stays single-consumer on the render
   thread, preserving the between-frames coalescing `poll_resolved` relies on.
4. **`warn_pipeline_not_compiled` guard.** `Mutex<Option<HashSet>>` →
   `parking_lot::Mutex<HashSet>` / `DashSet`; keep it a single global guard (a
   multithreaded app does not want N redundant warn lines per failure).
5. **Frontend `drain_pipeline_status_events` subscriber.** If a worker subscribes,
   forward serialized events across the boundary. `PipelineGroupId` is
   `Copy + Hash + Eq` so trivially serializable; the only `!Serializable` payload
   is `PipelineGroupStatus::Failed { error: AwsmError }` — carry a tagged string,
   not the `AwsmError`.

### Editor-frontend invariants

- **`Rc` + `RefCell`.** `RendererHandle = Rc<RefCell<…>>` (and scene-editor's
  `renderer_bridge`) is the load-bearing single-thread assumption. Migrate to
  `Arc<Mutex<…>>`; audit every long `borrow_mut()` across an `.await` for places
  that should become `try_lock()` (the rAF loop already skips a frame when the host
  is busy with `prewarm_pipelines` — the same shape works under a `Mutex`). The
  `#[allow(clippy::await_holding_refcell_ref)]` comments mark the high-risk sites.
- **`Mutable<…>` is not `Send`.** All `EditState` is `Arc<Mutable<…>>`, `Send` only
  via `Arc`; the inner `Mutable` still needs cross-thread synchronization.
  Either build a `SendMutable` over `futures_signals` (Arc + RwLock), or per-thread
  `tokio::sync::watch` channels with the renderer thread holding one end. This is a
  UI-architecture-scale change, not a per-call patch.

### The multi-thread-ready template

`CoverageReadbackState` and `EdgeOverflowReadbackState` already use
`Arc<Mutex<…>>` with a small lock surface, no nested locks, write-through-Arc —
forward-compatible out of the box (their `mapAsync` resolution runs in a detached
`spawn_local`). Use them as the canonical "this is what right looks like" when
refactoring other state.

### Boundaries that won't move

- **WebGPU command-encoder ownership.** `GpuCommandEncoder.beginRenderPass(…)`
  returns a pass encoder bound to the encoder; both `!Send`. Render passes run on
  the thread holding the encoder. Multi-encoder render-frame parallelism is
  possible but an order of magnitude beyond "use `Arc<Mutex>`".
- **`web_sys::*` JsValue ownership.** Every handle (`GpuBuffer`, `GpuTexture`,
  `GpuBindGroup`, …) is `JsValue` underneath and `!Send`. `Materials`, `Meshes`,
  `Lights`, `Shadows`, etc. are therefore `!Send` and stay on the render thread.
  CPU-side ECS / spatial / **physics** work runs on other threads; the bridge to
  the renderer is the boundary — which is exactly what Layer 2's SAB arena is.

### Migration checklist

- [ ] Audit `Rc` → `Arc` in the editor frontends (~50 sites, clippy-driven).
- [ ] Audit `RefCell` → `Mutex`; migrate `borrow_mut()`-across-`.await` to
      `try_lock()` (see the `#[allow]` comments for high-risk sites).
- [ ] Decide a `SendMutable` story (wrap `futures_signals::Mutable`, or a signal
      lib that ships `Send + Sync`).
- [ ] Single-thread island for `PipelineScheduler` (message-passing front so it can
      stay `!Send` while the rest goes multi-thread).
- [ ] Scheduler internal locks (the changes under "What changes…" above).
- [ ] Verify `CoverageReadbackState` / `EdgeOverflowReadbackState` need zero changes
      (use as the reference template).
- [ ] CI build under `--cfg=web_sys_unstable_apis` + nightly with
      `+atomics,+bulk-memory` + shared memory + COOP/COEP headers. Confirm the
      pinned `wasm-bindgen = 0.2.118` supports `SharedArrayBuffer` on the target
      before any of the above.

### What you do NOT have to migrate

- The visibility buffer + compute kernels — GPU-side, thread-irrelevant.
- The `Materials`/`Meshes`/`Lights`/etc. GPU-resource structs — stay render-thread.
- The `FuturesUnordered` patterns themselves — fine single-threaded; only their
  `!Send` inner future types are the constraint.

---

## Cost / sequencing

Multi-PR, not a feature flag. Suggested order (each layer independently
verifiable):

1. **Platform prerequisites** — the `Send`/`Sync` sweep + the `+atomics`
   shared-memory wasm build + COOP/COEP. Unblocks *everything else* and is
   independently useful. Start here; nothing else can run without it.
2. **Arena + seqlock primitive in isolation** — re-base `DynamicUniformBuffer`
   (then `DynamicStorageBuffer`) onto the chunked stable-address SAB arena with
   per-slot version/seqlock + tiered chunk-dirty layer. Rust-unit-testable with no
   worker plumbing (concurrent writer/reader simulation).
3. **Worker-hosted renderer (Layer 1)** — promote the OffscreenCanvas example to a
   supported game deployment mode; build the command/event protocol.
4. **Physics in a second worker (Layer 2 end-state)** — topology command channel +
   the typed sim-state schema + the opt-in foreign-writable buffer set; physics
   writes world transforms, render packs + uploads.

---

## Open questions / risks

- **Editor coupling (only if the editor is ever moved to a worker — out of scope
  per the deployment decision).** The controller + many bridges call
  `renderer_handle().lock().await` directly and pass closures touching main-thread
  UI state in the same scope. Since the renderer is `!Send`, moving it to a worker
  would turn that whole surface into an async message protocol — the bulk of the
  work and the main risk. The plan keeps the editor main-thread precisely to avoid
  this; it is documented here only so the cost is explicit if that ever changes.
  The same applies to the two items below — they bite *only* a worker-hosted app
  (a game), never the main-thread editor/model-viewer.
- **Scene-graph ownership.** Single-owner-in-worker vs a mirrored copy on main (for
  hit-testing / inspector reads) — and how an app's authored scene relates to the
  worker's renderer scene.
- **Interactive-query latency.** Picking / gizmo hit-tests become a main↔worker
  round-trip; may need a main-thread spatial mirror for instant hit-testing, or to
  accept a frame of latency.
- **Bundle / wasm-bindgen.** Single bundle, two scopes (example pattern); watch
  module-init duplication, and confirm `wasm-bindgen 0.2.118` SAB support.
- **Sim-state schema scope.** Which buffers join the foreign-writable sim-owned set
  beyond transforms (which uniforms? skins? morph weights?) — opt-in, decided per
  buffer.

---

## Relationship to other plans

Orthogonal to the "one geometry flow" epic (`docs/plans/todo.md`) — that
consolidates *what* geometry is and how it's authored; this moves *where the
renderer runs* and *who may write its sim state*. They don't depend on each other,
but both serve the same "one obvious way, optimised + debugged in one place" goal.

## Reference files

- `docs/DEPLOYMENT_MODES.md` — main-thread vs. OffscreenCanvas worker modes.
- `docs/ROADMAP.md` (§ Multithreading) — the deferred SAB item this plan resolves.
- `packages/examples/render-worker/` — working OffscreenCanvas worker PoC.
- `packages/crates/renderer/src/buffer/dynamic_uniform.rs` — `update_with` showing
  the value/dirty/allocation coupling that Layer 2 decision 3 splits.
- `packages/crates/renderer/src/buffer/dynamic_storage.rs` — buddy-allocated
  variable-extent buffer; same value-vs-topology split.
- `packages/crates/renderer/src/transforms.rs` — the world-matrix pack + dirty
  upload; where the render-thread pack step lands.
- `packages/crates/renderer/src/buffer/mapped_uploader.rs`,
  `.../mapped_staging_ring.rs` — the untouched downstream upload path (decision 5).
- `packages/crates/renderer/src/pipeline_scheduler/mod.rs` — single-thread
  invariants documented inline (Platform prerequisites).
- `CoverageReadbackState` / `EdgeOverflowReadbackState` in
  `packages/crates/renderer/src/lib.rs` — the multi-thread-ready template.
- `packages/crates/renderer/src/workers/{pool,blob}.rs` — existing CPU-only
  WorkerPool infra to build the command channel on.
- `packages/frontend/editor/src/engine/context.rs` — the `Rc<RefCell<…>>` renderer
  handle + clippy escape (editor-frontend migration).
