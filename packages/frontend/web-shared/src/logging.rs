//! Explicit logging + profiling configuration for awsm-renderer frontends.
//!
//! [`LoggingConfig`] is the single source of truth for what a frontend's
//! `tracing` subscriber does and what per-frame profiling the renderer starts
//! with. [`crate::logger::init_logger`] consumes only the *log* fields; the
//! *profiling* sub-config seeds the renderer's runtime tiers and the DevTools
//! mirror.
//!
//! Profiling itself is **not** implemented here — it lives in the renderer
//! ([`awsm_renderer::profiling`]), gated at the source so that a disabled tier
//! is a complete no-op (no timer, no query set, nothing installed). Because the
//! gate is a plain runtime field, the same knobs can be flipped live from an
//! editor menu; this config just provides the *initial* values.
//!
//! [`LoggingConfig::from_url`] maps the editor's URL conventions (`?log`,
//! `?trace`, `?gputime`, `?devtools`) onto a config. The editor calls it;
//! players may reuse it or build a [`LoggingConfig`] by hand. The perf HUD is a
//! **separate** concern (`?perfhud`, [`crate::perf_hud`]) — deliberately not
//! tied to whether profiling is capturing.

use awsm_renderer::debug::{AwsmRendererLogging, TimingTier};
use tracing_subscriber::filter::LevelFilter;

/// Subscriber-level + initial-profiling configuration. Construct explicitly, or
/// derive it from the URL with [`LoggingConfig::from_url`].
#[derive(Clone, Debug)]
pub struct LoggingConfig {
    /// Level filter for `tracing::{error,warn,info,debug,trace}!` lines. Does
    /// **not** gate profiling.
    pub level: LevelFilter,
    /// Install the fmt→browser-console writer (human-readable DevTools output).
    pub console_writer: bool,
    /// Install the bounded ring-buffer capture layer any embedder can read back
    /// (the editor exposes it over MCP). Cheap and bounded regardless.
    pub capture_buffer: bool,
    /// Initial per-frame profiling state. `None` (the default) means the
    /// renderer starts with both tiers `Off` and the DevTools mirror disabled —
    /// a complete no-op. A menu can still turn profiling on later.
    pub profiling: Option<ProfilingConfig>,
}

/// Initial per-frame profiling state. Applied to the renderer via
/// [`ProfilingConfig::renderer_logging`] (tiers) and [`ProfilingConfig::apply`]
/// (DevTools mirror). All of it is runtime-mutable afterwards.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProfilingConfig {
    /// CPU-scope timing tier → the CPU aggregator (per-pass wall time).
    pub cpu: TimingTier,
    /// GPU timestamp-query tier → the GPU aggregator (per-pass device time).
    pub gpu: TimingTier,
    /// Also mirror CPU scopes into the browser User-Timing timeline for the
    /// DevTools flame chart. Kept bounded by `logger::frame_boundary`.
    pub devtools_measure: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: LevelFilter::INFO,
            console_writer: true,
            capture_buffer: true,
            profiling: None,
        }
    }
}

impl ProfilingConfig {
    /// The renderer-core logging flags (initial tiers) this config implies.
    pub fn renderer_logging(&self) -> AwsmRendererLogging {
        AwsmRendererLogging {
            cpu: self.cpu,
            gpu: self.gpu,
        }
    }

    /// Apply the runtime-only bits (currently the DevTools mirror) to the
    /// renderer's global profiling state.
    pub fn apply(&self) {
        awsm_renderer::profiling::set_devtools_measure(self.devtools_measure);
    }
}

impl LoggingConfig {
    /// Initial renderer tiers — `Off/Off` (zero per-frame cost) when `profiling`
    /// is `None`. Feed into `AwsmRendererBuilder::with_logging`.
    pub fn renderer_logging(&self) -> AwsmRendererLogging {
        self.profiling
            .map(|p| p.renderer_logging())
            .unwrap_or_default()
    }

    /// Apply the runtime-only profiling bits (DevTools mirror). Call once at
    /// boot after constructing the renderer. No-op when `profiling` is `None`.
    pub fn apply_profiling(&self) {
        if let Some(p) = self.profiling {
            p.apply();
        }
    }

    /// Build a config from the page URL query (opt-in — a frontend chooses to
    /// call it; nothing reads the URL implicitly).
    ///
    /// - `?log=off|error|warn|info|debug|trace` → `level` (default `INFO`)
    /// - `?trace=off|frame|sub-frame`           → enables profiling, sets `cpu`
    /// - `?gputime=off|frame|sub-frame`         → enables profiling, sets `gpu`
    /// - `?devtools`                            → enables profiling, `devtools_measure`
    ///
    /// `profiling` stays `None` unless at least one of `?trace` / `?gputime` /
    /// `?devtools` is present. (The perf HUD's `?perfhud` is intentionally
    /// independent — see [`crate::perf_hud`].)
    pub fn from_url() -> Self {
        let level = crate::perf::log_level_override().unwrap_or(LevelFilter::INFO);

        let cpu = crate::perf::query_param("trace").and_then(|v| TimingTier::parse(&v));
        let gpu = crate::perf::query_param("gputime").and_then(|v| TimingTier::parse(&v));
        let devtools = crate::perf::query_flag("devtools");

        let profiling = (cpu.is_some() || gpu.is_some() || devtools).then(|| ProfilingConfig {
            cpu: cpu.unwrap_or_default(),
            gpu: gpu.unwrap_or_default(),
            devtools_measure: devtools,
        });

        Self {
            level,
            console_writer: true,
            capture_buffer: true,
            profiling,
        }
    }
}
