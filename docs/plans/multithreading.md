# Plan (idea): worker-hosted renderer — main-thread responsiveness

**Status: idea / motivated, not scheduled.** Captured from a design discussion; this is a direction,
not a committed plan. It is deliberately kept OUT of `docs/plans/todo.md` — the renderer library should
NOT add asynchronous jank (mid-operation yields) to work around an application threading choice. The
right fix is architectural: move the renderer off the main thread.

## The problem this solves

Both shipping apps run the `AwsmRenderer` **on the main thread**:

- model-tests: `renderer: Arc<futures::lock::Mutex<AwsmRenderer>>`
- editor: `RendererHandle = Arc<xutex::AsyncMutex<AwsmRenderer>>` (its `WorkerPool` runs only
  `GltfParseJob` — parallel glTF *parsing* — not the renderer)

So any heavy *synchronous* renderer work blocks the main thread's layout/paint and input. Two concrete
symptoms:

1. **The loading UI can't paint its fast phases.** `commit_load`'s geometry phase (`resolve_geometry`)
   is synchronous — it sets `on_progress(UploadingGeometry X/Y)` then runs straight into the texture
   phase without yielding, so the browser never gets a frame to paint that line (the DOM is retained-
   mode: imperative `set_text_content` updates the tree synchronously but the *screen paint* still only
   runs when the task yields — so this is NOT a dominator/reactive issue and NOT fixable app-side). Only
   the phases that `.await` (texture finalize, pipeline compile) can paint, and only when the work spans
   real frames (cold compile → "Compiling pipelines (N)" shows; warm cache → nothing). 20× CPU throttle
   does not help — the blocker is *yielding*, not speed.
2. **Live editor edits jank the UI.** Every material/geometry/texture edit runs `commit_load` on the
   main thread, stalling input + paint for its duration.

The tempting-but-wrong fix is to make `commit_load` yield to the event loop after each phase. That adds
~1 event-loop turn per phase (`setTimeout(0)` ≈ 4ms clamped, rAF ≈ 16ms) to **every** commit — including
every live edit — purely so a cosmetic progress line can paint, and it relaxes the deliberately-atomic
"commit_load stays identical" invariant. The library has no reason to add that latency for an
application's threading decision.

## The idea

Host the renderer in a **Web Worker** against an **`OffscreenCanvas`** (`transferControlToOffscreen`):

- **Main thread** = UI / DOM (dominator), input, app state.
- **Worker thread** = the `AwsmRenderer` + its render loop.

Then a synchronous `commit_load` (or any heavy upload/compile) blocks the **worker**, not main. The main
thread stays free to lay out, paint, and handle input throughout. Progress/events cross the boundary via
`postMessage`, arriving on the main thread as **discrete event-loop tasks** — so each `on_progress`
update (geometry X/Y → textures X/Y → compiling N) paints **for free**, no library yield, no
`resolve_geometry` change. The granular-loading paint nuance dissolves as a side effect, and live edits
stop janking.

## What already exists (the seed)

- **`packages/examples/render-worker`** — a working example: the page calls
  `transferControlToOffscreen()`, spawns a worker, the worker builds the renderer via
  `AwsmRendererWebGpuBuilder::new_with_offscreen_canvas` and drives its own `requestAnimationFrame` loop.
  A single wasm bundle serves both scopes (selected by `is_worker_scope`). It also sketches the
  input-forwarding wire shape (`WorkerInputEvent`).
- **Editor `WorkerPool`** infra (`WorkerPool` / `WorkerPoolBootstrap`, today running `GltfParseJob`) — a
  precedent for spawning + messaging workers from the app.

## What it would take (rough shape)

- Move the `AwsmRenderer` instance into a worker; the main thread holds a thin **proxy / command +
  event protocol** instead of `renderer.lock().await` + direct calls.
- Convert every main-thread renderer interaction to message-passing:
  - **Commands → worker**: begin_load / register_geometry / add_mesh / commit_load / set_mesh_material /
    transforms / materials / env, etc.
  - **Events → main**: `LoadingStats` progress, compile status, errors (all small / `Copy` → cheap to
    serialize).
  - **Async round-trips** for anything that's a sync query today: picking, scene-capture/screenshot
    readback, bounds/AABB queries.
- Transfer the `OffscreenCanvas` to the worker; forward pointer / resize / keyboard input to it
  (`WorkerInputEvent`).
- Keep one wasm bundle serving both scopes (as the example does).

## Open questions / risks

- **Editor coupling.** The controller + many bridges call `renderer_handle().lock().await` directly and
  pass closures that touch main-thread UI state. That whole surface becomes an async message protocol —
  the bulk of the work, and the main risk.
- **Scene-graph ownership.** Decide single-owner-in-worker vs a mirrored copy on main (for hit-testing /
  inspector reads) — and how the editor's authored scene relates to the worker's renderer scene.
- **Interactive-query latency.** Picking / gizmo hit-tests become a main↔worker round-trip; may need a
  main-thread spatial mirror for instant hit-testing, or to accept a frame of latency.
- **Bundle / wasm-bindgen.** Single bundle, two scopes (example pattern); watch module init duplication.
- **Shared memory.** postMessage-only avoids `SharedArrayBuffer` + COOP/COEP headers; only reach for
  shared memory if profiling demands it.

## Payoff

- Main thread never janks on heavy renderer work (loads, big uploads, compiles, live edits).
- The granular-loading paint nuance is gone for free (per-phase `postMessage` = per-phase main-thread
  paint).
- A foundation for further parallelism.

## Relationship to other plans

Orthogonal to the "one geometry flow" epic (`docs/plans/todo.md`) — that consolidates *what* geometry is
and how it's authored; this moves *where the renderer runs*. They don't depend on each other, but both
serve the same "one obvious way, optimised + debugged in one place" goal.
