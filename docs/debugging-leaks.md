# Debugging memory leaks (browser + WebGPU + wasm)

A reusable playbook for finding slow memory/address-space leaks in a
browser-hosted renderer — the class of bug that takes minutes-to-hours to
manifest, often ends in a hard `Aw, Snap!` renderer trap, and is invisible to a
casual glance at the JS heap. The techniques generalize to any long-running
WebGPU/wasm app; nothing here is specific to one subsystem.

The golden rule: **a leak lives in exactly one of a handful of memory pools, and
each pool is visible to a different tool. Find the pool first, the call site
second.** Most wasted time comes from staring at the wrong metric.

---

## 1. The memory model — what each metric sees, and what it misses

A browser tab spreads memory across several pools that no single number covers:

| Pool | What lives there | How to read it | Blind to |
|---|---|---|---|
| **JS heap** | live JS objects | `performance.memory.usedJSHeapSize` (Chrome), CDP `HeapProfiler` | native, GPU, wasm, *detached/external* buffers |
| **wasm linear memory** | Rust/C++ compiled to wasm | `WebAssembly.Memory.buffer.byteLength` | everything outside the wasm arena |
| **Native C++ heap** | renderer/Blink objects, ArrayBuffer backing stores | OS RSS; macOS `vmmap` (PartitionAlloc regions) | logical intent — you see bytes, not which object |
| **V8 page allocator** | V8-managed pages incl. *external* ArrayBuffer pages | crash-dump `page-allocator-mapped-size`; macOS `vmmap` (V8 regions) | **not** `usedJSHeapSize` — mapped ≠ used |
| **GPU objects** | buffers, textures, bind groups, mapped ranges | your own create/destroy census; GPU-process RSS | lazily freed on GC, so counts lag |
| **Virtual address space** | reservations (may be 0 resident) | `ps vsz`, `vmmap` VIRTUAL | resident growth *within* a fixed reservation |

Two traps that burn hours:

- **`usedJSHeapSize` is not "how much memory the tab uses."** It is live JS
  objects only. A tab can map tens of GB of *external* ArrayBuffer pages (e.g.
  WebGPU `getMappedRange` results) while `usedJSHeapSize` sits flat at a few MB.
- **Virtual size ≠ committed size ≠ resident size.** A leak can grow *resident*
  and *region count* while *virtual* stays pinned at a huge constant reservation
  (allocators reserve address space up front and commit into it). Watching only
  virtual size (or `ps vsz`) makes a real leak look flat.

---

## 2. Step 0 — read the crash dumps first

If the leak ends in a crash, the dumps already narrow the search for free.

- macOS Crashpad minidumps: `~/Library/Application Support/Google/Chrome/Crashpad/completed/`.
- Look at, across many dumps:
  - **Which process** — renderer vs GPU vs browser. A uniform signature across
    dumps means a deterministic ceiling, not random corruption.
  - **Exception type** — `EXC_BREAKPOINT (0x6)` is a *deliberate* trap
    (PartitionAlloc/V8 `IMMEDIATE_CRASH`, wasm `unreachable`, OOM handler), not a
    wild segfault. That tells you an allocator hit a limit, not that memory got
    scribbled.
  - **Annotations** — `page-allocator-mapped-size` and friends. A byte-identical
    value across dumps is a hard ceiling being hit repeatedly. That number *is*
    your leak's destination; note which allocator owns it (see §5).

Turn the dump into a rate: `ceiling ÷ time-to-crash` → bytes/sec → bytes/frame.
That estimate tells you whether to suspect a per-frame path (small, steady) or a
per-event path (bursty).

---

## 3. Step 1 — in-page instrumentation (cheap, permanent)

Add a queryable census that a soak can sample. Surface, at minimum:

- `performance.memory` (used/total/limit JS heap).
- `WebAssembly.Memory` byte length (the wasm arena — the unbounded-growth pool
  for Rust/C++-in-wasm).
- Live counts of every pooled resource you own (meshes, textures, pipelines,
  bind groups, materials, …). A leak in any of these shows as a monotonic count.
- **Cumulative allocation counters** at the choke points you control (e.g. a
  central `create_buffer`/`create_texture` wrapper): a running count + bytes.

Two principles that save you from false signals:

- **Prefer cumulative (increment-only) counters over "live" counters when there
  is no central free path.** If frees are scattered (or handles are released by
  GC with no explicit destroy), a decrement-based "live" count drifts and
  *manufactures* a fake leak. A cumulative *creation* count is unambiguous: its
  **slope** over a soak tells you the creation rate; cross-referenced with an OS
  metric it separates "creating too much" from "creating fine, freeing nothing."
- **Gate hot-path counters behind a debug/diag cfg** so release builds carry zero
  always-on cost, but keep the *accessor* defined (reads zero) so downstream code
  compiles either way. Soaks run a dev build, so they still see the numbers.

