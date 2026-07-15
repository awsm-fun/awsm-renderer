# WebGPU per-frame churn — leak fix + performance uplift

**STATUS: PROPOSED (for discussion, not started).** Written 2026-07-15 after the
soak investigation (see `docs/debugging-leaks.md`, the `crashes.md` follow-up, and
the diagnostic data in the soak runs). Fixes the editor-tab VA-leak crash *and*
cuts per-frame CPU overhead — they share one root cause.

## TL;DR

The idle editor tab crashes after ~71 min because the renderer process leaks
virtual address space at ~35 never-freed VM regions/sec. Diagnostics traced this
to **high per-frame GPU-object churn**: the render loop creates **~172
`createBindGroup` and ~516 `createCommandEncoder` calls per second** (≈1.4 and ≈4.3
per frame at 120fps), while `createBuffer` is flat. A subset of that churn is never
reclaimed by Chrome's PartitionAlloc/V8 page allocators (GC can't see GPU/native
memory), which is what marches to the 70 GB `page-allocator-mapped-size` ceiling in
the crash dumps. **Reducing the churn fixes the leak and improves throughput at the
same time.** This is exactly the anti-pattern WebGPU best-practice guidance warns
against.

## Evidence chain (from the soak diagnostics)

1. **Reproduced**: idle `ssr-arena`, crash at 71 min (`target-crashed`).
2. **Eliminated**: `create_buffer` flat (528), wasm heap flat, JS heap bounded
   sawtooth, all object counts flat, RSS saturates ~10 GB. So not a buffer/heap/
   object-count leak; RSS is not the driver.
3. **Localized (full `vmmap`)**: linear accumulation of ~35 regions/sec in
   **Memory Tag 253 = PartitionAlloc** (+4.8 GB) and **Tag 255 = V8 page allocator**
   (+1.5 GB) — Tag 255 *is* the crash dump's `page-allocator-mapped-size`.
4. **Ablated**: `?noring` (staging ring → writeBuffer-only, zero map/getMappedRange)
   left the rate unchanged (35.5/s) ⇒ **ring exonerated**, leak is upload-path
   independent. Empty scene (`meshes=0`) still leaks ~28/s ⇒ **base render loop**,
   scene-independent.
5. **Named the churn** (new `create_*` census): bind groups ~172/s, command encoders
   ~516/s, buffers 0/s. The leak is the un-reclaimed slice of this per-frame churn.

Research corroboration (primary sources, see `webgpu-perf-leak-research` memo): a JS
GPU-object wrapper is ~150 bytes but pins MB–GB the GC never sees; bind
groups/pipelines/query-sets have **no `.destroy()`** — the only lever is *don't
recreate per frame, cache them*; per-frame bind-group/descriptor creation is the
textbook renderer-process growth cause.

## Guiding principle

Extend David's standing rule — *zero per-frame allocations in the hot path* — 
**explicitly to GPU objects** (bind groups, command encoders, transient render
targets, readback buffers), not just JS/wasm heap. Create-once/cache/reuse; only the
per-draw *data* changes per frame (via `writeBuffer` + dynamic offsets).

## What's already good (do NOT redo)

The renderer is not naive — the plan builds on solid foundations:
- **Async pipeline creation + caching** (`pipeline_scheduler`, `createRenderPipelineAsync`). ✅
- **~1 `submit` per frame** in steady state. ✅
- **Dynamic offsets** already used in several passes (transparent, geometry, shadows). ✅
- **`create_buffer` well-managed** (flat under soak; oversized guard in place). ✅

The gaps are: command-encoder consolidation, bind-group caching coverage, and
(untapped) render bundles.

## The plan

### Phase 1 — Consolidate command encoders (biggest churn cut, low risk)
- **Finding**: `buffer/mapped_uploader.rs:167` creates a *new* command encoder per
  subsystem upload-flush per frame (~4–5/frame), plus the main "Rendering" encoder.
- **Change**: thread ONE per-frame command encoder through the upload flushes and the
  main pass (record all copies + passes into it), then one `finish`+`submit`. This is
  the recommended shape (one encoder, one submit) and removes the bulk of the 516/s
  encoder churn.
- **Risk**: low–moderate — must preserve the ring's "submit before kicking mapAsync"
  ordering. Verify with the soak (encoder rate → ~1/frame) and a visual check.

