# Multithreaded renderer — remaining **testing** before "production-ready"

The Phase 1 + Phase 2 architecture is landed and verified (see
`docs/PLAYER-GUIDE.md` §9). What remains before a public ship is **validation**,
not architecture. This doc is *testing only* — code work deferred from the
hardening lives in `docs/plans/multithread-build-plan.md`, not here.

Everything below was verified **in Chrome (desktop) only**; that is the single
biggest source of unknowns.

## T1 — Cross-browser bring-up (highest priority)
Run every `?demo=` (esp. `remote`, `skin`, `lights`, `crowd`, `churn`) on:
- **Safari** (macOS + iOS): WebGPU is newest here; verify `SharedArrayBuffer`
  under COOP/COEP, the worker bootstrap (`init({module, memory})`), and
  `OffscreenCanvas` transfer all work.
- **Firefox**: same matrix; confirm `+atomics` shared memory import + COEP.
- **Per-API checks:** `crossOriginIsolated`, shared `WebAssembly.Memory`,
  `queue.writeBuffer` from a SAB-backed view (works in Chrome — re-confirm),
  `OffscreenCanvas.convertToBlob` (fails in Chrome on a WebGPU canvas — check if
  it differs; informs the build-plan's screenshot path).
- **Exit:** each demo renders + its gate passes, or the failure is logged with
  the engine-specific cause.

## T2 — Mobile (one real device each: iOS Safari, Android Chrome)
- Memory headroom vs the `--max-memory` ceiling (currently 2 GiB); confirm a
  large scene + churn doesn't OOM the shared memory.
- DPR / backing-store sizing (the resize path) on a high-DPR phone.
- Touch input forwarding (`?demo=input` pointer events).
- Thermal / sustained frame-time on mobile GPUs.

## T3 — Performance at scale + soak (the one that catches slow leaks)
- **Frame-time budgets** under `?stress=N` for `motion` / `crowd` at rising N;
  record the mover-count where the render worker misses 16.6 ms.
- **Multi-minute soak with a heap trace** (`?trace=sub-frame`,
  `take_heapsnapshot`): confirm **no per-frame heap growth** in the render hot
  path (the pack/upload path is pooled by construction — verify under load).
- **Shared-memory growth is BOUNDED under sustained spawn/despawn churn**
  (`?demo=churn` over many minutes). Shared `WebAssembly.Memory` grows but never
  shrinks; confirm the arena's free-slot reuse keeps growth flat rather than
  ratcheting. **If growth is unbounded, that flips to a build item** (arena
  compaction / slab reuse policy) — note it back in the build-plan.
- **Exit:** documented N-vs-frametime curve; flat memory over a 10-min churn soak.

## T4 — Resilience **verification** (after the build-plan lands the code)
The recovery *code* is in `multithread-build-plan.md`; this is its test side:
- Force `GPUDevice` loss (devtools / `device.destroy()`) mid-session → renderer
  rebuilds and keeps rendering.
- Kill the render worker mid-session → main thread respawns it, re-hands the
  `OffscreenCanvas`, re-establishes arena bindings, scene is intact.
- Asset-fetch failure during a scene load → clean `Error` event, no hang.

## T5 — Allocation / GC validation (David's standard)
- Under `?stress` + `?trace=sub-frame`, confirm the render hot path does **zero**
  per-frame heap allocation (pooled scratch in `transforms::descend_pack_arena`;
  pre-allocated binding/bind tables in the physics workers). Catch any
  regression that reintroduces a per-frame `Vec`/`Box`.

## How to run
- Desktop Chrome gates: chrome-devtools MCP (as used throughout Phase 2).
- Cross-browser / mobile: real browsers + devices (MCP is Chrome-only).
- Server: `task mt:dev` (port 9090, COOP/COEP).
