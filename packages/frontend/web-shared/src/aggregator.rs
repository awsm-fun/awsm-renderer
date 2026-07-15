//! Bounded rolling aggregator for renderer CPU-timing spans.
//!
//! [`AggregatorLayer`] is a `tracing` [`Layer`] that measures each span's wall
//! duration (via `performance.now()` on enter/exit) and folds it into a
//! fixed-size per-span-name statistic ([`TimingStat`]) — `last` / `ema` / `min`
//! / `max` / `count`. Unlike `tracing_web::performance_layer`, it accumulates
//! **nothing** over time: the stats table is keyed by the small, fixed set of
//! span names the renderer emits, so memory is flat no matter how long a session
//! runs. It's installed only when CPU profiling is on (`ProfilingConfig::cpu !=
//! Off`).
//!
//! Read the current snapshot with [`timing_stats`]; the editor renders it in a
//! debug overlay and exposes it over MCP, players read it their own way.

use std::collections::HashMap;
use std::sync::Mutex;

use tracing::span;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Rolling stats for one span name. All times in milliseconds.
#[derive(Clone, Copy, Debug)]
pub struct TimingStat {
    /// Most recent observed duration.
    pub last_ms: f64,
    /// Exponential moving average (alpha = [`EMA_ALPHA`]).
    pub ema_ms: f64,
    /// Smallest duration seen since init.
    pub min_ms: f64,
    /// Largest duration seen since init.
    pub max_ms: f64,
    /// Number of observations folded in.
    pub count: u64,
}

impl TimingStat {
    fn first(ms: f64) -> Self {
        Self {
            last_ms: ms,
            ema_ms: ms,
            min_ms: ms,
            max_ms: ms,
            count: 1,
        }
    }

    fn fold(&mut self, ms: f64) {
        self.last_ms = ms;
        self.ema_ms = EMA_ALPHA * ms + (1.0 - EMA_ALPHA) * self.ema_ms;
        self.min_ms = self.min_ms.min(ms);
        self.max_ms = self.max_ms.max(ms);
        self.count += 1;
    }
}

/// Smoothing factor for the EMA: higher = more responsive, lower = smoother.
const EMA_ALPHA: f64 = 0.1;

/// Global stats table. Keyed by the span's `&'static str` name (a small, fixed
/// set), so the map never grows unbounded. wasm is single-threaded → no
/// contention.
static TIMINGS: Mutex<Option<HashMap<&'static str, TimingStat>>> = Mutex::new(None);

/// Snapshot of the current per-span stats, sorted by descending EMA (slowest
/// first) so the hottest spans surface at the top of an overlay.
pub fn timing_stats() -> Vec<(&'static str, TimingStat)> {
    let guard = TIMINGS.lock().unwrap();
    let Some(map) = guard.as_ref() else {
        return Vec::new();
    };
    let mut out: Vec<_> = map.iter().map(|(k, v)| (*k, *v)).collect();
    out.sort_by(|a, b| b.1.ema_ms.total_cmp(&a.1.ema_ms));
    out
}

/// Clear the accumulated stats (e.g. when starting a fresh measurement window).
pub fn clear_timing_stats() {
    if let Ok(mut guard) = TIMINGS.lock() {
        if let Some(map) = guard.as_mut() {
            map.clear();
        }
    }
}

fn record(name: &'static str, ms: f64) {
    let mut guard = TIMINGS.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    map.entry(name)
        .and_modify(|s| s.fold(ms))
        .or_insert_with(|| TimingStat::first(ms));
}

/// `performance.now()` in ms, or 0.0 if unavailable.
fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}

/// Per-span start timestamp, stashed in the span's registry extensions between
/// enter and exit.
struct EnteredAt(f64);

/// A `tracing` layer that folds span durations into the bounded [`TIMINGS`]
/// table. Requires a subscriber that stores span data (the `registry()` does).
pub struct AggregatorLayer;

impl AggregatorLayer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AggregatorLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for AggregatorLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().replace(EnteredAt(now_ms()));
        }
    }

    fn on_exit(&self, id: &span::Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let started = span.extensions().get::<EnteredAt>().map(|e| e.0);
            if let Some(start) = started {
                let elapsed = now_ms() - start;
                if elapsed >= 0.0 {
                    record(span.name(), elapsed);
                }
            }
        }
    }
}