### Phase 2 — Bind-group caching (kills the leak's main slice + rebind cost)
- **Audit** the ~172/s `createBindGroup` sites for ones rebuilt every frame instead of
  on-change. Known non-cached/inline candidates from the survey: `material_prep`
  edge bind group (`render_pass.rs:402`), occlusion/HZB (`hzb/bind_group.rs` push
  sites), tonemap/present. (Most material/shadow/bloom bind groups are already cached
  in fields — good.)
- **Change**: a first-class **bind-group cache** keyed by (layout + resource ids);
  recreate only when a referenced resource changes. Organize by update frequency —
  `@group(0)` per-frame (camera/env), `@group(1)` per-material, `@group(2)` per-draw
  via one big buffer + 256B-aligned dynamic offsets (extend what several passes already
  do). Use explicit shared layouts (no `layout:'auto'` for shared groups) so one
  camera bind group works across pipelines.
- **Risk**: moderate — invalidation correctness is the tricky part. The soak is the
  regression gate; add a `create_bind_group` rate assertion.

### Phase 3 — Upload-path simplification (evaluate; complexity + churn reduction)
- Research is clear that `queue.writeBuffer` is the recommended default for per-frame
  dynamic data (internally sub-allocates staging + non-blocking copy; no hand-rolled
  N-buffering needed). Our bespoke mapped-staging-ring adds complexity and shows
  chronic fallback churn (~150/s) with no measured win.
- **Proposal**: A/B the ring vs `writeBuffer` + one big dynamic-offset uniform/storage
  buffer for the small per-frame uniforms (camera, frame globals, transforms) under
  `?trace=sub-frame`. If writeBuffer is within noise (likely), retire the ring for
  those paths (keep it only where a measured stall justifies it). Fewer encoders,
  less code, same or better perf.
- **Risk**: moderate — touches core upload; gated by the A/B measurement. Do AFTER
  Phase 1–2 stop the bleeding.

### Phase 4 — Render bundles for the static opaque pass (biggest untapped CPU win)
- None exist today. Record the opaque draw list into a `GPURenderBundle` once; replay
  with `executeBundles`; mutate referenced buffers (via writeBuffer) instead of
  re-recording. Pull viewport/scissor/blend-const/stencil out of the bundle. Skip when
  GPU-bound (it only cuts CPU encode cost). Compose with Phase 6 indirect draws so
  culling doesn't force a re-record.
- **Risk**: moderate — bundles fully re-specify state; format/sampleCount must match.

### Phase 5 — Readback hygiene
- Move the per-frame GPU→CPU readbacks (coverage/occlusion/overflow) to a fixed **ring
  of 2–3 `MAP_READ` staging buffers**, allocated once, consumed N-frames-late, never
  per-frame-created, small counters coalesced into one buffer/one `mapAsync`. (Mostly a
  correctness/robustness cleanup — coverage is off by default in the editor, but the
  pattern should be right for when it's on.)

### Phase 6 — GPU-driven rendering expansion (future, ties into nanite/LOD)
- Single-buffer `drawIndexedIndirect` + compute culling (all indirect args in ONE
  buffer — Chrome/D3D12 validation is ~300× cheaper that way, and invisible to
  timestamp queries). `multiDrawIndirect`/bindless/64-bit-atomics are flag-gated
  experimental → optional accelerations with fallbacks. Overlaps the existing cluster/
  nanite work.

### Phase 7 — Profiling + permanent gate
- Add a `timestamp-query` GPU-frame-time harness (ping-pong result buffers, dev-flag to
  unquantize) to measure the perf wins objectively.
- Keep the soak (`task soak` + the `create_*` census) as a **permanent leak gate**:
  bind-group/encoder rates and mapped-region count must stay flat over an 8h run. This
  is what turns "fixed once" into "stays fixed."

## Sequencing & verification

- **Do Phase 1 → 2 first** — they stop the crash and are the safe churn cuts. Re-run
  the soak after each; target: encoder rate → ~1–2/frame, bind-group rate → near-zero
  steady-state, mapped-region count FLAT over 8h.
- Phase 3–4 are the perf multipliers (do after the leak is closed and measured).
- Phase 5–6 are robustness/future.
- Every phase is verified by the existing soak harness + census (the instrument that
  found the bug is the instrument that proves the fix).

## Open question to resolve before implementing

The exact leaking object *within* the churn isn't pinned 1:1 (leak ~35/s vs churn
172+516/s) — but the fix (cache/consolidate) is identical regardless, and Phase 1–2
will drive the rate to zero and confirm by soak. If we want certainty first, one more
ablation (instrument-then-disable a specific bind-group site) would pin it — optional,
~30 min, not required to proceed.
