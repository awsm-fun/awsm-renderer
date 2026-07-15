//! Debug helpers and logging flags.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

/// Renderer profiling/logging flags — what per-frame timing work the renderer
/// produces. Both tiers default to [`TimingTier::Off`], so an embedder that
/// constructs `AwsmRendererLogging::default()` pays ZERO per-frame cost (no
/// spans created, no GPU timestamp queries).
#[derive(Clone, Debug, Default)]
pub struct AwsmRendererLogging {
    /// CPU-side `tracing` span granularity. Each span enter/exit can route
    /// through `tracing_web::performance_layer` (User Timing) and/or a bounded
    /// aggregator — but the span is never even *created* unless the tier permits
    /// it (gated in Rust at the call site), so `Off` is truly free.
    pub cpu: TimingTier,
    /// GPU-side timestamp-query granularity. `Off` = no query set, no per-pass
    /// `timestampWrites`, no resolve/readback. `Frame` = one begin/end around
    /// the whole frame's GPU work; `SubFrame` = per-pass timestamps.
    pub gpu: TimingTier,
}

/// How much timing to emit per frame — shared by the CPU (`tracing` spans) and
/// GPU (timestamp query) paths.
///
/// Ordering matters: each tier is a strict superset of the
/// previous one. Comparing with `>=` is the canonical way to test
/// at a span site.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum TimingTier {
    /// No timing at all. The crate-level default — any embedder that constructs
    /// `AwsmRendererLogging::default()` pays zero cost. Frontends explicitly opt
    /// in to a non-`Off` tier; see `web-shared/src/logging.rs` for the URL-param
    /// wiring (`?trace=` for CPU, `?gputime=` for GPU).
    #[default]
    Off,
    /// Just the outermost `"Render"` span / one begin+end GPU timestamp per
    /// frame — tells you total frame (or whole-frame GPU) time for essentially
    /// nothing.
    Frame,
    /// Every render pass, GPU write, hook, and renderer-internal
    /// stage opens its own span / gets its own GPU timestamp pair. This is what
    /// you want when diagnosing why a frame is slow; too chatty for shipping.
    SubFrame,
}

impl TimingTier {
    /// True when *any* render-timing span should be created.
    /// Equivalent to `!= Off`.
    pub fn enabled(self) -> bool {
        self != TimingTier::Off
    }

    /// True when sub-frame spans (passes, GPU writes, hooks, …)
    /// should be created. Equivalent to `>= SubFrame`.
    pub fn sub_frame(self) -> bool {
        self >= TimingTier::SubFrame
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
