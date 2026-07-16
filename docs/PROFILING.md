# Profiling & logging

How to measure renderer frame work — CPU scope timing, GPU pass timing, a live
on-screen HUD, and the DevTools flame chart — **without paying for the
measurement when it's off**. One rule above all:

> **Profiling is a complete no-op when disabled.** Timing is gated at the
> *source*: a CPU scope or GPU timestamp is only ever created when its tier is
> non-`Off`. When off there is no timer, no query set, no allocation, and
> nothing installed in the `tracing` subscriber. Because the gate is a plain
> runtime field on the renderer, it can be flipped live (an editor menu, a
> player hotkey) with the same guarantee.

For what the spans *mean* and what "normal" values look like, see
[PERFORMANCE.md](PERFORMANCE.md). For the leak that a previous always-on version
of this caused (and how it was fixed), see the history section at the bottom.

---

## TL;DR

```
?trace=off|frame|sub-frame      # CPU scope timing → CPU aggregator + HUD
?gputime=off|frame|sub-frame    # GPU timestamp-query timing → GPU aggregator + HUD
?devtools                       # ALSO mirror CPU scopes into the DevTools flame chart
?perfhud                        # show the on-screen perf HUD (independent of the above)
?log=off|error|warn|info|debug|trace   # tracing log-line level (not profiling)
```

