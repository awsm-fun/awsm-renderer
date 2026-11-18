//! Renderer-owned, opt-in profiling: CPU scope timing and GPU timestamp queries,
//! each folded into a bounded rolling aggregator.
//!
//! **Complete no-op when disabled.** There is no always-installed tracing layer
//! and no background bookkeeping. Timing is gated at the *source*: a
//! [`CpuScope`] is only constructed when [`AwsmRendererLogging::cpu`] permits,
//! and GPU timestamp writes are only emitted when [`AwsmRendererLogging::gpu`]
//! permits. When both tiers are [`TimingTier::Off`] nothing here runs — no
//! `performance.now()`, no query set, no timestamp writes, no resolve or
//! readback, no stats fold, no allocation. (The query set is created at build
//! if the GPU tier is already on, or else by the first frame that has it on.)
//! Because the gate is a plain
//! field on the renderer, it can be flipped **at runtime** (an editor menu, a
//! player hotkey) with the same zero-cost-when-off guarantee.
//!
//! The same holds for the renderer's other measurements, which have no
//! `AwsmRendererLogging` at hand and ask [`cpu_timing_enabled`] instead (the
//! upload rings' `mapAsync` wait, the worker pool's round trip), and for the
//! load-time `awsm_renderer::boot_timing` log lines, which read the clock only
//! while something listens to that target.
//!
//! # CPU scopes
//!
//! The CPU scopes of a frame, by [`cpu_scope_kind`]:
//!
//! * [`CpuScopeKind::Frame`]: `"Render"`, the whole of `render()`.
//! * [`CpuScopeKind::Prep`] (names start with [`CPU_PREP_PREFIX`]): what
//!   `render()` does before the first pass: pipeline polling, readback ingest,
//!   the per-subsystem buffer uploads, pass resources, bind groups, collecting
//!   the renderables.
//! * [`CpuScopeKind::Pass`]: encoding one render or compute pass.
//! * [`CpuScopeKind::Submit`] (names start with [`CPU_SUBMIT_PREFIX`]):
//!   finishing the encoder and `queue.submit`, then kicking the readbacks.
//! * [`CpuScopeKind::Hook`] (names end with `" Hook"`): a caller's render hook.
//!
//! Every kind but `Frame` needs [`TimingTier::SubFrame`]. They do not overlap
//! one another, and all lie inside `"Render"`, so that scope's time less their
//! sum is what `render()` spends in the glue between them.
//!
//! Two rolling aggregators (CPU + GPU) hold `last/ema/min/max/count` per named
//! scope, keyed by the small fixed set of scope names → they never grow. Read
//! them with [`cpu_timing_stats`] / [`gpu_timing_stats`]; the editor surfaces
//! them in `memory_stats` and the shared `web-shared` perf HUD renders them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use awsm_renderer_core::command::compute_pass::ComputeTimestampWrites;
use awsm_renderer_core::command::render_pass::RenderTimestampWrites;
use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use std::cell::RefCell;
use wasm_bindgen::prelude::*;

use crate::AwsmRendererLogging;

// ---------------------------------------------------------------------------
// Rolling per-scope stats
// ---------------------------------------------------------------------------

/// Rolling stats for one scope name. All times in milliseconds.
#[derive(Clone, Copy, Debug)]
pub struct TimingStat {
    pub last_ms: f64,
    pub ema_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub count: u64,
    /// Sum of every sample folded so far. With [`Self::count`] this gives an
    /// exact mean over any window: sample both at the window's start and end
    /// and divide the deltas (no EMA lag).
    pub total_ms: f64,
}

impl TimingStat {
    fn first(ms: f64) -> Self {
        Self {
            last_ms: ms,
            ema_ms: ms,
            min_ms: ms,
            max_ms: ms,
            count: 1,
            total_ms: ms,
        }
    }
    fn fold(&mut self, ms: f64) {
        self.last_ms = ms;
        self.ema_ms = EMA_ALPHA * ms + (1.0 - EMA_ALPHA) * self.ema_ms;
        self.min_ms = self.min_ms.min(ms);
        self.max_ms = self.max_ms.max(ms);
        self.count += 1;
        self.total_ms += ms;
    }
}

const EMA_ALPHA: f64 = 0.1;

