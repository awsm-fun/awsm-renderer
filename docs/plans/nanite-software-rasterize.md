# Nanite software rasterizer + streaming (future, test-gated)

> A separate, **high-risk** future optimization on top of the cluster-LOD work
> (Phase B, now shipped — see PR #143 / the deleted `lod.md`). LOD shipping does
> **not** depend on any of this. The Phase 0 spike below is the go/no-go gate; if
> it fails, HW-raster cluster-LOD is the end-state and that is fine.

> **STATUS — IN PROGRESS on the `nanite-streaming` branch** (started after the LOD
> PR #143). Plan/ordering for this branch, gated behind NEW default-off flags so
> the shipped LOD path never regresses:
> 1. **Phase 0 — SW-raster spike (GO/NO-GO), first.** New standalone wasm crate
>    `packages/frontend/bench-nanite-sw-raster/` mirroring `model-tests` (Trunk +
>    `taskfiles/frontend/...` dev server; build the device via renderer-core
>    `AwsmRendererWebGpuBuilder`). Implement HW baseline + Encoding A (packed
>    `atomicMax`) + Encoding B (emulated-64 CAS), a triangle-size + overdraw knob,
>    `performance.now()` timing over many iters, and a payload-image diff vs HW.
>    Drive it via chrome-devtools (navigate → read browser console). RECORD the
>    verdict + numbers here. If NO-GO, skip Phase 3 and go straight to streaming.
> 2. **Phase 5 — streaming (the multi-million-tri fix), independent of Phase 0.**
>    This is the user's priority (multi-million-tri scaling). Start with the
>    intermediate win: cap/page cluster-page residency so the cluster render mesh
>    `M` no longer uploads its FULL exploded geometry (today's ceiling) — GPU
>    feedback marks the pages the cut needs, CPU streams/evicts against a budget,
>    LOD clamps to resident pages. Land gated, show it renders a denser asset than
>    today, document the residual gap, then iterate toward full residency.
> 3. **Phase 3 — hybrid rasterization, ONLY if Phase 0 is GO.**
> Discipline unchanged from the LOD work: gate behind default-off flags
> (flag-off byte-identical), test before every commit, on-device verify via
> chrome-devtools (renderer tracing → BROWSER console), GPU-readback to confirm
> compute output, no per-frame heap allocs, pure-Rust algorithms (wasm target),
> one logical step per commit, never leave the renderer broken (revert+diagnose).

## Why this is split out

Nanite's signature win is rasterizing **sub-pixel triangles** in a compute
shader instead of the hardware rasterizer (HW raster wastes a full 2×2 quad per
triangle → ~25% efficiency or worse for pixel-sized triangles). The compute
rasterizer does a 64-bit `atomicMin(depth << 32 | payload)` per covered pixel:
one atomic that simultaneously depth-tests and records the winning triangle.

**WebGPU has no 64-bit atomics and no extension for them.** WGSL atomics are
`atomic<u32>` / `atomic<i32>` only. So the entire SW-raster advantage hinges on
emulating the depth+payload atomic well enough — correct enough, fast enough —
to beat HW raster for tiny triangles on our target hardware. This is the single
biggest risk in the whole Nanite story, and it is the *only* part that is
genuinely WebGPU-hostile. Anchoring LOD delivery to it would be a mistake — so
it lives here, behind its own gate, and `lod.md` ships without it.

Other WebGPU constraints relevant here:
- **No mesh/task shaders** → cluster expansion is indirect compute dispatch +
  indirect draw, not a mesh-shader amplification stage.
- **No persistent-thread forward-progress guarantee** → the SW-raster scheduler
  must be a fixed multi-pass DAG, not a persistent-thread work queue, and no
  per-pixel spinlock (deadlock risk).

---

## Phase 0 — SW-rasterizer atomic-emulation spike (GO/NO-GO)

> **VERDICT: NO-GO on this WebGPU target (Apple GPU). Phase 3 is NOT built; the
> shipped HW-raster cluster-LOD (Phase B) is the end-state — exactly the outcome
> this doc says is fine. Work pivots to Phase 5 (streaming) for multi-million-tri.**
>
> Spike ran as a self-contained WebGPU bench via chrome-devtools `evaluate_script`
> (own GPUDevice, no editor interference); code + raw numbers in
> `nanite-sw-raster-bench.js`.
> - **Step 1 — atomic-throughput baseline:** ~5.75 G `atomicMax` ops/sec (16.7M /
>   2.92 ms). Harness works on target.
> - **Step 2 — HW baseline vs Encoding A** (packed-u32 `atomicMax` SW raster, the
>   PERF CEILING probe; 16-bit payload, unusable in production), three runs sweeping
>   triangle pixel-size: A is **at best ~1.5–1.8× faster than HW only at sub-pixel
>   (≤1px) sizes — and that margin sits within the sub-ms measurement noise — while
>   A LOSES at ≥2px** (HW ~0.03 ms vs A growing with triangle area). Chrome
>   wall-clock timing is noisy at these sub-ms scales and timestamp-queries are
>   quantized to 100 µs, so a precise margin isn't recoverable, but the *shape* is
>   stable across all three runs: no blowout win.
> - **Why NO-GO:** the doc's gate is "A/B beat HW by a WORTHWHILE margin at small
>   sizes." Not met. A is the *ceiling*; the production Encoding B (emulated-64
>   depth+payload via a CAS spin) is strictly slower than A, so if the ceiling only
>   ties/marginally-beats HW, the realistic encoding loses. Apple's HW rasterizer
>   is efficient even for tiny triangles, and WebGPU's missing 64-bit atomics cap
>   the SW approach — together they remove the sub-pixel-quad advantage SW raster
>   exists to exploit. Building the emulated-atomic SW rasterizer + resolve pass +
>   hybrid routing is not justified by a ~1.5× noisy maybe-win.
> - **Caveat / re-open condition:** measured on one Apple target with a simple
>   one-thread-per-triangle rasterizer. If a future target shows HW raster as a
>   *proven* sub-pixel bottleneck (the original gate condition), re-run with a
>   heavier controlled workload (≥50 ms) to beat the noise before reconsidering.
> - **Phase 3 below is therefore SKIPPED.** Remaining work on this branch =
>   Phase 5 (streaming), independent of the SW rasterizer.

De-risk before building anything. In isolation, implement and benchmark a
compute software rasterizer that emulates the 64-bit depth+payload atomic.

**Harness:** standalone crate `packages/frontend/bench-nanite-sw-raster/` (no
production render passes), with a **triangle-size knob** (SW raster only wins
below the HW quad-efficiency cliff) and an **overdraw knob** (atomic
contention). For each of {HW baseline, A, B} × {size, overdraw}: dispatch/draw →
readback via `new_copy_and_extract_buffer` → time with `performance.now()` over
many iters → (for A/B) diff the payload image vs HW ground truth.

**Decision gate:**
- **GO** (build Phase 3 below): B is correct enough (negligible payload
  mismatch) and at small triangle sizes A/B beat HW by a worthwhile margin.
- **NO-GO**: A is too slow, or B's error rate is visible, or neither beats HW at
  realistic Nanite triangle sizes → stop; HW-raster cluster-LOD (`lod.md`
  Phase B) is the end-state.

### Depth convention

Renderer is reverse-Z and the HZB is **max-reduced** (closer = larger depth).
That lines up with `atomicMax`: largest packed value wins ⇒ closest fragment
wins, *provided depth sits in the high bits*. No convention change needed.

### The bit-budget problem (why one u32 isn't enough for production)

Pack `packed = (depth << PAYLOAD_BITS) | payload`, resolve with `atomicMax`. The
payload must identify the winning surface:
- **triangle-within-cluster**: clusters ≤128 tris ⇒ **7 bits**.
- **visible-cluster index**: indexes the *per-frame compacted visible-cluster
  list* (not a global cluster id). Even a dense scene caps at, say,
  2^18 = 262 144 ⇒ **18 bits**.

Payload ≈ 25 bits, leaving only **7 bits of depth** in a u32 — useless
(z-fighting). This is exactly why a faithful implementation needs 64 bits:
depth(32) + payload(32). On WebGPU we can't have that in one atomic — hence two
encodings to benchmark, a perf ceiling and a realistic one.

### Encoding A — single-u32 packed `atomicMax` (PERF CEILING probe)

`packed = (depth16 << 16) | payload16`, one `atomic<u32>` per pixel, resolved
with `atomicMax`. 16-bit payload is too small for production, but A **measures
the upper bound on atomic-raster throughput** — the single fastest the approach
can ever go (one atomic, no loop, no second store). **If A is already too slow,
full SW raster is dead on WebGPU** — stop, don't even tune B.

### Encoding B — emulated 64-bit (REALISTIC correctness + perf)

Two parallel per-pixel arrays: `depth: array<atomic<u32>>` (full 32-bit) and
`payload: array<u32>` (full 32-bit, non-atomic). Per covered pixel:

```
loop {
  let cur = atomicLoad(&depth[i]);
  if (my_depth <= cur) { break; }                 // reverse-Z: not closer, lose
  let res = atomicCompareExchangeWeak(&depth[i], cur, my_depth);
  if (res.exchanged) { payload[i] = my_payload; break; }
  // else: someone moved depth; retry with res.old_value
}
```

Full depth precision + full 32-bit payload (production-viable), but the payload
store is **not atomic with the depth CAS** — a closer fragment from another
workgroup can land its depth between our successful CAS and our payload write,
leaving payload mismatched for that pixel for one frame.

Measure for B:
1. **Correctness / error rate** — % of pixels whose payload disagrees with the
   final depth winner vs HW ground truth. Expected tiny (only exact sub-pixel
   contention) but must be quantified; visible cracks are unacceptable.
2. **Perf** — the CAS spin adds contention cost under overdraw. Measure vs A and
   vs HW.

**Forward-progress caveat:** WebGPU gives no cross-workgroup forward-progress
guarantee, so a *per-pixel spinlock* (hold lock, write both words, release) is
unsafe — it can deadlock. B deliberately avoids a lock: the CAS loop only spins
on its *own* retry and always terminates (depth is monotonic), accepting the
rare payload race instead of locking. A lock-based race-free variant is out of
scope for the spike.

### HW-raster baseline

Same triangle soup through a minimal pipeline writing `(depth, payload)` to
attachments (mirrors today's geometry pass). Gives the throughput +
ground-truth payload image both encodings are compared against.

### API anchors (renderer-core, verified)

- Device: `AwsmRendererWebGpuBuilder::new(gpu, canvas).with_device_request_limits(DeviceRequestLimits::max_all()).build()` → `AwsmRendererWebGpu { device, .. }`.
- Compute: `compile_shader`, `create_compute_pipeline` (async), `create_bind_group_layout`, `create_bind_group`, `create_command_encoder` → `begin_compute_pass` → `dispatch_workgroups` → `submit_commands`.
- Buffers: `create_buffer`, `write_buffer`, `new_copy_and_extract_buffer` for readback.
- All in `packages/crates/renderer-core/src/{methods.rs,renderer.rs,buffers.rs,command/compute_pass.rs}`.

---

## Phase 3 — Hybrid rasterization (only if Phase 0 is GO)

Route clusters/triangles by screen size:
- **HW-raster path** (large triangles): the `lod.md` Phase B path — compute
  builds a compacted index stream → single `drawIndexedIndirect` → existing
  geometry fragment shader writes the vis buffer.
- **SW-raster path** (sub-pixel triangles): compute rasterizer using the Phase 0
  emulated atomic, writing `(depth|payload)` to a storage target, then a resolve
  pass merges it into the **same** visibility-buffer textures the material
  passes already read.

Both paths converge on one visibility buffer so `material_prep` /
`material_opaque` keep working unchanged.

---

## Phase 5 — Streaming (virtual geometry)

Page-based cluster residency, analogous to virtual texturing. Independent of the
SW rasterizer; kept here because it is the other large, deferrable Nanite bet
that LOD shipping does not require.

- GPU feedback buffer marks the cluster pages the LOD cut needs this frame.
- CPU streams requested pages from disk/network into the GPU pools and evicts
  cold pages (LRU against a VRAM budget — ties into
  `PERFORMANCE_OPEN_WORLD_PLAN.md`).
- LOD selection clamps to resident pages until higher-detail pages arrive.

The cost that scales with "distinct meshes" is memory/streaming (N unique
datasets resident vs one when instanced); this is what bounds the working set to
the sum of visible LOD cuts.

## Verification

- Bake a multi-million-triangle reference asset; compare SW-raster vs HW-raster
  for visual parity (payload-image diff + screenshots via chrome-devtools MCP).
- Cross-check the vis buffer via the existing GPU picker compute path.
- Stress with `?stress=N` / `?trace=sub-frame`; no per-frame heap allocs in the
  hot path.