Everything defaults **off**. In the editor you don't need URL params at all — the
overflow (`⋯`) menu has runtime toggles (see [Editor menu](#editor-menu)).

| Tier | CPU (`?trace`) | GPU (`?gputime`) |
|---|---|---|
| `off` | no scopes created | no query set, no timestamp writes |
| `frame` | outer `"Render"` scope only | (currently same coverage as sub-frame) |
| `sub-frame` | every pass / GPU-write scope | per-pass device timestamps |

---

## The pieces

### CPU scope timing (`?trace`)

Each instrumented render stage opens a [`CpuScope`](../packages/crates/renderer/src/profiling.rs)
only when `AwsmRendererLogging::cpu` permits. On drop it folds its wall duration
(`performance.now()` delta) into a **bounded rolling aggregator** — `last / ema /
min / max / count` per scope name, keyed by the small fixed set of names, so it
never grows. `?trace=frame` times just the outer `"Render"` scope (≈ whole-frame
CPU build+submit); `?trace=sub-frame` adds every pass and GPU-write scope.

Read it with `awsm_renderer::profiling::cpu_timing_stats()`. The editor also
exposes it in the `memory_stats` MCP query as `cpu_span_timings`.

### GPU pass timing (`?gputime`)

Real GPU pass durations (not CPU submit time) via WebGPU **timestamp queries**.
When `AwsmRendererLogging::gpu` is on and the device has the `timestamp-query`
feature, instrumented passes attach `timestampWrites`; after submit the renderer
`resolveQuerySet`s into a ring-buffered readback, and a few frames later folds
`(end − begin)` nanoseconds into the **GPU aggregator**
(`awsm_renderer::profiling::gpu_timing_stats()`, `memory_stats.gpu_span_timings`).

Instrumented passes: **Geometry**, **Light Culling**, **Material Classify**,
**Material Prep**, **Material Opaque** (unified shade), **SSR**, **Bloom**,
**Effects**, and **Display** — render *and* compute passes. Zero-cost when off:
no query set is created, no timestamp writes are attached.

> **Reading the Display number.** `Display` is the final fullscreen-triangle
> tonemap into the **swapchain** texture. Its timestamp routinely reads a few ms
> even though the shader is trivial — that's the GPU **present / vsync
> backpressure** wait (the frame is vsync-capped, so with only ~1-2 ms of real
> work the GPU idles and that wait lands inside the presenting pass's
> timestamps). It is *not* tonemap cost. Judge real GPU work from the other
> passes; treat `Display` as "present overhead," not compute.

### On-screen HUD (`?perfhud`)

A shared overlay ([`web-shared/src/perf_hud.rs`](../packages/frontend/web-shared/src/perf_hud.rs))
that renders the CPU + GPU aggregators as a compact fixed panel, sampled at 4 Hz.
Its visibility is a runtime flag **independent of whether profiling is
capturing** — `?perfhud` (or the menu) shows it; if no tier is on the tables are
empty with a hint. Hidden = zero cost (the sampling loop only runs while shown).

### DevTools flame chart (`?devtools`)

Opt-in mirror of CPU scopes into the browser User-Timing timeline
(`performance.measure`), so they appear in the DevTools **Performance** flame
chart. Runtime-toggleable. Kept leak-safe by clearing the User-Timing buffer once
per frame (`logger::frame_boundary`, wired into the editor render loop) — a live
recording still captures the entries created during it.

### Logging (`?log`) — not profiling

`?log=…` sets the `tracing` subscriber level for `tracing::{info,warn,…}!` log
lines. Independent of the tiers above. The subscriber installs a console writer +
a bounded capture ring (readable over MCP) and nothing else — no profiling layer.

---

## Editor menu

The editor's overflow (`⋯`) menu → **Profiling…** opens a modal with runtime
controls, no reload needed:

- **CPU timing** — segmented `Off / Frame / Sub-frame` → `AwsmRendererLogging::cpu`.
- **GPU timing** — segmented `Off / Frame / Sub-frame` → `AwsmRendererLogging::gpu`.
- **Perf HUD** — show/hide the overlay (shown over the canvas, top-left).
- **DevTools flame chart** — flip the User-Timing mirror.

The controls reflect the live renderer state and mutate it directly, so you can
turn profiling on, read the numbers, and turn it back off to a genuine no-op —
all in one session. Turning a tier on auto-shows the HUD.

---

## Testing with URL params (editor / model-tests)

Both frontends parse the same params via `LoggingConfig::from_url()`:

```
# editor (:9085) — CPU sub-frame + HUD
http://localhost:9085/?trace=sub-frame&perfhud

# editor — GPU pass timing + HUD
http://localhost:9085/?gputime=frame&perfhud

# editor — CPU scopes in the DevTools flame chart (record a Performance trace)
http://localhost:9085/?trace=sub-frame&devtools

# model-tests (:9080) — same knobs
http://localhost:9080/?trace=sub-frame&perfhud
```

Verify quickly from the console:

```js
// default session accumulates nothing (leak-safe)
performance.getEntriesByType('measure').length          // 0 unless ?devtools

// aggregator populated under ?trace / ?gputime
JSON.parse(await window.wasmBindings.editor_query_json('{"query":"memory_stats"}'))
  // → .cpu_span_timings[], .gpu_span_timings[]
```

---

## Wiring it in a player

Players own their opt-in. Minimal:

```rust
use awsm_renderer_web_shared::logging::LoggingConfig;
use awsm_renderer_web_shared::{logger, perf_hud};

let cfg = LoggingConfig::from_url();   // or build one by hand
logger::init_logger(&cfg);             // console + capture ring + level
cfg.apply_profiling();                 // seed the DevTools mirror
perf_hud::init_from_url();             // seed HUD visibility from ?perfhud

// when building the renderer:
let renderer = AwsmRendererBuilder::new(gpu)
    .with_logging(cfg.renderer_logging())   // Off/Off unless a tier was requested
    .build().await?;

// mount the HUD once near your root DOM, and each frame:
logger::frame_boundary();              // keeps ?devtools bounded (no-op otherwise)
```

Everything above is a no-op unless the player (or its URL) asks for a tier — a
shipping player pays nothing. Players that want their own conventions can skip
`from_url()` and construct `LoggingConfig` / set the tiers directly; the runtime
setters are `renderer.logging.cpu/gpu`, `profiling::set_devtools_measure`, and
`perf_hud::set_visible`.

---

## Extending GPU coverage

Render pass — add `timestamp_writes` to its `RenderPassDescriptor`:

```rust
timestamp_writes: ctx.gpu_timestamps.and_then(|t| t.writes_for("My Pass")),
```

Compute pass — use the descriptor builder helper:

```rust
ComputePassDescriptor::new(Some("My Pass"))
    .with_timestamp_writes_opt(ctx.gpu_timestamps.and_then(|t| t.writes_for_compute("My Pass")))
```

The slot allocator (64 timestamp slots = 32 pass-pairs) hands out a begin/end
pair per call and folds the resolved duration under that name; extras beyond the
budget are silently untimed. Still uninstrumented: per-light **Shadow Generation**
passes and the **Bloom** downsample/upsample chain (only the build dispatch is
timed) — add them the same way if you need that granularity.

---

## Architecture (why it's a no-op when off)

- **Source-gated.** `profiling::cpu_scope(logging, name, sub)` returns `None` when
  the tier is off — no `performance.now()`, no allocation. GPU `writes_for` is
  only reached when `logging.gpu` is on. There is **no** always-installed tracing
  layer; the subscriber does logging only.
- **Renderer-owned aggregators.** CPU + GPU stats are globals in
  `awsm_renderer::profiling`, fed directly by the scopes / readback. `web-shared`
  and the editor read them; nothing is coupled to `tracing`.
- **Runtime-flippable.** The gates are plain fields (`renderer.logging.cpu/gpu`)
  and atomics (`set_devtools_measure`) + a `Mutable` (HUD), so the editor menu
  turns everything on/off live without reinstalling anything.
- **Bounded.** Aggregators are fixed-size (keyed by scope name). The GPU readback
  is a single-buffered ring. The DevTools User-Timing buffer is cleared per
  frame. Nothing grows without bound.

### History — the 70 GB leak

An earlier version installed `tracing_web::performance_layer()` *unconditionally*,
turning every `INFO` span into a `performance.measure`. The renderer emits
per-frame spans and the browser never auto-clears that buffer, so User-Timing
entries accumulated forever → PartitionAlloc grew unbounded → the editor tab
crashed at the ~70 GB page-allocator ceiling after hours. The fix was this
redesign: no always-on layer, source-gated timing, and a per-frame clear for the
(now opt-in) DevTools mirror. If you reintroduce a `performance.measure`/`mark`
per frame, clear it in `frame_boundary` or it will leak again.