/// Whether a CPU timing tier is on, as the renderer last said: for the
/// measurements that have no [`AwsmRendererLogging`] at hand.
static CPU_TIMING: AtomicBool = AtomicBool::new(false);

/// Called by the renderer when it is built and at the start of every frame.
pub(crate) fn set_cpu_timing(logging: &AwsmRendererLogging) {
    CPU_TIMING.store(logging.cpu.enabled(), Ordering::Relaxed);
}

/// Whether the renderer's [`AwsmRendererLogging::cpu`] tier was on at its last
/// frame. `false` until a renderer says otherwise, so nothing that asks this
/// measures by default.
pub fn cpu_timing_enabled() -> bool {
    CPU_TIMING.load(Ordering::Relaxed)
}

/// Wall-clock milliseconds for the load-time `awsm_renderer::boot_timing` log
/// lines, or `0.0`, without reading the clock, while nothing listens to that
/// target at `INFO` (the lines are not printed then either).
pub(crate) fn boot_clock_ms() -> f64 {
    if tracing::enabled!(target: "awsm_renderer::boot_timing", tracing::Level::INFO) {
        web_sys::js_sys::Date::now()
    } else {
        0.0
    }
}

/// Global CPU + GPU stats tables, keyed by `&'static str` scope name (a small,
/// fixed set → the maps never grow). wasm is single-threaded → no contention.
static CPU_TIMINGS: Mutex<Option<HashMap<&'static str, TimingStat>>> = Mutex::new(None);
static GPU_TIMINGS: Mutex<Option<HashMap<&'static str, TimingStat>>> = Mutex::new(None);

fn record(table: &Mutex<Option<HashMap<&'static str, TimingStat>>>, name: &'static str, ms: f64) {
    if !ms.is_finite() || ms < 0.0 {
        return;
    }
    if let Ok(mut guard) = table.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        map.entry(name)
            .and_modify(|s| s.fold(ms))
            .or_insert_with(|| TimingStat::first(ms));
    }
}

fn snapshot(
    table: &Mutex<Option<HashMap<&'static str, TimingStat>>>,
) -> Vec<(&'static str, TimingStat)> {
    let guard = table.lock().unwrap();
    let Some(map) = guard.as_ref() else {
        return Vec::new();
    };
    let mut out: Vec<_> = map.iter().map(|(k, v)| (*k, *v)).collect();
    out.sort_by(|a, b| b.1.ema_ms.total_cmp(&a.1.ema_ms));
    out
}

/// CPU scope stats, slowest (highest EMA) first. Empty when CPU profiling has
/// never run.
pub fn cpu_timing_stats() -> Vec<(&'static str, TimingStat)> {
    snapshot(&CPU_TIMINGS)
}

/// GPU pass stats, slowest first. Empty when GPU profiling has never run.
pub fn gpu_timing_stats() -> Vec<(&'static str, TimingStat)> {
    snapshot(&GPU_TIMINGS)
}

/// Name of the whole-frame GPU entry in [`gpu_timing_stats`]: per frame, the
/// latest end minus the earliest begin over every timed pass (see
/// [`GpuTimestamps::kick_readback`]).
pub const GPU_FRAME_SCOPE: &str = "Frame";

/// Whole-frame GPU time — the [`GPU_FRAME_SCOPE`] entry — or `None` until GPU
/// timing has produced a sample (tier off, or no `timestamp-query`).
///
/// This is the number to show next to a CPU frame time: it measures how long
/// the GPU spends on a frame, which is what decides whether a frame makes
/// vsync once the CPU side is cheap. It is an upper bound — it spans the gaps
/// between passes too, including any GPU work another process interleaves.
/// The span starts at the first TIMED pass (Geometry in a normal frame); the
/// virtual-geometry cluster-cut dispatch, when enabled, runs before it untimed.
pub fn gpu_frame_timing() -> Option<TimingStat> {
    let guard = GPU_TIMINGS.lock().ok()?;
    guard.as_ref()?.get(GPU_FRAME_SCOPE).copied()
}

/// Drop all accumulated stats (e.g. when starting a fresh measurement window).
pub fn clear_timing_stats() {
    for t in [&CPU_TIMINGS, &GPU_TIMINGS] {
        if let Ok(mut g) = t.lock() {
            if let Some(m) = g.as_mut() {
                m.clear();
            }
        }
    }
}

