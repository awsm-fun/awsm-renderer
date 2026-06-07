//! Runtime overrides for renderer perf-tracing.
//!
//! Two URL query parameters control how much we trace at runtime.
//! Both apply to any awsm-renderer frontend that calls
//! [`crate::logger::init_logger`] (or its app-local variants) at
//! boot:
//!
//! * `?trace=off|frame|sub-frame` — which render-side spans get
//!   created. Maps directly onto
//!   [`awsm_renderer::debug::RenderTimings`]. Span creation is
//!   gated *in Rust*, so a tier that doesn't include a span pays
//!   zero wasm↔JS crossings for it.
//! * `?log=error|warn|info|debug|trace` — the tracing-subscriber
//!   level filter. Affects `tracing::{error,warn,info,debug,trace}!`
//!   log lines (which go to the browser console). Spans are gated
//!   by `?trace=…`, not by this.
//!
//! Each frontend decides its own *default* (typically: SubFrame +
//! DEBUG in debug builds, Frame + INFO in release). The query
//! params, when present, override.

use awsm_renderer::{debug::RenderTimings, profile::RendererProfile};
use tracing_subscriber::filter::LevelFilter;

/// Read `?trace=…` from `window.location.search`, if any. Returns
/// `None` when the param is absent or unrecognised — caller falls
/// back to its build-time default.
pub fn render_timings_override() -> Option<RenderTimings> {
    query_param("trace").and_then(|v| RenderTimings::parse(&v))
}

/// Read `?mobile=…` from `window.location.search` and resolve to a
/// [`RendererProfile`]. Accepts:
///
/// - `true` / `1` / `yes` / `mobile` → `RendererProfile::Mobile`
/// - `false` / `0` / `no` / `desktop` → `RendererProfile::Desktop`
/// - `cinema` / `max` / `ultra` → `RendererProfile::Cinema`
///
/// Returns `None` when the param is absent or unrecognised — caller
/// falls back to its build-time default.
pub fn renderer_profile_override() -> Option<RendererProfile> {
    let raw = query_param("mobile")?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "mobile" => Some(RendererProfile::Mobile),
        "false" | "0" | "no" | "desktop" => Some(RendererProfile::Desktop),
        "cinema" | "max" | "ultra" => Some(RendererProfile::Cinema),
        _ => None,
    }
}

/// Resolve the effective renderer profile: query param wins, else the
/// supplied build-time default. Frontends call this when constructing
/// the renderer so a `?mobile=true` link forces the mobile-friendly
/// defaults even on a desktop browser (and vice versa).
pub fn resolve_renderer_profile(default: RendererProfile) -> RendererProfile {
    renderer_profile_override().unwrap_or(default)
}

/// Read `?log=…` from `window.location.search`, if any.
pub fn log_level_override() -> Option<LevelFilter> {
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

/// Resolve the effective render-timings tier: query param wins,
/// else the supplied build-time default.
pub fn resolve_render_timings(default: RenderTimings) -> RenderTimings {
    render_timings_override().unwrap_or(default)
}

/// Resolve the effective log level: query param wins, else default.
pub fn resolve_log_level(default: LevelFilter) -> LevelFilter {
    log_level_override().unwrap_or(default)
}

/// Tiny `?key=value` lookup against `window.location.search`. Not a
/// full URL parser — just enough for the two perf knobs we care
/// about. Returns the raw value (URL-decoded for `+` → ` `; we
/// don't accept percent-encoded keys in practice).
fn query_param(key: &str) -> Option<String> {
    let window = web_sys::window()?;
    let search = window.location().search().ok()?;
    // `search` is either "" or "?a=b&c=d".
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
