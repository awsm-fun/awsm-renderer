# Profiling & logging redesign

Supersedes the deleted `crashes.md` / `webgpu-churn-leak-perf.md`. The 70GB
editor crash was root-caused (2026-07-15) to `web-shared::init_logger`
unconditionally installing `tracing_web::performance_layer()`: every
`Level::INFO` tracing span became a `performance.measure()`/`mark()` User Timing
entry, and the renderer's per-frame render-timing spans are `INFO`, so ~24
(SubFrame) or ~1 (Frame) UserTiming entries accumulated **per frame forever**
(the browser never auto-clears that buffer) → PartitionAlloc/Tag253 grows
unbounded → tab crash. The stopgap gated `perf_layer` on `?trace`. This plan is
the full re-evaluation.

## Requirements (David)

1. **Zero-cost by default.** Profiling is explicit opt-in; nothing that costs
   per-frame runs unless asked for.
2. **No magic URL inspection** in shared/renderer code. The *editor* toggles via
   URL queries; *players* wire their own opt-in however they like.
3. A **shared URL→config parser** exists so players *can* reuse the editor's
   query conventions easily — but consuming it is their choice.
4. **Leak-safe even when opted-in** — no unbounded growth while profiling.
5. **Best methods**, my call: CPU spans + GPU timestamp queries, each gated by
   opt-in; DevTools User-Timing as an optional bounded visual mode.

## Shipped design

### Config shape (`web-shared`)

One nested struct — `profiling: None` is *structurally* zero-cost, which is the
whole point:

```rust
// web-shared/src/logging.rs
pub struct LoggingConfig {
    pub level: LevelFilter,          // ?log=  — tracing line filter
    pub console_writer: bool,        // fmt → browser DevTools console
    pub capture_buffer: bool,        // bounded ring buffer any embedder reads
                                     //   (editor → MCP; NOT MCP-specific)
    pub profiling: Option<ProfilingConfig>,  // None => zero per-frame cost
}
pub struct ProfilingConfig {
    pub cpu: TimingTier,             // ?trace=   — CPU spans → aggregator
    pub gpu: TimingTier,             // ?gputime= — GPU timestamps → aggregator
    pub devtools_measure: bool,      // ?devtools — ALSO mirror CPU spans to
                                     //   the User-Timing flame chart (bounded)
}
// renderer core: AwsmRendererLogging { cpu: TimingTier, gpu: TimingTier }
//   TimingTier = Off | Frame | SubFrame  (Off = default, span never created)
```

- `LoggingConfig::from_url()` — THE shared opt-in parser (`?log`/`?trace`/
  `?gputime`/`?devtools`). `profiling` stays `None` unless a profiling param is
  present. Editor calls it; players may.
- `init_logger(cfg: &LoggingConfig)` — installs layers strictly per `cfg`, reads
  **no** URL. Replaced the old unconditional `init_logger()`.
- `LoggingConfig::renderer_logging()` → `AwsmRendererLogging` (`Off/Off` when
  `profiling` is `None`) → `AwsmRendererBuilder::with_logging`.
- The rolling aggregators are *implicit*: they run whenever a tier != `Off`, so
  there's no separate flag.

### CPU timing — two independent opt-in outputs

1. **Aggregator layer** (`web-shared/src/aggregator.rs`): a `tracing` Layer that
   folds each span's `performance.now()` duration into a fixed-size per-name stat
   (`last`/`ema`/`min`/`max`/`count`). Keyed by the small fixed set of span
   names → never grows. Installed when `cpu != Off`. Read via
   `aggregator::timing_stats()`; the editor surfaces it under
   `memory_stats.cpu_span_timings`.
2. **DevTools timeline** (`performance.measure`): opt-in `devtools_measure`.
   Kept leak-safe by `logger::frame_boundary()` calling
   `performance.clearMarks()/clearMeasures()` once per frame (wired in the editor
   render loop). A DevTools *recording* still captures entries created during the
   recording, so the flame-chart workflow survives while the live buffer stays
   bounded.

### GPU timing (`?gputime`)

CPU spans measure our JS/wasm build+submit time, NOT the GPU. Real GPU pass
durations need timestamp queries.

- **Shipped:** `timestamp-query` device feature requested when the adapter
  exposes it (free when unused), `AwsmRendererWebGpu::has_timestamp_query()`
  accessor, and the `gpu: TimingTier` config plumbed end-to-end
  (`ProfilingConfig.gpu` → `AwsmRendererLogging.gpu`). The renderer-core type
  plumbing (`RenderTimestampWrites`, `ComputeTimestampWrites`,
  `create_query_set`) already exists.
- **Follow-up (tracked):** the actual capture/readback wiring — one timestamp
  `QuerySet` (2 slots per tracked pass), `timestampWrites` on each render/compute
  pass gated by `logging.gpu`, `resolveQuerySet` → buffer, ring-buffered
  `mapAsync` readback (results land a few frames later), ns→ms, fold into a
  renderer-side rolling aggregator (`gpu_frame_timings()`), surfaced under
  `memory_stats.gpu_span_timings`. Deliberately **not** landed in the same PR as
  the leak fix: it touches the render hot path across ~25 pass sites and must be
  browser-verified against real device numbers on its own, with an eye on the
  no-per-frame-allocation rule (fixed query set + fixed readback ring, zero
  churn). Zero-cost when `gpu == Off`: no query set, no timestamp writes, no
  resolve/readback.

## Frontend wiring (shipped)

- **Editor**: `init_logger(&LoggingConfig::from_url())` in `main.rs`;
  `with_logging(LoggingConfig::from_url().renderer_logging())` in `context.rs`;
  `logger::frame_boundary()` per frame in `render_loop.rs`; CPU aggregator
  surfaced in the `memory_stats` MCP query.
- **model-tests**: its duplicate `logger.rs` (which still had the *unconditional*
  perf_layer — a second copy of the leak) was deleted and unified onto the shared
  `init_logger` + `LoggingConfig::from_url()`. Default = zero cost.
- **player-tests**: already rolled its own perf-layer-free tracing and never set
  `with_logging` (so `Off/Off`) — already zero-cost, left as-is; may adopt
  `LoggingConfig::from_url()` if it ever wants the shared knobs.

## Leak-safety summary

- Default (`profiling: None`): no perf layer, no spans, no query sets → nothing
  accumulates. This is a normal editor session.
- CPU aggregator: fixed-size rolling stats, keyed by fixed span-name set.
- DevTools timeline: per-frame `clearMarks/clearMeasures` → bounded.
- GPU timestamps (when built): fixed query set + ring readback → bounded.

## Status

- **Phase 1** (config refactor, unify loggers, remove the leak): **DONE.**
- **Phase 2** (CPU aggregator + `memory_stats` surfacing): **DONE.**
- **Phase 3** (leak-safe DevTools timeline, `?devtools`, per-frame clear): **DONE.**
- **Phase 4** (GPU timestamps): enablement + config plumbing **DONE**; per-pass
  capture/readback aggregator is the tracked follow-up (own PR + browser verify).