/// Record a CPU scope duration directly (bypassing [`CpuScope`]). Rarely needed.
pub fn record_cpu(name: &'static str, ms: f64) {
    record(&CPU_TIMINGS, name, ms);
}

// ---------------------------------------------------------------------------
// DevTools User-Timing mirror (opt-in, runtime-toggleable)
// ---------------------------------------------------------------------------

/// When set, every [`CpuScope`] also emits a `performance.measure` so the scope
/// shows up in the DevTools Performance flame chart. Runtime-toggleable; kept
/// leak-safe by `web-shared`'s per-frame `clearMeasures` (see `frame_boundary`).
static DEVTOOLS_MEASURE: AtomicBool = AtomicBool::new(false);

/// Toggle the DevTools User-Timing mirror at runtime.
pub fn set_devtools_measure(on: bool) {
    DEVTOOLS_MEASURE.store(on, Ordering::Relaxed);
}

/// Whether the DevTools User-Timing mirror is currently on.
pub fn devtools_measure_enabled() -> bool {
    DEVTOOLS_MEASURE.load(Ordering::Relaxed)
}

#[wasm_bindgen(
    inline_js = "export function __awsm_measure_range(name, start, end) { try { performance.measure(name, { start, end }); } catch (_) {} }"
)]
extern "C" {
    #[wasm_bindgen(js_name = __awsm_measure_range)]
    fn measure_range(name: &str, start: f64, end: f64);
}

/// `performance.now()` in ms (main thread or worker), or 0.0 if unavailable.
pub fn now_ms() -> f64 {
    crate::web_global::performance_now().unwrap_or(0.0)
}

/// Folds one scope's duration into the CPU aggregator and, when the DevTools
/// mirror is on, emits its `performance.measure`.
fn record_cpu_scope(name: &'static str, start_ms: f64, end_ms: f64) {
    record(&CPU_TIMINGS, name, end_ms - start_ms);
    if devtools_measure_enabled() {
        measure_range(name, start_ms, end_ms);
    }
}

// ---------------------------------------------------------------------------
// CPU scope names
// ---------------------------------------------------------------------------

/// Name of the whole-frame CPU scope in [`cpu_timing_stats`]: all of
/// `render()`. The one scope the [`TimingTier::Frame`] tier produces.
pub const CPU_FRAME_SCOPE: &str = "Render";
/// Every [`CpuScopeKind::Prep`] scope's name starts with this.
pub const CPU_PREP_PREFIX: &str = "Prep: ";
/// Every [`CpuScopeKind::Submit`] scope's name starts with this.
pub const CPU_SUBMIT_PREFIX: &str = "Submit: ";
/// Every [`CpuScopeKind::Hook`] scope's name ends with this.
pub const CPU_HOOK_SUFFIX: &str = " Hook";

/// What part of a frame a CPU scope times. See the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CpuScopeKind {
    /// The whole of `render()`: [`CPU_FRAME_SCOPE`].
    Frame,
    /// Work `render()` does before the first pass.
    Prep,
    /// Encoding one render or compute pass.
    Pass,
    /// Finishing and submitting the frame, and what follows the submit.
    Submit,
    /// A caller's render hook.
    Hook,
}

/// The kind of the CPU scope called `name` (a name from [`cpu_timing_stats`]).
pub fn cpu_scope_kind(name: &str) -> CpuScopeKind {
    if name == CPU_FRAME_SCOPE {
        CpuScopeKind::Frame
    } else if name.starts_with(CPU_PREP_PREFIX) {
        CpuScopeKind::Prep
    } else if name.starts_with(CPU_SUBMIT_PREFIX) {
        CpuScopeKind::Submit
    } else if name.ends_with(CPU_HOOK_SUFFIX) {
        CpuScopeKind::Hook
    } else {
        CpuScopeKind::Pass
    }
}

// ---------------------------------------------------------------------------
// CPU scope guard
// ---------------------------------------------------------------------------

/// RAII CPU timing scope. Construct with [`cpu_scope`]; on drop it folds its
/// wall duration into the CPU aggregator and — when the DevTools mirror is on —
/// emits a `performance.measure`. Holding an `Option<CpuScope>` that is `None`
/// (profiling off) costs nothing.
pub struct CpuScope {
    name: &'static str,
    start: f64,
}

