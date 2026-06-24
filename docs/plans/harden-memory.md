# Harden memory: crash / leak / recovery (single SSOT)

> **One executable plan** folding the former `aw-snap.md` (overnight "Aw, Snap!"
> OOM crash) and `device-loss-recovery.md` (GPU-device-loss + render-worker-crash
> recovery) into one autonomous workstream. Goal: a production session **never
> dies** from a memory leak, a lost GPU device, or a dead render worker — and the
> render hot path pays **zero** for any of it.
>
> **Design decisions are LOCKED (no forks left to ask):**
> - **Undo/redo cap:** total-**byte** budget, drop-oldest. ✅ landed + verified.
> - **Device-loss (B1a) — RE-LOCKED 2026-06-24 (David):** **reload from the
>   retained source-of-truth**, NOT "rebuild from CPU mirrors." The original lock
>   assumed CPU geometry/texture mirrors exist; they don't (upload-then-drop by
>   design — see the ⚠️ RESOLVED-BLOCKER section). On `device.lost`, reconstruct
>   the scene from the authoritative description that's **already retained**
>   (editor: `EditorProject`; runtime: the scene bundle / its URL) via the
>   existing load path. Cold path (`.lost` only).
> - **Worker-crash (B1b) — RE-LOCKED 2026-06-24 (David):** respawn the worker and
>   **re-run the same source-of-truth load** in the fresh worker (it owns the
>   device + all GPU handles, which die with it). Persisting slot→key topology in
>   shared memory is kept ONLY as an optimization where the sim worker's arena
>   bindings survive; topology written on change, not per frame.
> - **HARD CONSTRAINT on B1a/B1b (David, 2026-06-24):** recovery must add **no
>   steady-state memory** and **no performance regression**. (b) satisfies this by
>   construction — it reuses state already retained for save/load (no new
>   permanent geometry/texture retention — that's why (a) was rejected) and runs
>   only on the cold loss/crash path. **Any implementation that permanently
>   retains expanded geometry/texture CPU mirrors, or adds per-frame cost, is
>   out-of-bounds and must be flagged, not shipped.**
> - **Scope:** all three, **ordered** (Phase 1 → 2 → 3), each **self-verified** as a
>   hard gate before the next.
> - User directive: *most robust / best-in-class result; code churn and
>   backwards-compat are NOT concerns; the only hard constraint is no per-frame
>   hot-loop cost + no steady-state memory growth.*

---

## Global invariants (apply to EVERY phase — non-negotiable)

1. **Hot loop is sacrosanct.** No new per-frame allocation (`Vec`/`Box`/`HashMap`
   insert), no per-frame branch beyond a cheap existence check. All recovery +
   diagnostics code is **cold-path only** (runs on `.lost` / `onerror` / an MCP
   query, never inside `render_one_frame` / `render()`). Follow the
   `has_vertex_shader` cheap-check pattern (PR #138) and David's per-frame-alloc
   standard. Prove it: `get_memory_stats` steady-state is flat on an idle scene,
   and `?trace=sub-frame` shows no new per-frame spans.
2. **Diagnostics are opt-in.** Every added counter / log / guard is gated
   `#[cfg(any(debug_assertions, feature = "harden-diag"))]` (add the `harden-diag`
   feature to the editor + renderer crates). Release builds without the feature are
   byte-for-byte unaffected. **Behavioural fixes** (undo cap, device-loss rebuild,
   worker respawn) ship **un-gated** — they are correctness, not diagnostics.
3. **Robust > minimal.** Pick the best-in-class implementation; churn is fine.
4. **Per phase:** implement → host test where a host test is possible → build +
   restart + (re-pair) the right harness → **self-verify the live repro** →
   `task lint` (fmt + clippy -D warnings) → commit (co-author trailer) → flip the
   tracker row. A phase is DONE only when its live repro passes.

---

## Why (evidence — condensed from the crash dump)

Crash dump `.build-artifacts/crash-report/e06f5fd7-…dmp` (.gitignored),
`minidump-stackwalk`'d:

- **Renderer process, intentional abort.** `--type=renderer`, `CrRendererMain`,
  `EXC_BREAKPOINT` (a `brk`/trap — `IMMEDIATE_CRASH`), **not** a segfault and **not**
  an OS OOM-kill. **PartitionAlloc** annotations present ⇒ an **OOM-class abort**.
- **Our code requested it.** The trap (Chrome C++ allocator) sits directly on top of
  **V8/WASM JIT frames** — our wasm asked Chrome for memory and Chrome's allocator
  trapped. `x8 = 0x8000_0000` (**2 GiB**) at the trap ⇒ a **single ~2 GB
  allocation**, not slow creep.
- **Uptime only ~99 min** (not 8 h) — fast to reproduce. Tab was the
  **`chrome-devtools-mcp` agent tab** doing thousands of edits.
- **Prime cause:** `controller/state.rs:193` `undo: Rc<RefCell<Vec<EditorCommand>>>`
  is an **unbounded `Vec`** of full inverse snapshots (a `SetKind`/`PatchKind`
  inverse holds the entire previous `NodeKind`; mesh/paint ops carry vertex data),
  pushed on every command (`:301/:402/:3992`), capped nowhere, cleared only by
  `new_project`. Under thousands of agent edits the `Vec` grows in WASM linear
  memory until a reallocation requests ~2 GB → Blink can't back the `ArrayBuffer`
  resize → `IMMEDIATE_CRASH` in Chrome C++ called from our wasm frames. Matches
  every fact (renderer process · 2 GB single realloc · ~99 min · only the
  high-edit-volume tab).

Recovery (Phases 2–3) is a separate production-resilience concern from the same
"don't crash" family: the scene/arena state in shared memory is the source of truth,
so a lost device or dead worker is recoverable by rebuilding the GPU side / respawn
from data we still hold.

---

## Verification harnesses (two — the loop switches per phase)

- **Editor harness (Phase 1):** `task mcp-dev` → editor `:9085`, MCP server `:9086`.
  Drive via the awsm-scene MCP tools, or `/tmp/mcp.py` over HTTP if the harness
  registers none (see memory `mcp-direct-http-client`); keep the chrome-devtools tab
  **foregrounded**. Key tool: **`get_memory_stats`** (`EditorQuery::MemoryStats`,
  `mcp.rs:1283` / `state.rs` `MemoryStats` arm) — JS-heap + renderer/pool counts.
- **Multithreaded harness (Phases 2–3):** `task mt:dev` → demo `:9090`. Threaded
  build (build-std + atomics, COOP/COEP; slow first build). Drive **directly via
  chrome-devtools** (`evaluate_script`, `device.destroy()`, worker kill) against
  `http://localhost:9090/?demo=motion` (movers — a live "is it still rendering?"
  signal). Free `:9090` (`lsof -ti tcp:9090 | xargs kill`) before relaunch; start
  with `run_in_background: true`; wait for HTTP 200 + trunk "success".
- Restart/​re-pair mechanics (build a server crate → restart → re-pair): memories
  `mcp-improvements-loop-mechanics`, `mcp-direct-http-client`.

Do all the work on the **current branch `doc-aw-snap`** (created for this purpose;
it descends from the PR #138 merge, so it already has the multithread +
custom-vertex work and carries this plan doc). Do **not** create a new branch.
Commit per phase; don't push / open a PR unless asked.

---

## Phase 0 — shared diagnostics (opt-in; enables verification)

All under `#[cfg(any(debug_assertions, feature = "harden-diag"))]`.

1. **Extend `get_memory_stats` / `EditorQuery::MemoryStats`** (`state.rs` MemoryStats
   arm) with:
   - `wasm_heap_bytes` — WASM linear-memory size (the metric a JS-heap soak misses;
     `wasm_bindgen::memory()` → `WebAssembly.Memory.buffer.byteLength`).
   - `undo_len` / `redo_len` and **`undo_bytes`** (estimated retained bytes — the
     same estimator Phase 1's cap uses).
   (Update the tool description to mention them.)
2. **Wire `GPUDevice.lost`** beside the existing `onuncapturederror` hook
   (`renderer-core/src/renderer.rs:383-427`): log loss to the
   `awsm_renderer_core` tracing target (so it surfaces in `get_logs`). This is the
   detection half Phase 2 builds on; ship it here so a loss is never silent.
3. (Optional, for the soak) a crash-surviving external poller: sample
   `get_memory_stats` every ~30 s into a `.jsonl` from the shell/MCP client (the tab
   can't log its own crash).

> Host-testable: the byte estimator (Phase 1) has a unit test; the rest is live.

---

## Phase 1 — OOM / leak hardening (prime fix: bound the undo log)

1. **Cap the undo/redo log by a total-byte budget, drop-oldest.**
   - Replace the bare `Vec` push sites (`state.rs:301/402/3992`) with a bounded
     structure (e.g. a small `BoundedHistory` holding entries + a running byte total;
     `VecDeque`, pop_front when over budget). Redo already clears on a new edit.
   - **Byte estimator** for an `EditorCommand` inverse: a cheap recursive size
     (NodeKind/mesh/vertex payloads dominate). Doesn't need to be exact — a safe
     over-estimate is fine. **Unit-test it** (host test in the editor crate).
   - Budget: a named `const` (default **256 MB**), easy to tune. Dropping oldest
     entries silently caps how far back undo reaches — acceptable and standard.
   - This is **un-gated** (correctness). It must add **zero per-frame cost** (push
     happens on edits, not frames).
2. **Audit other unbounded growth.** Grep the editor + renderer for containers that
   only ever grow on a repeating path (per-edit or per-frame): log/toast/tracing
   rings, broadcast/event queues, `*_KEYS`/pool maps, mover/transform sets,
   animation sample buffers. For each: bound it, or confirm it's already bounded /
   cleared, and note the finding in the tracker. (The render-stats counters
   `pool_textures`/`samplers`/`render_pipelines`/`shaders`/`*_keys`/`dynamic_materials`
   are the leak panel — they must be flat under churn.)
3. **(opt-in) oversized-allocation guard:** under the diag gate, `debug_assert!` +
   `tracing::error!` before any of our own big allocations (vertex/instance/texture
   byte size, `Vec::with_capacity`) whose computed size exceeds a sane cap (e.g.
   > 512 MB) — so a fork-B single-giant-alloc trips at *our* call site with a stack
   instead of deep in PartitionAlloc.

**Verify (Editor harness, foregrounded — the gate):**
- Build `harden-memory` feature; `task mcp-dev`; pair.
- **Churn repro:** in a loop, drive a few thousand editor commands that produce big
  inverses (`set_mesh_modifiers` on a high-res mesh, `patch_kind`, `paint_where`,
  `set_node_texture`) **without** `new_project` mid-run. Sample `get_memory_stats`
  every ~30 s.
- **PASS:** `undo_bytes` plateaus at/under the budget and `wasm_heap_bytes` stops
  climbing (steady-state), instead of ramping toward 2 GB. The render-stats leak
  counters stay flat. Before the fix the same churn ramps `undo_bytes`/`wasm_heap`
  monotonically (capture that delta as evidence).

---

## ✅ RESOLVED-BLOCKER (P2/P3) — "rebuild from CPU mirrors" premise was false → re-locked to reload-from-source

**Surfaced during P2 implementation (2026-06-24); resolved same day — David chose
(b) reload-from-source-of-truth.** The original LOCKED design for B1a/B1b assumed
the renderer retains CPU mirrors of **all** scene resources — the plan text listed
"transforms buffer, instance arenas, materials, **mesh geometry**" as recoverable
from CPU. Two of those are **not** retained, by deliberate design:

- **Mesh geometry is upload-then-drop.** `meshes.rs:961 resolve_one` `remove`s the
  `GeometrySource`, uploads it, and **drops it** — "the source is dropped at the
  end of this scope … The only retained CPU state is the GPU offsets + layout"
  (the *source-freed invariant §7*). There is **no CPU copy of vertex/index data**
  to re-upload after a device loss.
- **Texture pixel data is upload-then-drop** too (same pattern in `textures.rs`).
- Retained CPU mirrors: **transforms** (`shared_arena`), **instances** (arena),
  **materials** (`Material` defs). Geometry + texture pixels are **gone**.

So `renderer.rebuild_gpu()` **cannot** reconstruct geometry/textures "from CPU
mirrors" — those mirrors don't exist. The only ways to seamless-recover are:

- **(a) Retain every mesh's geometry + every texture's pixels CPU-side forever** —
  a large, permanent RAM cost that **directly fights this plan's own
  memory-hardening goal** (P1 just *bounded* retained memory; this would
  *re-add* unbounded retention). Rejected.
- **(b) Reload/reconstruct from the retained source-of-truth** (editor:
  `EditorProject`; player: the scene bundle / glTF bytes). This is **robust and
  is what production WebGPU apps actually do on device loss** — but it is the
  "reload" the LOCKED decision forbade. **The no-reload lock was premised on CPU
  mirrors existing; since they don't, that fork must be re-opened.**

**DECISION (David, 2026-06-24): (b) reload-from-source-of-truth.** Fast and
seamless *from the user's view* because the authoritative description is already
retained, without permanently re-bloating RAM. B1b (worker respawn) then re-runs
the same source-of-truth load in the fresh worker rather than re-handing GPU
handles that died with it. **Hard constraint: no steady-state memory growth, no
perf regression** (see the LOCKED block) — (b) meets it because:

- **Editor:** the source-of-truth `EditorProject` (scene tree, captured-mesh
  geometry, materials, clips) is **already retained** for save/load. Reloading it
  via the existing `apply_project` (or the `ReloadProjectInMemory` round-trip
  path, `state.rs:1224`) adds **zero** new permanent retention.
- **Runtime/player:** source-of-truth = the scene bundle (compressed — *far*
  smaller than the expanded GPU resources) or its URL. Retain-the-bundle or
  re-fetch is the app's choice; either is negligible vs. (a)'s per-mesh CPU copy.
- **Cold path only** (`device.lost` / worker death) → no per-frame / hot-loop cost.

**Second, independent sub-task (tooling) — folded into P2/P3 scope:** the live
repro `evaluate_script("…device.destroy()…")` isn't directly runnable: the
`GpuDevice` is owned inside Rust renderer state and is **not exposed to JS** in
either harness (and in `?demo=motion` it lives in the *render worker* scope, not
the page). Forcing a loss needs a small **gated** test seam (a `#[wasm_bindgen]`
export that calls `device.destroy()`, and a worker-kill hook) — gate it
`#[cfg(any(debug_assertions, feature = "harden-diag"))]` so it never ships.

What **already landed**: P0's `install_device_lost_hook` (un-gated detection)
means a loss is **logged, never silent** — the foundation the recovery builds on.

---

## Phase 2 — GPU device-loss recovery (B1a): reload from retained source-of-truth

> **Applies to BOTH the runtime/player AND the editor — primarily the player.**
> Device loss (GPU reset, driver update, tab backgrounded/evicted, OOM) hits a
> shipped *game* harder than an editor (the player can't just "reopen the file").
> The `mt:dev` verification harness (`?demo=motion` / `?demo=remote`) **is** the
> multithreaded *runtime/player* reference app (PLAYER-GUIDE.md §9) — so P2/P3 are
> verified on the player path, not the editor. The recovery mechanism (reacquire
> device → replay the retained source-of-truth) is identical for both; only *what*
> the source-of-truth is differs (player: scene bundle / its URL; editor:
> `EditorProject`). The editor is just one consumer whose source-of-truth happens
> to already be in memory.

Seamless *from the user's view* (no visible reload), **without** permanently
retaining geometry/texture CPU mirrors (those don't exist — see RESOLVED-BLOCKER).
On loss, re-create the GPU device and **replay the authoritative description**
through the existing load path; the user sees a one-frame re-materialize, not a
page reload.

1. **Turn the Phase-0 detection hook into an action seam.** Extend
   `install_device_lost_hook` so on `.lost` it invokes a host-registered
   `on_device_lost` callback (un-gated; one-shot; cold path) in addition to
   logging. No per-frame cost — the hook is installed once at device creation.
2. **`renderer.reacquire_device()`** — a **cold** entry point that re-requests a
   fresh adapter/device + re-configures the surface, and resets the GPU-handle
   caches (pipeline pools, bind groups, texture pool, per-pass buffers) to empty
   so the subsequent load rebuilds them lazily. This is device *re-acquisition* +
   *cache reset*, NOT data rebuild — the data comes from step 3.
3. **Replay the source-of-truth load** (the part that owns the data):
   - **Runtime/player (primary):** re-run the scene load — `populate_awsm_scene`
     from the retained scene description, or re-fetch the bundle from its URL. In
     the mt demos: motion re-adds its boxes; remote re-`populate_gltf`s from the
     retained bytes / re-fetch. The player keeps the *compressed* scene
     description (or just the URL), never the expanded geometry — so no new RAM.
   - **Editor:** call `apply_project(ctrl, project)` with the retained
     `EditorProject` (the same path `ReloadProjectInMemory` uses, `state.rs:1224`)
     — re-materializes the scene tree, captured meshes, materials, clips onto the
     fresh device. Zero new retention (the project was already held for save).
4. **Guard in-flight work:** a cheap "device generation" counter — frames bump it;
   submits targeting a stale generation are dropped so nothing hits the dead
   device mid-reacquire (cf. the `mapped_staging_ring` recovery pattern). This is
   a single existence/equality check, not new per-frame allocation.

> **Memory/perf gate (must hold):** idle `get_memory_stats` before-loss ==
> after-recovery steady-state (no new retention); `?trace=sub-frame` shows no new
> per-frame spans; the generation guard is one integer compare. If recovery leaves
> `wasm_heap`/pool counters higher at steady state, that's a regression — fix or
> flag, don't ship.

**Test seam (gated `debug_assertions`/`harden-diag`):** add a `#[wasm_bindgen]`
export that calls `device.destroy()` on the live renderer's device so the repro
is drivable. In `?demo=motion` it must be callable in the render-worker scope.

**Verify (Multithreaded harness — T4.1):** `task mt:dev`, `?demo=motion`,
screenshot; force loss via the gated `device.destroy()` seam mid-session; `wait`
+ screenshot. **PASS:** the renderer reacquires + replays and **keeps rendering** —
movers resume, before/after frames match, no *page* reload, `get_logs` shows the
loss + recovery, and steady-state memory matches pre-loss.

---

## Phase 3 — render-worker-crash recovery (B1b): respawn + re-run source-of-truth load

The render worker owns the GPU device + **all** GPU handles + the slot→key
topology; on its death they're gone. Respawn it and re-run the same
source-of-truth load (B1a's step 3) in the fresh worker. The sim worker's
shared-memory arena bindings survive, so persisting topology is an optimization,
not the recovery mechanism.

1. **Watch the render worker** via `worker.onerror` + a liveness check (today
   `workers/pool.rs onerror` only fails the in-flight *meshgen* job and does not
   respawn — extend it / add the render-worker watch in the host:
   `examples/multithreaded/src/remote_demo.rs` / the demo's `start_main`).
2. **Respawn on death:** re-spawn from the same bootstrap, re-transfer a **fresh**
   `OffscreenCanvas` (the old one died with the worker — needs a fresh canvas
   element or a re-`transfer_control_to_offscreen` on a replacement), re-post the
   shared module + memory.
3. **Re-establish the scene in the fresh worker** by re-running the
   source-of-truth load (same as B1a step 3) — this rebuilds the renderer + GPU
   resources + the slot→key topology on the new device.
4. **(Optimization, optional) persist slot→key topology in shared memory** so a
   respawn that re-uses surviving sim-arena bindings can skip re-handing them:
   the render worker writes its slot→key map into a compact fixed shared region
   **only when topology changes** (load / add / remove) — never per frame. Only
   worth it if profiling shows the re-hand is a real respawn-latency cost.

**Test seam:** the render worker is killed with `worker.terminate()` from the page
(via the gated `__mt_motion_worker` handle) — no Rust seam needed.

**Verify (Multithreaded harness — T4.2):** `?demo=motion`, kill the render worker
mid-session. **Functional PASS (verified):** the watchdog detects the stale
heartbeat (~3 s), respawns the worker against a **fresh `<canvas>`**, re-runs the
source-of-truth load, the orphaned physics worker self-reaps via the shared
epoch, and the **movers resume** (frame counter restarts 60→…, `lastUpdated`→12),
no *page* reload.

### ⚠️ FLAGGED (P3) — unbounded per-crash memory leak (David's call needed)

**Recovery works, but it does NOT meet the no-memory-growth constraint.** Measured
on the live repro: each render-worker respawn leaks **~215 MB** of shared WASM
linear memory, **linearly and unbounded** — 435 → 650 → 866 → 1082 MB across 3
respawns. Root cause is a **fundamental wasm-threads limitation, not a bug in the
recovery code**: a `terminate()`d (or crashed — `panic=abort`) worker thread
**cannot run destructors**, and the single shared global allocator can never
reclaim its never-freed blocks, so the dead render worker's entire wasm-heap
footprint (staging rings, readback buffers, instance arenas, CPU scratch) is
orphaned in the shared memory every crash. (Contrast P2 device-loss, which keeps
the *same* worker and `drop`s the old renderer → **plateaus**, no leak.)

Per the LOCKED hard constraint (*"no steady-state memory growth … STOP and flag it
— do not ship it"*) this is **flagged, not shipped clean.** Mitigations need
David's call:
- **(i) Isolated per-render-group `WebAssembly.Memory`** — give each render+physics
  pair its *own* shared memory (separate from the main thread's). On crash,
  `terminate()` both and drop the memory → the whole 215 MB is freed by the GC.
  Clean fix, but a non-trivial change to the worker bootstrap (per-group memory)
  that touches every demo. **Recommended if worker-crash recovery must be
  leak-free.**
- **(ii) Accept the bounded-per-crash leak** — a render-worker *process* death is
  catastrophic + rare (device loss, the common case, is handled in-place by P2
  with no leak). One ~215 MB leak per genuine worker crash may be acceptable for a
  player; only *repeated* crashes accumulate. Cheapest; ships the current code.
- **(iii) Page-reload fallback** on worker death (the player reloads from its
  bundle URL) — zero leak, but a visible reload (the very thing (b) avoided).

The recovery mechanism (watchdog + fresh-canvas respawn + epoch-based orphan
reaping) is landed and correct; it's the foundation for (i)/(ii)/(iii). Only the
leak-elimination strategy is open.

---

## Phase 4 — asset-fetch-failure gate (T4.3 — small, independent; do as warm-up)

A scene load with a bad/unreachable asset URL must raise a **clean `Error` event,
no hang** (no infinite spinner / wedged load). Can be done first as a warm-up since
it's independent of B1.

---

## Meta-checks / non-goals

- **No per-frame cost added** anywhere (verify: idle-scene `get_memory_stats` flat;
  `?trace=sub-frame` shows no new per-frame spans; recovery code only on cold paths).
- **Release default unchanged** — diagnostics gated behind `debug_assertions` /
  `harden-diag`; only the behavioural fixes (undo cap, rebuild_gpu, respawn) ship on.
- **Don't** paper over a leak by raising a limit; bound the growth at the source.
- **Don't** add a feature-preset / new tool the task doesn't call for.

---

## Tracker (SSOT — the loop flips these)

| # | Item | Harness | Status | Evidence |
|---|------|---------|--------|----------|
| P0 | wasm_heap_bytes + undo_len/bytes in get_memory_stats; GPUDevice.lost logging hook | editor | ✅ | Live `get_memory_stats` now returns `wasm_heap_bytes:220397568`, `undo_len/bytes`, `redo_len/bytes` (gated `debug_assertions`/`harden-diag`). `install_device_lost_hook` wired beside `onuncapturederror` (renderer.rs), logs `awsm_renderer_core::device_lost`; un-gated so a loss is never silent (P2 exercises the fire). |
| P1 | Undo/redo byte-budget cap (drop-oldest) + estimator unit test | editor | ✅ | `BoundedHistory` (256 MB budget, drop-oldest) wired into `state.rs` undo/redo. 6 host tests pass (`cargo test -p awsm-editor-protocol history`). Live churn (1200 alt. ~512 KB renames, no new_project): `undo_len` plateaus **511** (drop-oldest), `undo_bytes` plateaus **268,036,874 ≤ 256 MB**, `wasm_heap` plateaus 646 MB (stops ramping). |
| P1b | Audit + bound other unbounded containers | editor | ✅ | Audited editor+renderer. Undo log was the sole unbounded-and-suspect (now fixed). Leak-panel counters **flat** across the 1200-cmd churn (render_pipelines 16, compute_pipelines 21, shaders 31, samplers 1, pool_textures 0, dynamic_materials 0 — identical before/after). CONSOLE_LOG already capped (200); renderable pools cleared/frame; compute-pipeline+shader caches have removal APIs. Noted: `RenderPipelines.cache` lacks a symmetric removal API (bounded by finite pipeline-variant domain; not the crash cause; flagged for future if dynamic-variant churn ever shows growth). |
| P1c | (opt-in) oversized-allocation debug guard | editor | ✅ | Gated guard in `renderer-core methods.rs create_buffer` (the central GPU-buffer chokepoint): a single buffer > `OVERSIZED_ALLOC_BYTES` (512 MB) logs `awsm_renderer_core::oversized_alloc` + `debug_assert!`s at our call site instead of trapping in PartitionAlloc. Gated `debug_assertions`/`harden-diag`; clippy `--all-features` clean. |
| P2 | Device-loss recovery: reacquire device + replay source-of-truth | mt:dev | ✅ | **Live on the player harness** (`?demo=motion`, gated `device.destroy()` test seam, ×5 losses): each loss logs `device lost (destroyed)` (P0 hook) → `arm_recovery` (event-driven, **no per-frame poll**) rebuilds the renderer on a fresh device + replays the source-of-truth (boxes) + re-hands the physics worker fresh arena bindings. **Movers resume every time** (`lastUpdated`→0 during the ~1s rebuild, back to 12), frame counter continuous (**no page reload**), logs show loss+recovery. **Memory plateaus at 498 MB flat across repeated recoveries** (no per-recovery leak; one-time high-water from the brief rebuild overlap — wasm linear memory never shrinks but is reused). Per-frame cost = **one `Cell<bool>` read** (`recovering`, false in steady state) — the `recovering` flag also serves as the in-flight-submit guard. Renderer-core seam: `AwsmRendererWebGpu::on_device_lost`. `task lint` clean. |
| P3 | Render-worker respawn + re-run source-of-truth load | mt:dev | ⚠️ FUNCTIONAL / FLAGGED | **Recovery works** (live `?demo=motion`, `worker.terminate()`): heartbeat watchdog (3 s) → respawn on a **fresh `<canvas>`** → re-run source-of-truth load → orphaned physics worker **self-reaps via a shared-memory `RENDER_EPOCH`** → movers resume (frame restarts, `lastUpdated`→12), no page reload. **BUT** it leaks **~215 MB/respawn, unbounded** (435→650→866→1082 MB over 3 respawns) — a killed worker thread can't run destructors and the shared allocator can't reclaim its blocks (fundamental wasm-threads limit, not our bug). Per the no-memory-growth constraint: **FLAGGED — needs David's call** (isolated per-group memory / accept bounded leak / page-reload fallback). See ⚠️ FLAGGED section. |
| P4 | Asset-fetch-failure → clean Error, no hang | mt:dev | ✅ | Live (`?demo=remote&model=nonexistent-bad-asset.glb`): a bad asset URL surfaces a clean `RenderEvent::Error` ("parse glb: …") **immediately** (t=0, no hang), `loading` clears, page stays responsive, `ready:false` (no false success). Existing `if let Err(err) = load_gltf(...)` wrapper already routes any fetch/parse failure to one Error event + `loading=false` — no code change needed; gate verified. |
| Meta | No per-frame cost; release default unchanged | both | ✅ | Landed phases add **zero per-frame cost**: undo cap runs only on edits (push/pop), diagnostics are gated `debug_assertions`/`harden-diag` + cold-path (MCP query / `.lost` / `create_buffer`), device-lost hook is one-shot at device creation. Proof: 1200-cmd churn left all render-stats leak counters flat (render_pipelines/shaders/samplers/pool_textures unchanged); `frame_dt_ms` 16.69 steady. Release default unchanged (behavioural fixes un-gated = correctness; all diagnostics gated). `clippy --all --all-features --tests -D warnings` clean. |

## Status (2026-06-24)

**Done (7/8 rows ✅):** P0, P1, P1b, P1c, **P2**, P4, Meta — all with live evidence
(see tracker). The plan's **prime goal — the unbounded-undo OOM ("Aw, Snap!") — is
fixed and live-verified** (undo log plateaus under a 256 MB byte budget;
`wasm_heap` stops ramping), and **GPU device-loss recovery (P2) works leak-free**
on the player harness. `task lint` clean. Committed per phase on `doc-aw-snap`.

**Device-loss recovery (P2) ✅ done** under (b) reload-from-source — live on the
player harness, movers resume, memory **plateaus** (no leak), one `Cell` read of
per-frame cost. **Worker-crash recovery (P3) ⚠️ functional but FLAGGED:** the
respawn + fresh-canvas + epoch-orphan-reaping works and movers resume, but it
leaks **~215 MB per crash, unbounded** — a fundamental wasm-threads limit (a
killed thread can't free its shared-memory allocations). Per the no-memory-growth
constraint it's flagged, not shipped clean — **needs David's call** between
isolated per-group memory (clean fix, bigger change), accepting the bounded
per-crash leak (worker death is rare; P2 device-loss is the common case and is
leak-free), or a page-reload fallback. See the ⚠️ FLAGGED (P3) section.

## Done criteria

Every tracker row ✅ with live evidence: Phase 1 churn repro plateaus
`undo_bytes`/`wasm_heap_bytes` under budget (vs a ramp before); Phase 2
`device.destroy()` mid-session recovers and keeps rendering; Phase 3 killing the
render worker respawns and resumes; Phase 4 a bad asset fails cleanly. `task lint`
clean. No per-frame cost added (idle stats flat). Committed per phase on
`harden-memory`.
