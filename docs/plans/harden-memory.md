# Harden memory: crash / leak / recovery (single SSOT)

> **One executable plan** folding the former `aw-snap.md` (overnight "Aw, Snap!"
> OOM crash) and `device-loss-recovery.md` (GPU-device-loss + render-worker-crash
> recovery) into one autonomous workstream. Goal: a production session **never
> dies** from a memory leak, a lost GPU device, or a dead render worker — and the
> render hot path pays **zero** for any of it.
>
> **Design decisions are LOCKED (no forks left to ask):**
> - **Undo/redo cap:** total-**byte** budget, drop-oldest.
> - **Device-loss (B1a):** in-place **`rebuild_gpu()` from CPU mirrors** —
>   best-in-class seamless recovery (no reload). Rebuild is a cold path.
> - **Worker-crash (B1b):** **persist slot→key topology in shared memory** —
>   fast seamless respawn (no scene reload). Topology written on change, not per
>   frame.
> - **Scope:** all three, **ordered** (Phase 1 → 2 → 3), each **self-verified** as a
>   hard gate before the next.
> - User directive: *most robust / best-in-class result; code churn and
>   backwards-compat are NOT concerns; the only hard constraint is no per-frame
>   hot-loop cost.*

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

## Phase 2 — GPU device-loss recovery (B1a): `rebuild_gpu()` from CPU mirrors

Best-in-class, seamless (no reload). The scene/arena/CPU mirrors (transforms buffer,
instance arenas, materials, mesh geometry) are retained; only `web_sys` GPU handles
are gone.

1. **Subscribe to `GPUDevice.lost`** (extend the Phase 0 hook from logging to
   action).
2. **`renderer.rebuild_gpu()`** — one **cold** entry point that:
   - requests a fresh adapter/device, re-creates the surface configuration, and
   - rebuilds **every** GPU-handle subsystem (render passes, pipeline pools, bind
     groups, texture pool, buffers) and re-uploads from the CPU mirrors.
   - Give each GPU-handle subsystem a `rebuild`/recreate method behind this single
     entry point. Churn is fine; **no per-frame cost** (only called on `.lost`). A
     cheap "device generation" check may guard in-flight submits during rebuild.
3. Guard in-flight work: drop/await the current frame's submits so nothing targets
   the dead device mid-rebuild (cf. the existing `mapped_staging_ring` recovery
   pattern).

**Verify (Multithreaded harness — T4.1):** `task mt:dev`,
`?demo=motion`, screenshot; force loss via chrome-devtools
`evaluate_script("…device.destroy()…")` (or the lost-context path) mid-session;
`wait` + screenshot. **PASS:** the renderer rebuilds and **keeps rendering** —
movers resume, before/after frames match, no reload, `get_logs` shows the loss +
recovery.

---

## Phase 3 — render-worker-crash recovery (B1b): persist topology in shared memory

Best-in-class, fast respawn (no scene reload). The sim worker's shared-memory
bindings survived; the **render worker that owned the slot→key topology died**.

1. **Persist topology in shared memory.** The render worker writes its slot→key map
   into shared `WebAssembly.Memory` **whenever topology changes** (load / add /
   remove) — **not per frame** (no hot-loop cost). Lay out a compact, fixed region
   for it.
2. **Watch the render worker** via `worker.onerror` + a heartbeat (today
   `workers/pool.rs onerror` only fails the in-flight *meshgen* job and does not
   respawn — extend it / add the render-worker watch in the host,
   `examples/multithreaded/src/remote_demo.rs`).
3. **Respawn on death:** re-spawn from the same bootstrap, re-transfer a **fresh**
   `OffscreenCanvas` (the old one died with the worker), re-post the shared module +
   memory, **read the topology back** from shared memory, re-hand every live arena
   `SlotBinding`, and resume — no scene reload.

**Verify (Multithreaded harness — T4.2):** `?demo=motion`, kill the render worker
mid-session via chrome-devtools; **PASS:** the host respawns it, re-hands the
`OffscreenCanvas`, re-establishes arena bindings from the persisted topology, and the
**movers resume** (scene intact), no reload.

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
| P2 | `rebuild_gpu()` from CPU mirrors + GPUDevice.lost action | mt:dev | ☐ | |
| P3 | Render-worker respawn + topology-in-shared-memory | mt:dev | ☐ | |
| P4 | Asset-fetch-failure → clean Error, no hang | mt:dev | ☐ | |
| Meta | No per-frame cost; release default unchanged | both | ☐ | |

## Done criteria

Every tracker row ✅ with live evidence: Phase 1 churn repro plateaus
`undo_bytes`/`wasm_heap_bytes` under budget (vs a ramp before); Phase 2
`device.destroy()` mid-session recovers and keeps rendering; Phase 3 killing the
render worker respawns and resumes; Phase 4 a bad asset fails cleanly. `task lint`
clean. No per-frame cost added (idle stats flat). Committed per phase on
`harden-memory`.
