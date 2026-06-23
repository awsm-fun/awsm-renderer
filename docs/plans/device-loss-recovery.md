# Device-loss + worker-crash recovery (B1) + verification (T4)

> **Status: NOT STARTED — standalone pickup doc.** This is the one remaining item
> from `docs/plans/todo.md` (everything else shipped in PR #138). It was
> deliberately deferred to its own focused session: it's greenfield error-recovery
> code with two real design forks, and a half-implementation is worse than none.
>
> Branch: `more-mcp` (or a fresh branch off it). Target: Chrome desktop. Verify
> via the chrome-devtools MCP against `task mt:dev` (`:9090`) — see
> `docs/plans/todo.md` § Dev environment for the build/restart cycle.

Production sessions must survive a lost GPU device and a dead render worker. The
shared-memory **arena + the renderer's CPU mirrors are already the source of
truth**, so recovery is largely "rebuild the GPU side from data we still hold" —
the scene graph / arena state in shared memory is untouched; only `web_sys` GPU
handles (and, for a worker crash, the worker itself) are gone.

Architecture + the landed multithread work: `docs/PLAYER-GUIDE.md` §9.

## What exists today (starting point)
- **No `GPUDevice.lost` subscription** anywhere. Confirmed: the only `recover_*`
  in the renderer is `buffer/mapped_staging_ring.rs` (mapAsync slot recovery —
  unrelated). There is **no `renderer.rebuild_gpu()`**.
- **No render-worker respawn.** `workers/pool.rs` `onerror` only fails the
  in-flight *meshgen* job; it is NOT the render worker, and it does not respawn.
- The CPU mirrors that a rebuild would re-upload from already exist: the
  transforms buffer, instance arenas, materials, and mesh geometry are all held
  CPU-side (that's what every per-frame upload reads from).
- The multithreaded example (`examples/multithreaded/`) is where this is driven +
  verified: `remote_demo.rs` (the Layer-1 protocol worker) is the natural host.

## B1a — GPU device-loss recovery
Subscribe to `GPUDevice.lost`. On loss: request a fresh adapter/device, recreate
the surface config + all GPU buffers/textures/pipelines/bind-groups, and re-upload
from the existing CPU mirrors. The arena/scene state in shared memory is untouched.

**Design fork (decide first):**
- **(A) `renderer.rebuild_gpu()` from CPU mirrors** — recreate every GPU handle in
  place + re-upload. Faster recovery; the cost is making *every* GPU-handle
  subsystem (render passes, pipeline pools, bind groups, texture pool, buffers)
  rebuildable behind one entry point.
- **(B) Full re-`commit_load`** — tear down + re-run the existing `commit_load`
  path against the retained scene/arena state. Reuses the proven load path (less
  new code); heavier recovery + needs the scene retained in a re-loadable form.

(David was asked this fork and deferred the whole item to this session — pick the
one that keeps the GPU-handle graph maintainable; lean (A) if the per-subsystem
`rebuild` surface stays small, else (B).)

## B1b — Worker-crash recovery
Main thread watches `worker.onerror` / a heartbeat. On death: respawn the worker
from the same bootstrap, re-transfer a **fresh** `OffscreenCanvas` (the old one
died with the worker), re-post the shared module + memory, and re-hand every live
arena `SlotBinding`. The sim worker's bindings are addresses into shared memory
that *survived* — but the render worker that owned the topology did not, so the
**slot→key topology must be re-derived**.

**Design fork (decide first):**
- **Persist topology in shared memory** — the render worker writes its slot→key
  map into shared `WebAssembly.Memory` so a respawn reads it back + rebinds without
  reloading the scene. Fast respawn; needs the topology laid out in shared memory.
- **Re-seed from a manifest / reload** — respawn reloads the scene (or a persisted
  manifest) to re-derive topology. Simpler, reuses the load path; slower respawn +
  needs the scene re-loadable.

## T4 — Resilience verification (the acceptance gates)
Run all via the chrome-devtools MCP against `task mt:dev`:
1. **Force `GPUDevice` loss** mid-session (`device.destroy()` via devtools/script) →
   the renderer rebuilds and keeps rendering (screenshot before/after match).
2. **Kill the render worker** mid-session → main thread respawns it, re-hands the
   `OffscreenCanvas`, re-establishes arena bindings, scene is intact (the demo
   keeps moving — e.g. `?demo=motion` movers resume).
3. **Asset-fetch failure** during a scene load → clean `Error` event, no hang
   (this third gate is small + independent of B1 — it can be done first as a warm-up).

## Notes / gotchas carried from the custom-vertex + multithread work
- `task mt:dev` is the threaded build (`wasm32-unknown-unknown` + build-std +
  atomics, COOP/COEP). Free `:9090` before relaunch; `run_in_background: true`.
- The harness may not auto-register the awsm-scene MCP tools; if so, drive that
  MCP via `/tmp/mcp.py` (see memory `mcp-direct-http-client`). For these demos you
  mostly drive `http://localhost:9090/?demo=<name>` + `evaluate_script` directly.
- Per-frame allocation standard (David's): any recovery hot-path additions must not
  add per-frame `Vec`/`Box`; existence checks should be cheap (cf. the
  `has_vertex_shader` pattern added in PR #138).