Add a `?memlog=N` URL flag (or equivalent) that logs the whole census as one
parseable line every N seconds. A soak then leaves a durable console trail even
if live sampling drops frames or the driver dies.

---

## 4. Step 2 — the overnight soak harness

The point of a harness: reproduce a multi-hour leak unattended and capture the
curve, **without a human (or an agent) in the loop** — so an 8-hour run costs
nothing. Do **not** drive the browser by hand-issuing automation calls every 30s;
write a standalone process.

Shape (see `tools/soak/` for a zero-dependency Node/CDP implementation):

1. **Launch a dedicated browser instance** — its own `--user-data-dir` and
   remote-debugging port, so it never touches the user's profile. Non-headless is
   the safe default for WebGPU.
2. **Defeat throttling.** An idle/occluded tab gets its render loop throttled to
   ~1fps, which changes the leak rate. Launch with
   `--disable-background-timer-throttling --disable-backgrounding-occluded-windows
   --disable-renderer-backgrounding` so the idle loop runs at full rate — the
   crash pattern is usually "idle but still rendering."
3. **Load a representative scene and leave it idle.** A second variant can replay
   a small interaction loop (orbit, select, undo/redo) each minute to cover
   interactive paths (picker readbacks, command churn).
4. **Sample every 30s**: the in-page census (via a CDP `Runtime.evaluate` of your
   query export) **plus** OS metrics on the target process (§5).
5. **Write both CSV (curated, stable columns) and JSONL (the full census,
   lossless).** CSV is for eyeballing/plotting; JSONL guarantees you never wish
   you'd logged a field you didn't.
6. **Detect the crash and exit gracefully** — CDP `Inspector.targetCrashed`, a
   WebSocket disconnect, or the target process vanishing. Record time-to-crash
   and the last N samples.
7. **Add a safety cutoff** (e.g. end the run if RSS exceeds half of machine RAM)
   so an unattended fast runaway can't thrash the box overnight.

**Pass criterion:** every counter and the OS mapped-size FLAT (< a few MB/hour
drift) over the target duration. Any monotonic series names the subsystem.

---

## 5. Step 3 — OS-level sampling (where the truth is on native/GPU leaks)

When in-page counters are flat but the process still grows, the leak is native,
GPU, or in an allocator's mapped pages. On macOS:

- **`ps -o rss=,vsz= -p <pid>`** — resident and virtual size, always available.
  Cheap; sample it every tick. RSS is the practical "is it growing" signal.
- **`vmmap --summary <pid>`** — a per-region-**type** table. Read the individual
  rows and, critically, the **region COUNT** column, not just totals.
  - ⚠️ **Rounding pitfall:** the `TOTAL` line rounds to ~0.1T. A 70GB leak buried
    inside a multi-TB constant reservation is invisible in `TOTAL` — it stays
    byte-identical while the leak rages. Never trust the summary TOTAL as your
    leak metric. Watch per-region **RESIDENT** and **region COUNT** instead.
- **`vmmap <pid>`** (full, non-summary) — every individual region with address,
  size, permissions, and share mode (`SM=PRV/ZER/NUL/COW`). This is how you
  identify *what* leaks: a leak shows as thousands of small (16–64KB) committed
  regions accumulating in one arena.
- **Sample every process of the instance** (browser / gpu-process / renderer /
  utility), not just the one with the highest RSS. The leaking process for the
  *virtual/mapped* metric may differ from the one that looks big in RSS.

### Identifying the arena: macOS VM tags → Chromium allocators

`vmmap` labels unknown application-specific VM tags as `Memory Tag NNN`. Chromium
tags its allocator pages via `base::PageTag` (see
`partition_allocator/.../page_allocator.h`):

| VM tag | Allocator |
|---|---|
| **252** | BlinkGC |
| **253** | PartitionAlloc (Chrome C++ heap, ArrayBuffer backing stores) |
| **254** | Chromium (general) |
| **255** | V8 (JS heap + the *page allocator* the crash annotation names) |

So a `Memory Tag 255` region count/resident that climbs monotonically **is** the
crash dump's `page-allocator-mapped-size` growing. Tags 253+255 rising together,
with `usedJSHeapSize` flat, is the fingerprint of **external/detached ArrayBuffer
pages** (e.g. WebGPU buffer mappings) that are allocated per frame and never
returned to the OS.

**Analysis:** diff an early dump against a late one, per region, on the RESIDENT
column and the COUNT. The row(s) that grew name the pool. Confirm the growth is
**linear** (unbounded leak) vs **plateauing** (first-load settling) by plotting
the series across all dumps.

---

## 6. Interpretation — a decision tree