impl Drop for CpuScope {
    fn drop(&mut self) {
        record_cpu_scope(self.name, self.start, now_ms());
    }
}

/// Times consecutive stretches of straight-line code as CPU scopes, one clock
/// read per boundary: [`Self::lap`] ends the stretch that began at the last
/// boundary and names it. Like [`cpu_scope`] with `sub_frame = true`, it does
/// nothing at all (no clock read) unless the tier is [`TimingTier::SubFrame`].
pub(crate) struct CpuLaps {
    /// Where the current stretch began; `None` while the tier is off.
    since_ms: Option<f64>,
}

impl CpuLaps {
    pub(crate) fn start(logging: &AwsmRendererLogging) -> Self {
        Self {
            since_ms: logging.cpu.sub_frame().then(now_ms),
        }
    }

    /// Charges the time since the last boundary to `name`.
    pub(crate) fn lap(&mut self, name: &'static str) {
        if let Some(since_ms) = self.since_ms {
            let now = now_ms();
            record_cpu_scope(name, since_ms, now);
            self.since_ms = Some(now);
        }
    }

    /// Starts a stretch here without charging the time since the last
    /// boundary to anything: after code that has scopes of its own.
    pub(crate) fn restart(&mut self) {
        if self.since_ms.is_some() {
            self.since_ms = Some(now_ms());
        }
    }
}

/// Open a CPU timing scope named `name` iff the logging tier permits it.
/// `sub_frame = true` gates on the [`TimingTier::SubFrame`] tier (per-pass
/// detail); `false` gates on any non-`Off` tier (frame-level scopes). Returns
/// `None` — a complete no-op, no `performance.now()` call — when the tier is off.
#[inline]
pub fn cpu_scope(
    logging: &AwsmRendererLogging,
    name: &'static str,
    sub_frame: bool,
) -> Option<CpuScope> {
    let active = if sub_frame {
        logging.cpu.sub_frame()
    } else {
        logging.cpu.enabled()
    };
    active.then(|| CpuScope {
        name,
        start: now_ms(),
    })
}

// ---------------------------------------------------------------------------
// GPU timestamp queries
// ---------------------------------------------------------------------------

/// Max timestamp slots (2 per instrumented pass) in the query set. Passes beyond
/// this simply aren't timed for that frame.
const GPU_MAX_SLOTS: u32 = 256;
const GPU_TS_BYTES: u32 = GPU_MAX_SLOTS * 8; // 64-bit nanosecond timestamps

struct GpuFrameScratch {
    next_slot: u32,
    records: Vec<(&'static str, u32)>,
}

struct GpuReadState {
    /// A `mapAsync` readback is in flight against `readback_buffer` — skip
    /// resolving this frame (single-buffered readback, mirrors the coverage /
    /// froxel-overflow readbacks).
    inflight: bool,
    /// Records (name, begin-slot) captured for the resolve currently pending
    /// readback.
    pending: Vec<(&'static str, u32)>,
    pending_used: u32,
}

/// GPU timestamp-query subsystem, owned by [`crate::AwsmRenderer`]. Created only
/// when the device has the `timestamp-query` feature; otherwise `None` and GPU
/// timing silently stays off. Even when present, it emits nothing while the
/// renderer's `gpu` tier is `Off`.
pub struct GpuTimestamps {
    query_set: web_sys::GpuQuerySet,
    resolve_buffer: web_sys::GpuBuffer,
    readback_buffer: web_sys::GpuBuffer,
    frame: RefCell<GpuFrameScratch>,
    state: Arc<Mutex<GpuReadState>>,
}

impl GpuTimestamps {
    /// Build the subsystem if the device supports timestamp queries. `None`
    /// otherwise (caller treats that as "GPU timing unavailable").
    pub fn new(gpu: &AwsmRendererWebGpu) -> Option<Self> {
        use awsm_renderer_core::buffers::{BufferDescriptor, BufferUsage};

        if !gpu.has_timestamp_query() {
            return None;
        }
        let query_set = gpu
            .create_query_set(
                web_sys::GpuQueryType::Timestamp,
                GPU_MAX_SLOTS,
                Some("GpuTimestamps"),
            )
            .ok()?;
        let resolve_buffer = gpu
            .create_buffer(
                &BufferDescriptor::new(
                    Some("GpuTimestampsResolve"),
                    GPU_TS_BYTES as usize,
                    BufferUsage::new().with_query_resolve().with_copy_src(),
                )
                .into(),
            )
            .ok()?;
        let readback_buffer = gpu
            .create_buffer(
                &BufferDescriptor::new(
                    Some("GpuTimestampsReadback"),
                    GPU_TS_BYTES as usize,
                    BufferUsage::new().with_map_read().with_copy_dst(),
                )
                .into(),
            )
            .ok()?;
        Some(Self {
            query_set,
            resolve_buffer,
            readback_buffer,
            frame: RefCell::new(GpuFrameScratch {
                next_slot: 0,
                records: Vec::with_capacity(GPU_MAX_SLOTS as usize / 2),
            }),
            state: Arc::new(Mutex::new(GpuReadState {
                inflight: false,
                pending: Vec::new(),
                pending_used: 0,
            })),
        })
    }

