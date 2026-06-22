# Multithreaded renderer — deferred **build** items

Code work deferred from the Phase 2 hardening. These are *not* testing (that
lives in `docs/plans/multithread-testing.md`) — they are renderer/protocol
features still to be written. Architecture + the landed work: `PLAYER-GUIDE`
§9.

> Not here: the **game-API parity** work (load an exported scene in the worker +
> runtime ops) — that is being built directly, not deferred.

## B1 — Device-loss + worker-crash recovery (resilience)
Production sessions must survive a lost GPU device and a dead worker. The arena
+ the renderer's CPU mirrors are already the source of truth, so recovery is
largely "rebuild the GPU side from data we still hold."

- **GPU device loss.** Subscribe to `GPUDevice.lost`. On loss: request a fresh
  adapter/device, recreate the surface config + all GPU buffers/textures/
  pipelines, and re-upload from the existing CPU mirrors (transforms buffer,
  instance arenas, materials, mesh geometry). The scene graph / arena state in
  shared memory is untouched — only `web_sys` GPU handles are rebuilt.
  - Open question: how much of the renderer's GPU-handle graph can be rebuilt
    behind a single `renderer.rebuild_gpu()` vs. needs a full re-`commit_load`.
- **Worker crash.** Main thread watches `worker.onerror` / a heartbeat. On death:
  respawn the worker from the same bootstrap, re-transfer a fresh
  `OffscreenCanvas` (the old one is gone with the worker), re-post the shared
  module+memory, and re-hand every live arena `SlotBinding` (the sim worker's
  bindings are addresses into shared memory that *survived* — but the render
  worker that owned topology did not, so topology must be re-derived; design
  whether the sim worker or a persisted manifest re-seeds it).
  - Open question: can the render worker's topology (slot→key map) live in shared
    memory so a respawn recovers it, rather than reloading the scene?
- **Acceptance:** T4 in the testing doc (force device loss / kill the worker →
  renderer continues).

## B2 — Screenshot capture path (the platform-bounded gap)
`OffscreenCanvas.convertToBlob` is rejected by Chrome on a WebGPU canvas
(`NotReadableError` — swapchain not host-readable post-present; measured in H7).
A robust capture needs renderer support:

- Render (or blit) the final frame into an explicit color target created with
  `COPY_SRC`, then `copyTextureToBuffer` → `mapAsync` → read the bytes →
  (optionally PNG-encode) → return over the protocol's `Screenshot` →
  `ScreenshotBytes` (+ Transferable buffer, already wired).
- Expose this as a renderer API (`renderer.capture_frame() -> Vec<u8>`) so both
  the single-threaded model-viewer and the worker path share it.
- Watch row-stride alignment (`bytesPerRow` 256-byte multiple) on readback.
- **Acceptance:** `?demo=remote` `Screenshot` returns non-empty bytes whose
  decoded image matches the on-screen frame.

## B4 — Migrate the stale `assets/world/*` scene fixtures
The bundled sample scenes (`assets/world/*/project.json`) are a **stale
pre-refactor format**: they use a `primitive` node kind that no longer exists
(the current schema is `Mesh { mesh: MeshRef }` with a primitive *base* in the
mesh asset's modifier stack). Neither the current `EditorProject` nor `Scene`
deserializes them, and `project_to_scene` can't bake them. `?demo=scene` works
around this by building an equivalent `Scene` in code.

- Either re-export these fixtures from the current editor, or write a one-shot
  migration (old-format → current `EditorProject`), then bundle a real exported
  scene so `?demo=scene` loads from a file (closer to the shipped path) and the
  fixtures are usable by tests/tools again.
- **Acceptance:** `?demo=scene` deserializes a bundled scene file + `project_to_scene`
  + `load_scene_for_player` with no in-code scene construction.

## B3 — (conditional) Arena growth policy
Only if T3's churn soak shows shared-memory growth is *unbounded*: add slab
reuse / compaction so long spawn/despawn sessions keep memory flat. Default
assumption is that free-slot reuse already bounds it — confirm in T3 first.
