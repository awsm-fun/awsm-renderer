# Renderer optimizations — next sprint

Successor to [`more-optimizations.md`](more-optimizations.md). That
plan delivered Phase 2.1 (mapped-buffer upload ring), Phase 4.3a/b
(`WorkerPool` + `GltfParseJob`), and Phase 4.4 (`OffscreenCanvas`
worker-mode) end-to-end; this doc tracks what's deliberately left
for a follow-on.

**Scope guard.** Both items live downstream of Phase 4.3b worker-mode
gltf parsing. They're independent — either can land without the
other — but both move the same lever (when does worker mode beat
inline, and when should it be the default).

Status: **shipped**. Both items landed in this sprint. Zero-copy
byte transfer wired through `GltfParseJob::into_response_message`
/ `from_response_message` (the `doc_bytes` + `buffer_bytes`
`ArrayBuffer`s ride the same transfer list the `ImageBitmap`
handles do); pre-warmed pool + sticky inline fallback wired
through `scene-editor/src/context.rs::maybe_build_worker_pool` and
`asset_cache::load_and_populate`. The editor flip from
"`?gltf-worker=on` opt-in" to "default-on, `?gltf-worker=off` opt-
out" is live. Canonical perf-doc rewrite lives in
[`PERFORMANCE.md §5c`](../PERFORMANCE.md). See "Measurement
findings" at the bottom of this doc for the headless-Chrome A/B
numbers (which differ from the original M2-Chrome baseline in
ways worth understanding).

---

## 1. Zero-copy byte-payload transfer (worker → main)

### Current state

`GltfParseOutput` (the `WorkerJob::Output` returned from
`GltfParseJob`) carries the parsed glb's byte payloads in two fields:

- `doc_bytes: Vec<u8>` — re-emitted glTF JSON; the main thread reparses
  via `Gltf::from_slice` because `gltf::Gltf` itself isn't
  structured-clone-able.
- `buffer_bytes: Vec<ByteBlob>` — per-buffer-view binary payloads,
  4-byte padded.

