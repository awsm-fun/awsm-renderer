# Perf tracing — runtime knobs

How to measure renderer frame work without paying for the
measurement. Three tiers, two URL params, one rule of thumb.

This is the day-to-day guide; for what the spans *mean* and what
"normal" values look like, see [PERFORMANCE.md](PERFORMANCE.md).

---

## TL;DR

```
?trace=off          # zero render-timing spans
?trace=frame        # default in release — one outer "Render" span per frame
?trace=sub-frame    # default in debug — every pass / GPU write opens a span
?log=info|debug|…   # subscriber-level filter for tracing::{info,debug,…}! logs
```

Defaults:

| Build | `render_timings` | `LevelFilter` |
|---|---|---|
| `cfg!(debug_assertions)` | `SubFrame` | `INFO` |
| release | `Frame` | `INFO` |

Either default is overridden by the matching URL param.

---

## Why this exists

Every `tracing` span enter/exit is routed through
`tracing_web::performance_layer`, which calls `performance.mark()`
and `performance.measure()` across the wasm↔JS boundary. On
desktop each crossing is microseconds; on mobile it's enough that
a single character at default settings was burning ~28 % of CPU
inside `performance.mark` / `performance.measure` and dropping
frames against the 16.7 ms vsync budget. The fix isn't to delete
the spans — it's to choose which ones fire.

Each span site in the renderer is gated *in Rust* on the current
`RenderTimings` tier. A site that doesn't match the tier never
creates a `tracing::Span`, so the JS host call never happens.

---

## The three tiers

`awsm_renderer::debug::RenderTimings` (defined in
[crates/renderer/src/debug.rs](../crates/renderer/src/debug.rs)):

* **`Off`** — no render-timing spans at all. The crate-level
  `Default::default()`. Pick this when you have no use for
  per-frame instrumentation and want the absolute zero.
* **`Frame`** — exactly one span per frame: the outermost
  `"Render"` (see [crates/renderer/src/render.rs](../crates/renderer/src/render.rs)).
  Costs one `performance.mark` + one `performance.measure` per
  frame. The shipping web build's default. Use this to know
  whether you're holding 60 fps without polluting the trace.
* **`SubFrame`** — every render pass, GPU-write, hook, and
  renderer-internal stage opens its own span. Roughly 30–60 spans
  per frame depending on enabled features. Use this when you're
  trying to understand *why* a frame is slow. **Not** suitable
  for production on mobile.

The tiers are ordered: `Off < Frame < SubFrame`. Two helpers do
the gating at call sites:

```rust
logging.render_timings.enabled()    // == !Off    — only the outer "Render" span uses this
logging.render_timings.sub_frame()  // == >= SubFrame — every other span
```

---

## Picking a tier at runtime

Append `?trace=…` to the URL the frontend is served from:

| URL | Effective tier |
|---|---|
| `…/scene-editor/?trace=off` | `Off` |
| `…/scene-editor/?trace=frame` | `Frame` |
| `…/scene-editor/?trace=sub-frame` | `SubFrame` |
| `…/scene-editor/` (no param) | build default |

The URL param wins over the build default unconditionally. It's
read once at boot — the renderer captures the value when
`AwsmRendererBuilder` runs, so reloading the page is the cheapest
way to switch.

Multiple param spellings are accepted for `sub-frame`:
`sub-frame`, `subframe`, `sub_frame`, or `2`.

### `?log=…`

The subscriber-level filter is a separate axis from span tiers.
It controls whether `tracing::{error,warn,info,debug,trace}!` log
lines reach the browser console. It does **not** gate spans —
those are gated by `?trace=…`.

| URL | Effective filter |
|---|---|
| `…/?log=info` | `INFO` (default) |
| `…/?log=debug` | `DEBUG` |
| `…/?log=trace` | `TRACE` |
| `…/?log=warn` | `WARN` |
| `…/?log=error` | `ERROR` |
| `…/?log=off` | nothing |

The renderer emits some `tracing::debug!` lines on the hot path
(pipeline-readiness transitions, optimisation policy flips). At
the previous `DEBUG` default this was acceptable noise; at the
new `INFO` default they're silenced. Lift to `?log=debug` when
investigating.

---

## Picking a tier from code

Frontends call into [`awsm_web_shared::perf`](../crates/web-shared/src/perf.rs):

```rust
use awsm_renderer::debug::{AwsmRendererLogging, RenderTimings};
use awsm_web_shared::perf::resolve_render_timings;

let renderer = AwsmRendererBuilder::new(gpu_builder)
    .with_logging(AwsmRendererLogging {
        render_timings: resolve_render_timings(
            if cfg!(debug_assertions) {
                RenderTimings::SubFrame
            } else {
                RenderTimings::Frame
            },
        ),
    })
    .build()
    .await?;
```

`resolve_render_timings(default)` returns the `?trace=…` override
if present, else `default`. `model-tests` uses a local copy of
the same logic (it doesn't depend on `web-shared`) — see
[crates/frontend/model-tests/src/logger.rs](../crates/frontend/model-tests/src/logger.rs).

---

## What you get back

When any non-`Off` tier is active, the spans surface as
`PerformanceEntry`s on the page:

```js
performance.getEntriesByType('measure')
  .filter(e => e.detail?.devtools?.color === 'primary-light')
  // each entry has .name, .startTime, .duration
```

`Frame` tier yields exactly one `"Render"` entry per rendered
frame. `SubFrame` yields one per pass — names listed in §4 of
[PERFORMANCE.md](PERFORMANCE.md).

The DevTools **Performance** panel also shows these in the
"Timings" track without any extra setup. That's the recommended
way to view them — open DevTools, hit Record, reload with
`?trace=sub-frame`, stop after a few seconds.

For programmatic / regression-driven measurement, the
`scene-editor` debug-build harness in
[crates/frontend/scene-editor/src/actions/measurement.rs](../crates/frontend/scene-editor/src/actions/measurement.rs)
reads `getEntriesByType('measure')` directly. It already loads
with debug-build defaults (= `SubFrame`); no URL params needed.

---

## Rule of thumb

* You're shipping → leave the defaults alone. Release builds get
  `Frame` + `INFO`. Mobile devices will hit framerate.
* You're chasing a slowdown on your dev machine → debug build
  picks up `SubFrame` automatically.
* You need to compare a *release* build to itself → reload with
  `?trace=sub-frame` once, capture, reload without.
* You're scripting a perf regression test → use the debug build,
  read `getEntriesByType('measure')`, see `measurement.rs`.
* You added a new render pass and want it timed → drop in
  ```rust
  let _span = self
      .logging
      .render_timings
      .sub_frame()
      .then(|| tracing::span!(tracing::Level::INFO, "My new pass").entered());
  ```
  (or copy any existing site's `let _maybe_span_guard = if … { … } else { None };`
  pattern — that's what the rest of the renderer uses).

---

## Why not a Cargo feature?

Tiers are runtime because the same wasm binary serves all of dev,
staging, and production on every device profile. Rebuilding to
flip a knob and re-deploying CDN bundles would mean the only
realistic way to investigate a mobile regression in the wild is
to reproduce it locally. With URL params, a user with a problem
can hand you `…/?trace=sub-frame` data from their device.

A Cargo feature *would* let us strip the `tracing-web::performance_layer`
registration entirely, but the current setup already costs zero
JS calls when no span site fires (Rust short-circuits before
`Span::new`), so there's nothing left to strip.
