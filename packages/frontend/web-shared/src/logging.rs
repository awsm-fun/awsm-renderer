//! Explicit logging + profiling configuration for awsm-renderer frontends.
//!
//! [`LoggingConfig`] is the single source of truth for what a frontend's
//! `tracing` subscriber does and how much per-frame profiling the renderer
//! produces. [`crate::logger::init_logger`] installs subscriber layers strictly
//! per the config — it performs **no** URL inspection of its own.
//!
//! [`LoggingConfig::from_url`] is an *opt-in* helper that maps the editor's
//! URL-query conventions (`?log`, `?trace`, `?gputime`, `?devtools`) onto a
//! config. The editor calls it; players may reuse it or build a [`LoggingConfig`]
//! by hand. Either way, the important property holds: **`profiling: None` is
//! structurally zero-cost** — no spans are created, no GPU queries issued, no
//! User-Timing entries accumulated. A normal long-lived editor session runs in
//! exactly that state, which is what closes the ~70GB User-Timing leak that used
//! to crash the tab (see `docs/plans/profiling.md`).

use awsm_renderer::debug::{AwsmRendererLogging, TimingTier};
use tracing_subscriber::filter::LevelFilter;

/// Subscriber-level configuration: where log lines and timings go. Passed by
/// value to [`crate::logger::init_logger`]. Construct it explicitly, or derive
/// it from the URL with [`LoggingConfig::from_url`].
#[derive(Clone, Debug)]
pub struct LoggingConfig {
    /// Level filter for `tracing::{error,warn,info,debug,trace}!` lines. Does
    /// **not** gate profiling spans — those are gated by `profiling`.
    pub level: LevelFilter,
    /// Install the fmt→browser-console writer (human-readable DevTools output).
    pub console_writer: bool,
    /// Install the bounded ring-buffer capture layer. It mirrors every event
    /// into a fixed-size in-memory buffer any embedder can read back — the
    /// editor exposes it over MCP, a player could surface it in an in-page
    /// overlay. Cheap and bounded regardless of how long the session runs, so
    /// there's no leak reason to turn it off; it's a knob purely so a player
    /// that doesn't need it can save the couple hundred KB.
    pub capture_buffer: bool,
    /// Per-frame profiling. `None` (the default) is structurally zero-cost: no
    /// spans, no GPU queries, no User-Timing entries. `Some(..)` opts into the
    /// tiers described on [`ProfilingConfig`].
    pub profiling: Option<ProfilingConfig>,
}

/// Per-frame profiling knobs. Only meaningful inside
/// [`LoggingConfig::profiling`]; the presence of the `Some` is itself the
/// master "profiling on" switch.
///
/// The rolling aggregators (last / ema / min / max per span or pass) are
/// *implicit* — they run whenever the corresponding tier is non-[`TimingTier::Off`],
/// so there's no separate flag for them. `devtools_measure` is the one extra,
/// heavier CPU-only output.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProfilingConfig {
    /// CPU-side `tracing`-span granularity. Non-`Off` folds each span's duration
    /// into the CPU aggregator (and, when `devtools_measure`, the User-Timing
    /// flame chart).
    pub cpu: TimingTier,
    /// GPU-side timestamp-query granularity. Non-`Off` requests the
    /// `timestamp-query` device feature, attaches per-pass `timestampWrites`,
    /// and folds resolved durations into the GPU aggregator.
    pub gpu: TimingTier,
    /// ALSO mirror CPU spans into the browser User-Timing timeline
    /// (`performance.measure`/`mark`) so they show up in the DevTools
    /// Performance flame chart. CPU-only. Off by default. This is the output
    /// that used to leak; it is kept bounded by clearing marks/measures once
    /// per frame (see [`crate::logger::frame_boundary`]).
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
    /// The renderer-core logging flags this profiling config implies.
    pub fn renderer_logging(&self) -> AwsmRendererLogging {
        AwsmRendererLogging {
            cpu: self.cpu,
            gpu: self.gpu,
        }
    }
}

impl LoggingConfig {
    /// The renderer-core logging flags for this config — `Off/Off` (zero
    /// per-frame cost) when `profiling` is `None`. Feed this into
    /// `AwsmRendererBuilder::with_logging`.
    pub fn renderer_logging(&self) -> AwsmRendererLogging {
        self.profiling
            .map(|p| p.renderer_logging())
            .unwrap_or_default()
    }

    /// True when the DevTools User-Timing mirror is requested — the only
    /// profiling output that needs the per-frame `performance.clear*` guard.
    pub fn devtools_measure(&self) -> bool {
        self.profiling.map(|p| p.devtools_measure).unwrap_or(false)
    }

    /// Build a config from the page URL query, applying the editor's
    /// conventions. This is opt-in: a frontend *chooses* to call it (the
    /// subscriber layer never reads the URL itself).
    ///
    /// - `?log=off|error|warn|info|debug|trace` → `level` (default `INFO`)
    /// - `?trace=off|frame|sub-frame`           → enables profiling, sets `cpu`
    /// - `?gputime=off|frame|sub-frame`         → enables profiling, sets `gpu`
    /// - `?devtools`                            → enables profiling, `devtools_measure`
    ///
    /// `profiling` stays `None` unless at least one of `?trace` / `?gputime` /
    /// `?devtools` is present, so a normal session is zero-cost.
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