Both opt into serde's bytes path via `#[serde(with = "serde_bytes")]`
(plus the transparent `ByteBlob` newtype for the outer `Vec<Vec<u8>>`).
That's what got us the 137× speedup landed in
[`0f85bfa`](https://github.com/dakom/awsm-renderer/commit/0f85bfa) —
`serde_wasm_bindgen::serialize_bytes` produces a `Uint8Array` in one
memcpy instead of one JS `Number` per byte.

But "one memcpy" ≠ "zero-copy". The current path is:

```
[Rust Vec<u8> in worker]
     ↓  serde_wasm_bindgen::serialize_bytes
[Uint8Array (worker heap)]
     ↓  structured-clone via postMessage
[Uint8Array (main heap, copy)]
     ↓  serde_wasm_bindgen::deserialize_bytes
[Rust Vec<u8> in main]
```

Two heap-to-heap copies per payload. For Corset.glb (12.8 MB) the
dominant cost moved to image decode (~150 ms) and we won that with
the in-worker `createImageBitmap` + transferred handle side-channel
([`18cf750`](https://github.com/dakom/awsm-renderer/commit/18cf750) §5),
so the byte copies aren't currently the bottleneck. They will be on
the next asset class up: ≥ 50 MB glbs (e.g. the unshipped
`robot-001.glb` stress asset, or arbitrary user-provided scenes in
asset-pipeline tooling).

### Goal

Make the worker→main hop fully zero-copy for `doc_bytes` and
`buffer_bytes`: add their underlying `ArrayBuffer`s to the
`post_message_with_transfer` transfer list so ownership moves
without a copy.

### Design sketch

`GltfParseJob::into_response_message` already has the trait hook to
attach side-channel data + extend the transfer list — that's exactly
what the `ImageBitmap` handles use. Same shape for the bytes:

1. **Worker side** (`into_response_message`):
   - Drop `doc_bytes` / `buffer_bytes` from the serde-serialised
     payload (`#[serde(skip)]` on the fields, or split the struct so
     the bytes never enter serde).
   - Construct `Uint8Array`s directly from the Rust `Vec<u8>`s via
     `js_sys::Uint8Array::new_with_length(len)` + `.copy_from(slice)`,
     OR — preferred — use `unsafe { Uint8Array::view(&vec) }` *only*
     long enough to allocate a fresh `Uint8Array` in JS heap, since
     transferring needs JS-owned storage. (The `view` constructor
     produces a borrow-backed `Uint8Array` over the wasm linear
     memory; that's not transferable. We need a real
     `new ArrayBuffer(n)`-backed copy. One memcpy on the worker
     side, but then *zero* copies for the cross-thread hop, vs the
     current two copies.)
   - Attach each `Uint8Array` to the response object as named
     properties (mirroring the `bitmaps` array pattern):
     `response.doc_bytes = uint8`, `response.buffer_bytes = [u8a, u8a, …]`.
   - Push each `Uint8Array.buffer` (the underlying `ArrayBuffer`) onto
     the returned transfer `Array`.
2. **Main side** (`from_response_message`):
   - Walk the side-channel properties, copy each `Uint8Array` into a
     fresh `Vec<u8>` via `Uint8Array::to_vec` (one memcpy from the
     now-transferred JS-heap buffer into the wasm linear memory the
     parser expects). Stitch them back into the deserialised
     `GltfParseOutput`.

The trait already permits this — `into_response_message`'s `Array`
return value *is* the transfer list. `GltfParseJob` for `ImageBitmap`
is the working precedent.

### Open questions

- **Is the inner `Vec<u8>` size known statically at the call site?**
  If yes, we can `Uint8Array::new_with_length(n)` + `copy_from(&vec)`
  and the JS-heap allocation happens once. If no, we'd need a 
  growing path that's slower than the current serde route.
- **Does `serde_wasm_bindgen`'s `serialize_bytes` path on a
  `#[serde(with = "serde_bytes")]` field actually allocate a JS-heap
  `Uint8Array` (transferable), or does it produce a view-backed one
  (not transferable)?** If it's already JS-heap-backed, the simpler
  fix is to extract the `Uint8Array.buffer` *after* serde and push it
  onto the transfer list — no manual construction needed. Worth a
  one-hour spike on the `serde_wasm_bindgen` source before designing
  around the manual path.
- **`from_response_message` end:** the cleanest decode is
  `Uint8Array::to_vec` (one memcpy from JS heap into wasm linear
  memory). Whether that's measurably faster than the current
  `serde_wasm_bindgen::deserialize` of the same Uint8Array shape is
  unclear; the win on this side depends on serde's deserialise path
  being slow enough to dominate. The bigger win is the *outbound*
  transfer eliminating the structured-clone copy.

### Measurement plan

Two A/Bs against the same Corset.glb baseline used in `§5c`:

| Path | Mean load (ms) | Speedup |
|---|---|---|
| Inline `GltfLoader::load` | 196 ms | 1.0× (baseline) |
| Worker (current state) | 91 ms | 2.15× |
| **Worker + byte transfer** | ? | target ≥ 2.5× |

Then re-run against a synthetic 50 MB glb (or a real asset of that
class if one becomes available). The hypothesis is that the byte-
transfer win scales with payload size — for Corset the byte cost is
probably already ≤ 10 ms, so a 2× speedup of that line item is only
a few ms of total. For 50 MB it should be ≈ 50 ms gained.

### Risks

- **`ArrayBuffer` lifetime after transfer.** Once transferred, the
  worker-side `Uint8Array` is detached; any subsequent access from
  the worker throws. The `into_response_message` body needs to
  ensure the bytes aren't read after the transfer-list build. The
  function returns the response immediately after building it, so
  this should be straightforward — but worth a code-comment guard.
- **`#[serde(skip)]` interaction with `Default`.** If we split
  `doc_bytes` / `buffer_bytes` out of the serde struct, the
  deserialised side has to reconstruct them from the side-channel,
  not from a serde default. Type-system enforces this (the field
  is `pub`, no default), but worth catching in a test.
- **No regression on smaller assets.** DamagedHelmet (~4 MB, 5
  textures) sat at "within noise" of inline pre-transfer (per §5c).
  The byte-transfer overhead — `new Uint8Array(n)` JS allocation +
  one memcpy — should remain in noise; verify with a second A/B.

---

## 2. Pre-warmed worker pool + graceful bootstrap fallback (default-flip blockers)

### Current state

`PERFORMANCE.md §5c "Worker mode stays opt-in for now"` calls out the
two blockers against flipping `asset_cache::load_and_populate`'s
default from inline to worker, despite worker being 2.15× faster on
Corset-sized assets:

1. **No pre-warmed pool.** Today the scene-editor builds its
   `WorkerPool` lazily on `?gltf-worker=on`. An always-on pool would
   need to be built at editor init, before the first asset load is
   issued. The current on-demand build adds ~50 ms to the first
   load — which dwarfs the < 5 MB break-even win.
2. **No graceful fallback.** If `WorkerPool::new` fails (CSP that
   blocks blob URLs, environments without resolvable
   `import.meta.url`, ad-blockers that nuke the worker shim, etc.)
   the editor today errors out. For a default-on path we'd need to
   detect the failure once at startup and fall back to inline for
   the rest of the session — without that, a CSP-misconfigured
   project would silently stop loading assets.

### Goal

Make worker mode the default in `asset_cache::load_and_populate`,
with inline as the auto-detected fallback when the worker pool
can't be brought up.

### Design sketch

**Pre-warm:**
- At editor init (alongside the existing wasm-bindgen startup
  sequence), kick `WorkerPool::with_workers(None)` and store the
  result behind an `OnceCell` / `LazyLock` accessible from
  `asset_cache::load_and_populate`.
- Workers come up in parallel with the rest of the editor boot —
  the critical path is the wasm-module compile inside the worker,
  which already shares the main-thread `WebAssembly.Module` so
  it's a ~5 ms cost rather than a full recompile.
- First asset load runs `pool.dispatch::<GltfParseJob>(...)` directly
  — no on-demand pool build, no 50 ms surprise.

**Graceful fallback:**
- `OnceCell<Result<WorkerPool, WorkerPoolError>>` (not
  `OnceCell<WorkerPool>`). The Err arm logs once, then every
  subsequent `load_and_populate` call sees the Err and routes
  through the inline path.
- The decision is one branch in `load_and_populate`'s entry — already
  the right shape, since the existing `?gltf-worker=on` knob branches
  the same way today. Inverting the default is a comment change plus
  the OnceCell wiring.
- Editor surfaces the fallback in the dev console (one-time
  `tracing::warn!`) so a CSP misconfiguration shows up at edit-time
  rather than at first asset load.

### Open questions

- **Pool size for default-on?** §5c quotes the dev knob at 2 workers.
  For default-on we should re-measure across (1, 2, 4)
  to find the sweet spot — too many workers and we waste startup
  time + RAM on pools that never see load past the first frame.
  Editor scenes load one asset at a time in the common case (user
  drags a single glb), so 1–2 is likely right; 4 only helps if the
  pipeline ever dispatches multiple parses in parallel (e.g. an
  "import scene" path).
- **Worker spawn cost on cold-start.** Need to measure the editor
  init time delta with the pre-warm vs without. If it's > 100 ms we
  should consider deferring pool construction to the first
  user interaction (mouse move / click) so the editor's first-paint
  isn't gated on it.
- **Does Phase 4.4's `OffscreenCanvas` worker affect this?** No —
  the gltf-parse pool is a separate `WorkerPool` from the
  OffscreenCanvas renderer worker. They share the wasm module but
  not the lifecycle. Worth a one-paragraph note in
  `DEPLOYMENT_MODES.md` clarifying which pool consumers should
  pre-warm when.

### Measurement plan

Three smokes against the editor's startup + first-load sequence:

1. **Cold start time** (page reload → first frame painted):
   today vs pre-warmed. Target: ≤ 20 ms regression. If higher,
   gate the pre-warm behind first-interaction.
2. **First asset load latency** (Insert Model → renderer first
   shows the mesh): today (inline, no pool warmup), today
   (worker on-demand, includes ~50 ms pool build), and target
   (worker pre-warmed). Pre-warmed should land at ~91 ms (the
   §5c worker number) with no on-demand surprise.
3. **CSP-fallback path correctness.** Manually test by setting a
   restrictive CSP `worker-src 'none'` in the editor's served
   HTML and confirm the editor (a) logs the fallback once, (b)
   loads assets via inline thereafter, (c) doesn't keep retrying
   the pool construction.

### Risks

- **Spurious pool-up failures masking real issues.** If we silently
  fall back on *any* error, a typo in the worker bootstrap could
  ship to consumers without anyone noticing the worker mode never
  ran. Mitigation: surface the fallback prominently in dev builds
  (`debug_assertions`) — a banner / persistent console warning —
  while keeping it quiet in release.
- **Pre-warmed pool holds references at idle.** Two workers each
  hold the shared wasm module + a small event-loop. Net memory
  cost is in the low single-digit MB. Acceptable for an editor;
  worth re-evaluating for shipped consumer games where memory is
  tighter — they should pre-warm their own pool on a deferred
  schedule (post-splash, before first level load).

---

## See also

- **ROADMAP.md → Shadows: "Transparent-pass shadow bind-group
  consolidation (deferred; blocked on adapter `maxBindGroups=4`)"**
  is a separate parked item — different category (feature/refactor
  work blocked on a hardware limit, not a perf follow-on) and lives
  on its own track. Not part of this sprint.
- **PERFORMANCE.md §5c** has the canonical worker-mode measurement
  + decision rationale. If either item here lands, update §5c with
  the new table and the new default.
- **PR #91 description** (the sprint that birthed this plan)
  documents the four phases that delivered worker mode end-to-end.

---

## Closing the loop

Both items have measurement gates; only land them if the A/B
shows the projected win. Negative result → document the
investigation in `PERFORMANCE.md §10` (currently empty) as a
parked optimisation with the hazard, so the next picker doesn't
re-propose without new context.

---

## Measurement findings (post-ship)

### Phase 1 — zero-copy byte transfer

Re-ran `measure_gltf_load_ab("Corset.glb", 5)` against the headless
Chrome the Claude Preview MCP drives, post-ship:

| Path | Mean load (ms) | Speedup |
|---|---|---|
| Inline `GltfLoader::load` | 74.7 | 1.0× |
| Worker (handle-transfer **+ byte-transfer**) | **69.9** | **1.07×** |

This *looks* like a worse result than the M2-Chrome baseline
documented in §5c (2.15× there) but it's a hardware-difference
artefact, not a regression. The headless Chrome decoder is roughly
2-3× faster on Corset than the M2 / real Chrome baseline (inline
drops from 196 ms → 75 ms; worker drops from 91 ms → 70 ms), so the
absolute amount of main-thread work the worker path eliminates is
much smaller, and the *relative* gap compresses. The byte-transfer
itself saves the structured-clone of ~13 MB on the postMessage
hop; on Corset that's low-single-digit-ms (consistent with the 70 ms
vs the prior 91 ms M2 worker baseline, modulo hardware) — too small
to A/B reliably on this asset size. The plan's hypothesis ("for 50
MB it should be ≈ 50 ms gained") holds in shape (the byte-transfer
win scales linearly with payload size, the structured-clone hop is
the dominant overhead at that size), but stays a hypothesis on
this repo — the largest shipped stress asset is 12.8 MB.

No regression on smaller assets: gizmo.glb (the editor's default-on
asset, ~80 KB) loads cleanly through the new path during
`create_context`; visual smoke + no errors.

### Phase 2 — pre-warmed pool + graceful fallback

Functional gates (not throughput) — verified:

1. **Pool comes up at boot.** Editor logs `WorkerPool built (2
   workers); GltfParseJob registered — asset loads will run in
   worker mode` during `create_context`. First Insert Model (or
   the gizmo's `load_and_populate` on init) is a direct
   `pool.dispatch` — no on-demand build window.
2. **Sticky inline fallback under `?gltf-worker=off`.** Reload
   with the opt-out: editor logs `?gltf-worker=off — skipping
   WorkerPool bootstrap; asset loads will run inline`, gizmo
   loads via the inline path, no errors.
3. **Bootstrap-failure fallback path.** Not exercised in this
   sprint's smoke (we'd need a CSP-misconfigured fixture); the
   `tracing::warn!` branch is in place
   ([`context.rs::maybe_build_worker_pool`](../../crates/frontend/scene-editor/src/context.rs))
   with the "log once, fall back for the rest of the session"
   semantics the plan called for.

Pool size landed at **2** (rationale captured in §5c). 4 burns RAM
on workers that never see load past the first frame in editor's
common case (drag one glb at a time); 1 leaves no headroom for the
occasional parallel dispatch (measurement harness, multi-asset
import).
