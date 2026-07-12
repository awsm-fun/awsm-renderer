# Editor tab crashes — investigation plan

**STATUS: PLANNED (not started).** Written 2026-07-12 after the overnight
crash `a2a9d5bb-0e97-4eef-ad99-f0d85d4ca951`.

## What the crash dumps already tell us

Local Crashpad minidumps (`~/Library/Application Support/Google/Chrome/
Crashpad/completed/`) hold 56 reports; **35 are the editor tab**
(`localhost:9085`). Every single editor crash has the SAME signature:

- **Process**: Chrome *renderer* process (never the GPU process).
- **Exception**: `EXC_BREAKPOINT (0x6)` — a deliberate trap
  (PartitionAlloc/V8 `IMMEDIATE_CRASH`, wasm `unreachable`, or OOM handler),
  never a wild segfault.
- **The smoking gun**: the `page-allocator-mapped-size` annotation reads
  **70,115,999,744 bytes (70.1 GB) in every dump that carries it** (7/8 of
  the recent ones — byte-identical value). That is a hard address-space
  ceiling being hit, not a random corruption: something maps virtual memory
  monotonically until V8's page allocator can't reserve another page and
  traps.
- **Timing**: crashes cluster at idle/overnight hours (00:17, 01:40, 02:08,
  06:38, 09:03…) — the tab dies after HOURS of accumulation, often with no
  user interaction. An idle editor still runs its render loop, so per-frame
  paths are prime suspects.

So the working model is: **a slow, unbounded virtual-address-space leak in
the editor page, ~hours to exhaust a ~70 GB budget, ending in a uniform
allocator trap.** Rough rate math: 70 GB over ~6 idle hours ≈ 3.3 MB/s ≈
~55 KB per frame at 60 fps — well within the size of one leaked staging
mapping, readback buffer, or ArrayBuffer per frame (or a larger one every
few frames).

## Suspects, ranked

1. **Mapped-buffer churn in the render loop** — `mapped_staging_ring` /
   `mapped_uploader` (meta uploads, skins, morphs, transforms) map/unmap
   every frame. A path that maps without unmapping on some branch (error
   path, zero-dirty frame, resize) leaks VA at exactly this rhythm. WebGPU
   `mapAsync` reservations live in the renderer process.
2. **Readback machinery** — picker readbacks, GPU-cut census readbacks,
   `?trace` timing queries, screenshot/`scene_png` paths: each creates a
   mappable staging buffer. Axis-8 deliberately left "readback future
   ownership moves" unpooled.
3. **wasm memory growth** — the editor wasm memory only grows, never
   shrinks; combined with wasm-bindgen externref/closure leaks (e.g. a
   per-frame closure registered but never dropped) the wasm heap can climb
   indefinitely. `WeakRef`/`FinalizationRegistry` diagnostics can see this.
4. **JS-side accumulation** — undo stack bytes (census exists:
   `undo_bytes`), tracing/log buffers, dominator signal graphs holding
   history, MCP link message buffers (the ws stays connected all night).
5. **Repeated pipeline/bind-group recreation** — lazy-compile kick/poll or
   AA flips re-creating GPU objects each frame without releasing old ones
   would show as GPU-process growth mirrored by renderer-side wrappers.

## Phase 1 — instrument (cheap, permanent)

- Extend `memory_stats` with the numbers that matter for THIS bug:
  `performance.memory.usedJSHeapSize/totalJSHeapSize`,
  `WebAssembly.Memory` byte length, count of live GPU buffers/textures
  (renderer already tracks buffers — surface a count + total bytes),
  mapped-staging-ring stats (maps issued vs unmapped — the uploader already
  has `UploadStats`; add outstanding-map count), readback-buffer
  live-count, undo/redo bytes (exists), MCP ws buffered bytes.
- Add a `?memlog=N` flag: log that census to the console every N seconds so
  a soak run leaves a parseable trail (browser console persists in the CDP
  log; renderer tracing lands there too).
- One-shot `estimate()` from `navigator.storage` + `performance.measureUserAgentSpecificMemory()`
  (if available in the profile) for cross-checks.

## Phase 2 — overnight soak harness (the ask)

A `task soak` target + `/tmp/drive`-style CDP script that:

1. Launches the standard automation Chrome profile with the editor +
   `?memlog=30`.
2. Loads a representative project (the jetpack arena is ideal: SSR + bloom
   + KTX env + 45 nodes) and leaves it IDLE — that matches the crash
   pattern; a second variant replays a small command loop (orbit camera,
   select/deselect, undo/redo) every minute to cover interactive paths.
3. Samples every 30 s via CDP: `performance.memory`, wasm memory size,
   `Browser.getProcessInfo`-equivalent (`ps` on the renderer PID for RSS +
   `vmmap --summary` for mapped regions — the direct analogue of
   `page-allocator-mapped-size`), plus the Phase-1 census.
4. Writes a CSV + dies gracefully when the tab crashes (CDP disconnect),
   recording time-to-crash and the last N samples.
5. **Pass criterion**: mapped size and every Phase-1 counter FLAT (< a few
   MB/hour drift) over 8+ hours. Any monotonic series names the subsystem.

## Phase 3 — bisect + fix

- The Phase-2 CSV should already name the growing counter. If it's mapped
  staging: audit every `map_async`/`unmap` pairing for early-return paths;
  pool readback buffers. If wasm heap: heap-snapshot diff two soak samples
  (`HeapProfiler.takeHeapSnapshot` over CDP compresses well at idle) and
  look at retainers — likely a per-frame closure or signal subscription.
- If nothing in-page grows but `vmmap` does: suspect WebGPU wrapper objects
  (buffers/textures created per frame and only GC'd lazily) — force
  `destroy()` on transient objects; wasm-bindgen handles don't free GPU
  memory on GC promptly.
- Land the fix + keep `task soak` as a permanent CI-adjacent gate (run
  before releases; the harness prints a one-line PASS/FAIL).
- Re-run the 8 h soak twice: once idle, once interactive. Both must pass.

## Phase 4 — guardrails (independent of the root cause)

- The oversized-GPU-buffer guard (1.9 GB cap) already exists; add the same
  err-not-abort posture to the next allocation seam the soak implicates.
- Consider a low-frequency in-editor watchdog: when
  `usedJSHeapSize`/wasm-memory crosses a threshold, toast + auto-save the
  project (the save-complete guard already refuses lossy saves), so an
  eventual crash never costs work — the crash-at-idle pattern means
  auto-save covers nearly all real losses.

## Non-goals for this plan

- Not chasing the GPU process (zero GPU-process dumps among the 35).
- Not treating this as a WebGPU driver bug: the uniform 70.1 GB ceiling +
  renderer-process trap is an us-shaped leak until proven otherwise.
