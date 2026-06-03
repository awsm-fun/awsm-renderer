use awsm_renderer::debug::RenderTimings;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::format::Pretty;
use tracing_subscriber::prelude::*;
use tracing_web::{performance_layer, MakeWebConsoleWriter};
use wasm_bindgen::prelude::*;

/// Default subscriber-level filter when no `?log=` URL override
/// is present. Mirrors the policy in `awsm-web-shared::logger`.
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

        let level_filter = log_level_override().unwrap_or(DEFAULT_LEVEL);

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

/// Default render-timings tier for model-tests. Debug builds get
/// the full sub-frame detail. Release builds get the single outer
/// `"Render"` span only (frame-time pulse with no per-pass noise).
/// `?trace=…` overrides either.
pub fn default_render_timings() -> RenderTimings {
    let build_default = if cfg!(debug_assertions) {
        RenderTimings::SubFrame
    } else {
        RenderTimings::Frame
    };
    render_timings_override().unwrap_or(build_default)
}

fn render_timings_override() -> Option<RenderTimings> {
    query_param("trace").and_then(|v| RenderTimings::parse(&v))
}

fn log_level_override() -> Option<LevelFilter> {
    let raw = query_param("log")?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => Some(LevelFilter::OFF),
        "error" => Some(LevelFilter::ERROR),
        "warn" => Some(LevelFilter::WARN),
        "info" => Some(LevelFilter::INFO),
        "debug" => Some(LevelFilter::DEBUG),
        "trace" => Some(LevelFilter::TRACE),
        _ => None,
    }
}

fn query_param(key: &str) -> Option<String> {
    let window = web_sys::window()?;
    let search = window.location().search().ok()?;
    let stripped = search.strip_prefix('?').unwrap_or(&search);
    for pair in stripped.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut it = pair.splitn(2, '=');
        let k = it.next()?;
        if k == key {
            return Some(it.next().unwrap_or("").to_string());
        }
    }
    None
}

#[wasm_bindgen(
    inline_js = "export function set_stack_trace_limit(limit) { Error.stackTraceLimit = limit; }"
)]
extern "C" {
    fn set_stack_trace_limit(limit: u32);
}
