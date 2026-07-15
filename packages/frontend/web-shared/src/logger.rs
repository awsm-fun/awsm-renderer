//! `tracing` subscriber installation, driven entirely by a [`LoggingConfig`].
//!
//! [`init_logger`] reads nothing from the environment — it installs exactly the
//! layers the passed config asks for. Frontends decide their config however they
//! like (the editor via [`LoggingConfig::from_url`], players by hand). Every
//! optional layer is an `Option<Layer>` (a `None` is a no-op `Layer`), so a
//! disabled output truly does nothing.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tracing_subscriber::fmt::format::Pretty;
use tracing_subscriber::prelude::*;
use tracing_web::{performance_layer, MakeWebConsoleWriter};
use wasm_bindgen::prelude::*;

use crate::logging::LoggingConfig;

/// In-memory ring buffer of recent `tracing` events (level + formatted line:
/// `<file:line> — <message + fields>`). The browser console is invisible to a
/// headless/embedding driver, so a custom [`CaptureLayer`] mirrors every event
/// here and any embedder can read it back — the editor exposes it over MCP;
/// nothing here is MCP-specific. Capped; oldest dropped. (wasm is
/// single-threaded, so the `Mutex` never contends.)
static CAPTURED_LOGS: Mutex<VecDeque<(String, String)>> = Mutex::new(VecDeque::new());
const CAPTURED_LOGS_CAP: usize = 1000;

/// Set at [`init_logger`] time when the DevTools User-Timing mirror is on, so
/// [`frame_boundary`] knows whether it needs to clear the (otherwise unbounded)
/// User-Timing buffer each frame.
static DEVTOOLS_MEASURE: AtomicBool = AtomicBool::new(false);

/// The last `limit` captured `tracing` events as `(level, line)`, oldest first.
/// Read (not drained) so repeated polls each see the full recent window.
pub fn captured_logs(limit: usize) -> Vec<(String, String)> {
    let buf = CAPTURED_LOGS.lock().unwrap();
    let start = buf.len().saturating_sub(limit);
    buf.iter().skip(start).cloned().collect()
}

/// Visitor that flattens an event's fields into one string — the `message`
/// field first (the `tracing::info!("...")` body), then any structured fields
/// as `key=value`.
#[derive(Default)]
struct FieldCollector {
    message: String,
    fields: String,
}
impl tracing::field::Visit for FieldCollector {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            if !self.fields.is_empty() {
                self.fields.push(' ');
            }
            let _ = write!(self.fields, "{}={value:?}", field.name());
        }
    }
}

/// Mirrors every `tracing` event into [`CAPTURED_LOGS`] (in addition to the
/// console fmt layer) so it's readable by an embedder.
struct CaptureLayer;
impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let meta = event.metadata();
        let mut c = FieldCollector::default();
        event.record(&mut c);
        let loc = match (meta.file(), meta.line()) {
            (Some(f), Some(l)) => format!("{f}:{l}"),
            _ => meta.target().to_string(),
        };
        let mut line = format!("{loc} — {}", c.message);
        if !c.fields.is_empty() {
            line.push_str(" {");
            line.push_str(&c.fields);
            line.push('}');
        }
        if let Ok(mut buf) = CAPTURED_LOGS.lock() {
            if buf.len() >= CAPTURED_LOGS_CAP {
                buf.pop_front();
            }
            buf.push_back((meta.level().to_string(), line));
        }
    }
}

/// Install the global `tracing` subscriber per `cfg`. Idempotent (the first call
/// wins; later calls are ignored). Reads nothing from the URL/environment — the
/// caller decides `cfg`.
///
/// Layers installed, each gated by `cfg`:
/// - fmt→console writer (`cfg.console_writer`)
/// - [`CaptureLayer`] ring buffer (`cfg.capture_buffer`)
/// - CPU rolling aggregator (`cfg.profiling.cpu != Off`) — bounded, safe
/// - DevTools User-Timing mirror (`cfg.profiling.devtools_measure`) — kept
///   bounded by [`frame_boundary`]
/// - level filter (`cfg.level`)
pub fn init_logger(cfg: &LoggingConfig) {
    static LOGGER_INITIALIZED: std::sync::Once = std::sync::Once::new();

    LOGGER_INITIALIZED.call_once(|| {
        set_stack_trace_limit(30);

        DEVTOOLS_MEASURE.store(cfg.devtools_measure(), Ordering::Relaxed);

        let fmt_layer = cfg.console_writer.then(|| {
            tracing_subscriber::fmt::layer()
                .with_file(true)
                .with_line_number(true)
                .with_ansi(false) // Only partially supported across JavaScript runtimes
                .without_time()
                .with_level(true)
                .with_target(false)
                .with_writer(MakeWebConsoleWriter::new().with_pretty_level())
        });

        let capture_layer = cfg.capture_buffer.then_some(CaptureLayer);

        // Bounded rolling aggregator — folds each render-timing span's duration
        // into fixed-size per-name stats. Installed only when CPU profiling is
        // on; never grows (keyed by the small, fixed set of span names).
        let cpu_on = cfg.profiling.map(|p| p.cpu.enabled()).unwrap_or(false);
        let aggregator_layer = cpu_on.then(crate::aggregator::AggregatorLayer::new);

        // DevTools User-Timing mirror: spans → `performance.measure`/`mark` for
        // the Performance flame chart. This is the output that used to leak
        // unbounded (the browser never auto-clears the User-Timing buffer and
        // the renderer emits per-frame spans). It is now (a) opt-in via
        // `?devtools` and (b) kept bounded by `frame_boundary()` clearing the
        // buffer each frame. `Option<Layer>` is itself a `Layer` (None = no-op).
        let perf_layer = cfg
            .devtools_measure()
            .then(|| performance_layer().with_details_from_fields(Pretty::default()));

        let level_filter = cfg.level;

        tracing_subscriber::registry()
            .with(fmt_layer)
            .with(aggregator_layer)
            .with(perf_layer)
            .with(capture_layer)
            .with(level_filter)
            .init();

        tracing::info!("(info) Logger initialized at {:?}", level_filter);
        tracing::debug!("(debug) Logger initialized");

        std::panic::set_hook(Box::new(tracing_panic::panic_hook));
    });
}

/// Call once per rendered frame (e.g. at the top of the rAF callback). When the
/// DevTools User-Timing mirror is active, this clears the `performance` mark /
/// measure buffer so it can't grow unbounded — a live DevTools *recording* still
/// captures the entries created during the frame, so the flame-chart workflow is
/// preserved while memory stays flat. A no-op (one relaxed atomic load) when the
/// mirror is off, so it's safe to call unconditionally every frame.
pub fn frame_boundary() {
    if !DEVTOOLS_MEASURE.load(Ordering::Relaxed) {
        return;
    }
    if let Some(perf) = web_sys::window().and_then(|w| w.performance()) {
        perf.clear_marks();
        perf.clear_measures();
    }
}

#[wasm_bindgen(
    inline_js = "export function set_stack_trace_limit(limit) { Error.stackTraceLimit = limit; }"
)]
extern "C" {
    fn set_stack_trace_limit(limit: u32);
}
