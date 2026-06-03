use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::format::Pretty;
use tracing_subscriber::prelude::*;
use tracing_web::{performance_layer, MakeWebConsoleWriter};
use wasm_bindgen::prelude::*;

use crate::perf::resolve_log_level;

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
