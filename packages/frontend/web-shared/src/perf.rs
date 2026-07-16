//! Low-level URL-query readers shared by awsm-renderer frontends.
//!
//! These are the raw building blocks (`?key`, `?key=value`, `?mobile=`,
//! `?log=`). The *composed* logging/profiling configuration lives in
//! [`crate::logging`] — see [`crate::logging::LoggingConfig::from_url`], which
//! layers the editor's `?trace` / `?gputime` / `?devtools` conventions on top
//! of these helpers. Reading a URL param is an explicit, opt-in act by a
//! frontend; nothing here runs implicitly.

use awsm_renderer::profile::RendererProfile;
use tracing_subscriber::filter::LevelFilter;

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

/// Resolve the effective log level: query param wins, else default.
pub fn resolve_log_level(default: LevelFilter) -> LevelFilter {
    log_level_override().unwrap_or(default)
}

/// Tiny `?key=value` lookup against `window.location.search`. Not a
/// full URL parser — just enough for the perf/logging knobs we care
/// about. Returns the raw value (we don't accept percent-encoded keys
/// in practice).
pub(crate) fn query_param(key: &str) -> Option<String> {
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

/// True when a bare `?key` (or `?key=…`) flag is present in the page URL.
pub(crate) fn query_flag(key: &str) -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let Ok(search) = window.location().search() else {
        return false;
    };
    let stripped = search.strip_prefix('?').unwrap_or(&search);
    stripped
        .split('&')
        .any(|p| p == key || p.starts_with(&format!("{key}=")))
}