```
process grows over a soak?
├─ NO  → not reproduced (wrong scene? throttled tab? too short?) — re-check §4.2
└─ YES → which pool?
   ├─ wasm_heap_bytes grows        → Rust/C++ leak in the wasm arena
   │                                  (unbounded Vec/cache, un-dropped closure)
   ├─ usedJSHeapSize grows (not     → JS leak: retained listeners, signal graphs,
   │   just sawtooth)                 growing log/undo buffers, closures
   ├─ a resource COUNT grows        → that subsystem leaks objects; audit its
   │   (textures, pipelines, …)       create/destroy pairing
   ├─ create_* cumulative slope     → per-frame creation of that resource;
   │   is steep, count follows        find the caller minting it each frame
   └─ ALL in-page counters flat,    → native / GPU / mapped-page leak.
      but RSS and/or vmmap region     Go to §5: which VM tag / region grows?
      count grow                      Linear region-count growth with
                                      create_buffer flat ⇒ per-frame VM mappings
                                      (getMappedRange / mapAsync / transient GPU
                                      objects) whose pages never return to the OS.
```

Note the sawtooth: `usedJSHeapSize` bouncing between two bounds is healthy GC, not
a leak. Look at the trend of the *lows*, not the peaks.

---

## 7. Step 4 — pinpoint the call site

Once the pool is known:

- **wasm heap:** take two CDP `HeapProfiler.takeHeapSnapshot` samples far apart
  and diff retainers. Look for a per-frame closure/subscription that is registered
  but never dropped, or a cache that only grows.
- **A resource count / cumulative counter:** grep the create path; audit every
  create for a matching destroy on *every* branch (error paths, early returns,
  resize). wasm-bindgen handles do **not** free GPU/native memory promptly on GC —
  transient GPU objects (bind groups, command encoders, query sets, readback
  staging) must be explicitly dropped/`destroy()`d each frame, not left to GC.
- **Mapped-page / native leak (all in-page flat):** the culprit is a per-frame
  WebGPU operation on *existing* objects — `map_async` / `getMappedRange` /
  `queue.writeBuffer`. Audit the map/unmap pairing: every `map_async`+
  `getMappedRange` needs an `unmap` on every path, including the ones where an
  `await` or `?` bails between map and unmap. Watch for a mapped `ArrayBuffer` /
  `Uint8Array` view captured by a closure that outlives the frame — a retained
  view keeps the buffer mapped, so the pages never free.
- **Ablation A/B (decisive when reading code is ambiguous):** disable one
  suspected per-frame map/readback source and re-run a short soak. If the region
  growth rate drops, you found it. Toggle them one at a time. This turns a
  guess into a measurement.

---

## 8. WebGPU + wasm gotchas (the ones that actually bite)

- **GC does not free GPU/native memory promptly.** A dropped wasm-bindgen handle
  frees the *wrapper* when the JS GC eventually runs; the underlying GPU
  allocation/mapping lingers until then and may effectively leak under per-frame
  pressure. Call `destroy()` / `unmap()` explicitly.
- **`getMappedRange` allocates real pages.** Each call returns an ArrayBuffer
  backed by mapped memory (V8 + PartitionAlloc pages). Even after `unmap`
  detaches it, those pages may not be returned to the OS immediately — so mapping
  every frame, or mapping when nothing is dirty, accumulates address space.
  Prefer mapping only when there is data to move; consider persistent-mapped or
  double-buffered staging over per-frame map/unmap.
- **`create_buffer` flat + memory still growing** ⇒ the leak is not *new*
  buffers; it's mappings or transient objects on existing ones. Don't chase
  buffer creation.
- **A fixed-depth staging ring can still starve.** A climbing writeBuffer-fallback
  counter with a flat peak-ring-depth means the ring is too shallow for the
  upload rate (a perf issue), not necessarily a leak — separate the two.

---

## 9. Cheat sheet

```bash
# OS metrics on a target PID
ps -o rss=,vsz= -p <pid>                     # resident / virtual (KB)
vmmap --summary <pid>                        # region-TYPE table (watch COUNT + RESIDENT, not TOTAL)
vmmap <pid>                                  # full: every region, size, SM= share mode
vmmap <pid> | grep '^Memory Tag 255'         # V8 page-allocator regions (the crash annotation)
vmmap <pid> | grep '^Memory Tag 253'         # PartitionAlloc regions

# find every chrome process of one launched instance
pgrep -f "<user-data-dir>"                   # then ps -o command= to read --type=

# diff two full/summary dumps per region on RESIDENT + COUNT to name the pool
```

- Reproduce unattended with a standalone CDP soak (`tools/soak/`), never by hand.
- Sample in-page census **and** OS metrics together; the disagreement between them
  is the diagnosis.
- Watch **region count** and **RESIDENT**, not rounded virtual totals.
- Confirm **linear vs plateau** before calling something a leak.
```