    /// Reset the per-frame slot allocator. Call once at the top of a frame
    /// (before any pass) when GPU timing is enabled.
    pub fn begin_frame(&self) {
        let mut f = self.frame.borrow_mut();
        f.next_slot = 0;
        f.records.clear();
    }

    /// Allocate a begin/end slot pair for `name`, or `None` when the query set
    /// is full for this frame.
    fn alloc_pair(&self, name: &'static str) -> Option<u32> {
        let mut f = self.frame.borrow_mut();
        if f.next_slot + 2 > GPU_MAX_SLOTS {
            return None;
        }
        let base = f.next_slot;
        f.next_slot += 2;
        f.records.push((name, base));
        Some(base)
    }

    /// Allocate a timestamp pair for a **render** pass and return the
    /// [`RenderTimestampWrites`] to splice into its descriptor. Call inline in
    /// the pass descriptor, only when the pass actually runs.
    pub fn writes_for(&self, name: &'static str) -> Option<RenderTimestampWrites<'_>> {
        self.alloc_pair(name).map(|base| RenderTimestampWrites {
            query_set: &self.query_set,
            beginning_index: Some(base),
            end_index: Some(base + 1),
        })
    }

    /// Allocate a timestamp pair for a **compute** pass and return the
    /// [`ComputeTimestampWrites`] to pass to `ComputePassDescriptor::with_timestamp_writes`.
    pub fn writes_for_compute(&self, name: &'static str) -> Option<ComputeTimestampWrites<'_>> {
        self.alloc_pair(name).map(|base| ComputeTimestampWrites {
            query_set: &self.query_set,
            beginning_index: Some(base),
            end_index: Some(base + 1),
        })
    }

    /// Record the `resolveQuerySet` + copy-to-readback into `encoder` (before
    /// `finish`/submit). Skips when nothing was timed this frame or a prior
    /// readback is still in flight. Returns `true` if a readback should be kicked
    /// after submit via [`Self::kick_readback`].
    pub fn resolve(&self, encoder: &awsm_renderer_core::command::CommandEncoder) -> bool {
        let used = self.frame.borrow().next_slot;
        if used == 0 {
            return false;
        }
        let mut st = self.state.lock().unwrap();
        if st.inflight {
            return false;
        }
        encoder.resolve_query_set(&self.query_set, 0, used, &self.resolve_buffer, 0);
        if encoder
            .copy_buffer_to_buffer(&self.resolve_buffer, 0, &self.readback_buffer, 0, used * 8)
            .is_err()
        {
            return false;
        }
        st.inflight = true;
        st.pending.clear();
        st.pending.extend_from_slice(&self.frame.borrow().records);
        st.pending_used = used;
        true
    }

    /// Kick the `mapAsync` readback (after submit). On completion, parse the
    /// 64-bit nanosecond timestamps, fold per-pass durations into the GPU
    /// aggregator, and clear the in-flight flag. Mirrors the coverage / froxel
    /// readbacks.
    pub fn kick_readback(&self) {
        let readback = self.readback_buffer.clone();
        let state = Arc::clone(&self.state);
        let used = { self.state.lock().unwrap().pending_used };
        wasm_bindgen_futures::spawn_local(async move {
            let bytes = used * 8;
            let result =
                awsm_renderer_core::buffers::extract_buffer_vec(&readback, Some(bytes)).await;
            match result {
                Ok(buf) if buf.len() as u32 >= bytes => {
                    let records = {
                        let st = state.lock().unwrap();
                        st.pending.clone()
                    };
                    // A name can be timed several times in one frame (one
                    // shadow pass per view, say): sum those first, so each
                    // name folds ONE per-frame sample and its stats read as
                    // "per frame", not "per pass".
                    let mut per_name: Vec<(&'static str, f64)> = Vec::new();
                    let mut frame_begin = u64::MAX;
                    let mut frame_end = 0u64;
                    for (name, base) in records {
                        let b = base as usize * 8;
                        let e = b + 8;
                        if e + 8 > buf.len() {
                            continue;
                        }
                        let begin = u64::from_le_bytes(buf[b..b + 8].try_into().unwrap());
                        let end = u64::from_le_bytes(buf[e..e + 8].try_into().unwrap());
                        // A zero is a slot the GPU never wrote (resolve
                        // returns 0 for those) — not a real time.
                        if begin == 0 || end <= begin {
                            continue;
                        }
                        frame_begin = frame_begin.min(begin);
                        frame_end = frame_end.max(end);
                        let ms = (end - begin) as f64 / 1_000_000.0;
                        match per_name.iter_mut().find(|(n, _)| *n == name) {
                            Some((_, total)) => *total += ms,
                            None => per_name.push((name, ms)),
                        }
                    }
                    for (name, ms) in per_name {
                        record(&GPU_TIMINGS, name, ms);
                    }
                    if frame_end > frame_begin {
                        record(
                            &GPU_TIMINGS,
                            GPU_FRAME_SCOPE,
                            (frame_end - frame_begin) as f64 / 1_000_000.0,
                        );
                    }
                }
                Ok(_) => {}
                Err(err) => {
                    tracing::warn!(target: "awsm_renderer::profiling", "GPU timestamp readback failed: {err:?}");
                }
            }
            state.lock().unwrap().inflight = false;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debug::TimingTier;

    // With the default logging nothing may measure. These run off the web,
    // where there is no clock to read: a scope or lap that tried would be
    // `Some` (and the clock read would panic outside wasm).
    #[test]
    fn default_logging_times_nothing() {
        let logging = AwsmRendererLogging::default();
        assert_eq!(logging.cpu, TimingTier::Off);
        assert_eq!(logging.gpu, TimingTier::Off);
        assert!(cpu_scope(&logging, CPU_FRAME_SCOPE, false).is_none());
        assert!(cpu_scope(&logging, "Geometry RenderPass", true).is_none());

        let mut laps = CpuLaps::start(&logging);
        assert!(laps.since_ms.is_none());
        laps.lap("Prep: Bind Groups");
        laps.restart();
        assert!(laps.since_ms.is_none());

        assert!(!cpu_timing_enabled());
        set_cpu_timing(&logging);
        assert!(!cpu_timing_enabled());
        assert_eq!(boot_clock_ms(), 0.0); // nothing listens to boot_timing here
    }

    // The frame tier is the one "Render" scope: no pass, prep or submit scope.
    #[test]
    fn frame_tier_has_no_sub_frame_scopes() {
        let logging = AwsmRendererLogging {
            cpu: TimingTier::Frame,
            ..Default::default()
        };
        assert!(cpu_scope(&logging, "Geometry RenderPass", true).is_none());
        assert!(CpuLaps::start(&logging).since_ms.is_none());
    }

    #[test]
    fn scope_kinds_by_name() {
        assert_eq!(cpu_scope_kind("Render"), CpuScopeKind::Frame);
        assert_eq!(cpu_scope_kind("Prep: Bind Groups"), CpuScopeKind::Prep);
        assert_eq!(
            cpu_scope_kind("Submit: Finish + Queue Submit"),
            CpuScopeKind::Submit
        );
        assert_eq!(cpu_scope_kind("PreRender Hook"), CpuScopeKind::Hook);
        assert_eq!(cpu_scope_kind("Geometry RenderPass"), CpuScopeKind::Pass);
        assert_eq!(cpu_scope_kind("Shadow Generation"), CpuScopeKind::Pass);
    }
}
