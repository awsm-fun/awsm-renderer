//! Debug helpers and logging flags.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

/// Renderer logging flags.
#[derive(Clone, Debug, Default)]
pub struct AwsmRendererLogging {
    /// How much per-frame work should open a `tracing` span.
    ///
    /// Each span enter/exit on the web routes through
    /// `tracing_web::performance_layer`, which calls
    /// `performance.mark()` and `performance.measure()` across the
    /// wasm↔JS boundary. On mobile the per-call cost is large enough
    /// that letting every sub-pass open a span dominates frame time.
    /// We therefore
    /// gate at the call site so a span is never even *created*
    /// unless the tier permits it.
    pub render_timings: RenderTimings,
}

/// How much render-side tracing to emit per frame.
///
/// Ordering matters: each tier is a strict superset of the
/// previous one. Comparing with `>=` is the canonical way to test
/// at a span site.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum RenderTimings {
    /// No render-timing spans at all. The crate-level default —
    /// matches the prior `render_timings: bool = false` behavior so
    /// any embedder that constructs `AwsmRendererLogging::default()`
    /// pays zero tracing cost. Frontends explicitly opt in to a
    /// non-`Off` tier; see `crates/web-shared/src/perf.rs` for the
    /// `?trace=…` URL-param wiring.
    #[default]
    Off,
    /// Just the outermost `"Render"` span — one
    /// `performance.mark` plus one `performance.measure` per
    /// frame. This is what the shipping web build runs by default:
    /// it tells you frame time (and lets the DevTools performance
    /// panel show a single bar per frame) while costing essentially
    /// nothing.
    Frame,
    /// Every render pass, GPU write, hook, and renderer-internal
    /// stage opens its own span. This is what you want when
    /// diagnosing why a frame is slow; it's far too chatty to run
    /// in shipping builds on mobile.
    SubFrame,
}

impl RenderTimings {
    /// True when *any* render-timing span should be created.
    /// Equivalent to `!= Off`.
    pub fn enabled(self) -> bool {
        self != RenderTimings::Off
    }

    /// True when sub-frame spans (passes, GPU writes, hooks, …)
    /// should be created. Equivalent to `>= SubFrame`.
    pub fn sub_frame(self) -> bool {
        self >= RenderTimings::SubFrame
    }

    /// Parse the value of a `?trace=…` URL parameter.
    ///
    /// Accepts (case-insensitive): `off`, `none`, `frame`,
    /// `sub-frame`, `subframe`, `sub_frame`. Returns `None` for
    /// anything else so callers can fall back to a build-time
    /// default.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "0" => Some(Self::Off),
            "frame" | "1" => Some(Self::Frame),
            "sub-frame" | "subframe" | "sub_frame" | "2" => Some(Self::SubFrame),
            _ => None,
        }
    }
}

/// Debug ID reserved for renderable tracking.
pub const DEBUG_ID_RENDERABLE: u32 = u32::MAX - 1;

static DEBUG_TRANSACTION_ID: LazyLock<Mutex<HashMap<u32, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static DEBUG_UNIQUE_STRING: LazyLock<Mutex<HashMap<u32, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// returns the old value
fn bump_transaction_count(id: u32) -> u64 {
    let mut lock = DEBUG_TRANSACTION_ID.lock().unwrap();
    let value = lock.entry(id).or_insert(0);
    let curr = *value;
    *value += 1;

    curr
}

/// Runs a closure only once per debug ID.
pub fn debug_once(id: u32, f: impl FnOnce()) {
    let transaction_count = bump_transaction_count(id);

    if transaction_count == 0 {
        f();
    }
}

/// Runs a closure up to `n` times per debug ID.
pub fn debug_n(id: u32, n: u64, f: impl FnOnce()) {
    let transaction_count = bump_transaction_count(id);

    if transaction_count < n {
        f();
    }
}

/// Runs a closure if the input string changes for the debug ID.
pub fn debug_unique_string(id: u32, input: &str, f: impl FnOnce()) {
    bump_transaction_count(id);

    let mut lock = DEBUG_UNIQUE_STRING.lock().unwrap();
    if let Some(value) = lock.get(&id) {
        if value == input {
            return; // already set
        }
    }

    f();
    lock.insert(id, input.to_string());
}
