use std::collections::VecDeque;
use std::sync::Mutex;

use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::format::Pretty;
use tracing_subscriber::prelude::*;
use tracing_web::{performance_layer, MakeWebConsoleWriter};
use wasm_bindgen::prelude::*;

use crate::perf::resolve_log_level;

/// In-memory ring buffer of recent `tracing` events (level + formatted line:
/// `<file:line> — <message + fields>`). The browser console is invisible over
/// MCP, so a custom [`CaptureLayer`] mirrors every event here and the editor's
/// `ConsoleLogs` MCP query reads it — letting a headless driver see the same
/// `WARN`/`ERROR`/`tracing::*` output a human sees in devtools. Capped; oldest
/// dropped. (wasm is single-threaded, so the `Mutex` never contends.)
static CAPTURED_LOGS: Mutex<VecDeque<(String, String)>> = Mutex::new(VecDeque::new());
const CAPTURED_LOGS_CAP: usize = 1000;

/// The last `limit` captured `tracing` events as `(level, line)`, oldest first.
/// Read (not drained) so repeated MCP polls each see the full recent window.
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
/// console fmt layer) so it's readable over MCP.
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

/// Default subscriber-level filter when no `?log=` URL override
/// is present.
///
/// Used to be `DEBUG`, which combined with render-timing spans
/// emitted ~1.6k UserTiming entries in 3s on mobile and burned a
/// significant chunk of frame time on `performance.mark`. Default
/// is now level-appropriate; lift back to `DEBUG` (or `TRACE`)
/// with `?log=debug` when investigating.
const DEFAULT_LEVEL: LevelFilter = LevelFilter::INFO;

pub fn init_logger() {
    static LOGGER_INITIALIZED: std::sync::Once = std::sync::Once::new();

    LOGGER_INITIALIZED.call_once(|| {
        set_stack_trace_limit(30);

        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_file(true)
            .with_line_number(true)
            .with_ansi(false) // Only partially supported across JavaScript runtimes
            .without_time()
            .with_level(true)
            .with_target(false)
            .with_writer(MakeWebConsoleWriter::new().with_pretty_level()); // write events to the console

        let perf_layer = performance_layer().with_details_from_fields(Pretty::default());

        let level_filter = resolve_log_level(DEFAULT_LEVEL);

        tracing_subscriber::registry()
            .with(fmt_layer)
            .with(perf_layer)
            // Mirror events into the MCP-readable ring buffer (see `captured_logs`).
            .with(CaptureLayer)
            .with(level_filter)
            .init();

        tracing::info!("(info) Logger initialized at {:?}", level_filter);
        tracing::debug!("(debug) Logger initialized");

        std::panic::set_hook(Box::new(tracing_panic::panic_hook));
    });
}

#[wasm_bindgen(
    inline_js = "export function set_stack_trace_limit(limit) { Error.stackTraceLimit = limit; }"
)]
extern "C" {
    fn set_stack_trace_limit(limit: u32);
}
