# WebGPU per-frame churn — leak fix + performance uplift

**STATUS: Phase 1 SHIPPED + verified. Phases 2–7 revised per review (below).**
Written 2026-07-15 after the soak investigation (see `docs/debugging-leaks.md`, the
`crashes.md` follow-up, and the diagnostic data in the soak runs). Fixes the
editor-tab VA-leak crash *and* cuts per-frame CPU overhead — they share one root
cause.

## Phase 1 result (shipped) — encoder churn 4/frame → 2/frame

Two consolidations, both verified on `ssr-arena` at a locked 60 fps (measured by
wrapping `GPUDevice.createCommandEncoder` + `GPUQueue.submit` in-page — do NOT infer
fps from encoder counts, that's circular):

**1a. Shared per-frame upload encoder.** Per-subsystem upload flushes each used to
create + submit their own encoder (~4–5/frame). Now they record their
`copyBufferToBuffer` into ONE shared encoder on the `AwsmRendererWebGpu` handle
(`record_upload` → lazy encoder; auto-flushed at the next
`submit_commands`/`submit_commands_batch`; the staging ring's `mapAsync` kick
deferred to the next `acquire`, guarded by a monotonic `upload_flush_epoch` so it
never maps a slot whose copy hasn't reached the queue).

**1b. Fold the per-frame opaque-texture clear into the render encoder.** The `opaque`
storage texture (deliberately not a RENDER_ATTACHMENT, for TBR mobile) is cleared via
`copy_buffer_to_texture`. That was its own per-frame encoder+submit — and because it
submitted mid-frame it *split* the upload flush in two (`upload-shared` ×2). Recording
the clear into the frame's "Rendering" encoder (ordered ahead of the opaque pass)
removes that encoder+submit AND collapses the upload flush to one.

Per-frame breakdown (encoder label × per-frame count), same scene, 60 fps:

| build | encoders/frame | submits/frame | labels |
|---|---|---|---|
| pre-Phase-1 | ~ (516/s ≈ several) | several | per-subsystem upload encoders + Rendering + Texture Clearer |
| 1a only | 4 | 4 | upload-shared ×2, Rendering ×1, Texture Clearer ×1 |
| **1a + 1b (shipped)** | **2** | **2** | **upload-shared ×1, Rendering ×1** |

That's the floor without a deeper frame-sequencing refactor: uploads happen in the
subsystem write paths *before* the "Rendering" encoder exists, so merging the last
`upload-shared` into `Rendering` (→ 1 encoder / 1 submit) needs the render encoder
created up front — deferred as riskier, not done. Bind-group rate unchanged at 3/frame
(untouched — that's Phase 2). No rendering regression (ssr-arena pixel-identical, cleared
background still black), **zero WebGPU validation errors** both times. An 8h soak is
still the durable leak gate.

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

### Phase 1 — Consolidate command encoders (biggest churn cut, low risk) — ✅ DONE
- **Finding**: `buffer/mapped_uploader.rs` created a *new* command encoder per
  subsystem upload-flush per frame (~4–5/frame), each with its own `submit`.
- **Change (shipped, 1a)**: one shared per-frame upload encoder on the gpu handle
  (`record_upload`), auto-flushed at the next `submit_commands`; ring `mapAsync` kick
  deferred to the next `acquire` and epoch-guarded (`upload_flush_epoch`) so the
  "copy submitted before map kicked" invariant holds without a per-flush submit.
- **Change (shipped, 1b)**: fold the per-frame opaque-texture clear
  (`TextureClearer::clear`) into the "Rendering" encoder instead of its own
  encoder+submit — also collapses the upload flush from 2→1 per frame.
- **Result**: 4 → 2 encoders/frame and 4 → 2 submits/frame, zero validation errors,
  ssr-arena pixel-identical. See the result table above.

### Phase 2 — Bind-group churn: FIND THE BUG (not a cache) — next
- **Correction from review**: bind-group recreation in this codebase is *supposed to
  be event-driven* — 46 of ~49 `create_bind_group` sites are in `recreate_*` methods
  keyed on change events. So a steady **172/s** create rate on an *idle* scene is a
  **bug** (something recreates every frame that shouldn't), NOT a missing cache. Do
  NOT bolt on a general bind-group cache; that would paper over the real defect.
- **Known per-frame (mesh-dependent) offenders from the survey**: `material_prep`
  `render_edge` (`render_pass.rs:402`), `material_opaque` `build_shade_bind_group`
  (`render_pass.rs:141`) and `build_edge_bind_groups` (`:181`). These rebuild inline
  per draw instead of on mesh-set change.
- **Change**: make those sites event-driven like the other 46 — cache the bind group
  on the owning struct, invalidate only when a referenced resource (mesh buffer,
  texture, layout) actually changes. The fix is per-site correctness, not a new
  abstraction. Soak `create_bind_group` rate → near-zero on an idle scene is the gate.
- **Risk**: low–moderate — localized; invalidation must cover every referenced resource.

### Phase 3 — Upload path: KEEP the staging ring (no A/B retire)
- **Correction from review**: the mapped-staging-ring is a *deliberate* perf choice
  over `queue.writeBuffer` — it writes straight into mapped memory and avoids the
  browser's staging-copy hop (see the module doc in `mapped_staging_ring.rs`). We
  moved *away* from `writeBuffer` + dynamic offsets on purpose. **Do not retire it.**
- The earlier "A/B then retire" proposal is **dropped**. If the ~fallback churn ever
  shows a measured cost, tune ring depth / acquisition, don't replace the ring.

### Phase 4 — Render bundles for the static opaque pass (conditional CPU win)
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

### Phase 6 — GPU-driven rendering — ALREADY DONE (dropped)
- **Correction from review**: we already do GPU-driven indirect rendering in the
  nanite/cluster path (single-buffer `drawIndexedIndirect` + compute cull/compaction).
  There is no separate work here; the "expand GPU-driven indirect" item is **dropped**
  as redundant. Any indirect-arg-buffer hygiene lives with the nanite code, not here.

### Phase 7 — Profiling + permanent gate
- Add a `timestamp-query` GPU-frame-time harness (ping-pong result buffers, dev-flag to
  unquantize) to measure the perf wins objectively.
- Keep the soak (`task soak` + the `create_*` census) as a **permanent leak gate**:
  bind-group/encoder rates and mapped-region count must stay flat over an 8h run. This
  is what turns "fixed once" into "stays fixed."

## Sequencing & verification

- **Phase 1 done** (encoder rate 516→229/s). **Phase 2 next** — hunt the per-frame
  bind-group *bug* (make the 3 mesh-dependent sites event-driven), target
  `create_bind_group` → near-zero on an idle scene.
- Phase 3 is a KEEP decision (ring stays); Phase 4 (render bundles) is a conditional
  CPU win once the leak is closed; Phase 5 is robustness; Phase 6 is already covered by
  nanite.
- Every phase is verified by the existing soak harness + census (the instrument that
  found the bug is the instrument that proves the fix). The 8h soak with FLAT
  mapped-region count is the durable gate.

## Open question — does Phase 1 alone move the leak?

Phase 1 halved encoder churn but did NOT touch the 172/s bind-group rate. If the leak
tracks bind-group churn (the more likely culprit — bind groups pin the resources they
reference and have no `.destroy()`), the crash won't close until Phase 2. Run a long
soak on the Phase-1 build to see whether the ~35 region/s rate drops proportionally
with the encoder cut or holds at the bind-group rate — that pins which churn stream
feeds the leak and confirms Phase 2 is the real fix.
