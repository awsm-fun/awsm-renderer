//! Renderer-owned, opt-in profiling: CPU scope timing and GPU timestamp queries,
//! each folded into a bounded rolling aggregator.
//!
//! **Complete no-op when disabled.** There is no always-installed tracing layer
//! and no background bookkeeping. Timing is gated at the *source*: a
//! [`CpuScope`] is only constructed when [`AwsmRendererLogging::cpu`] permits,
//! and GPU timestamp writes are only emitted when [`AwsmRendererLogging::gpu`]
//! permits. When both tiers are [`TimingTier::Off`] nothing here runs — no
//! `performance.now()`, no query set, no allocation. Because the gate is a plain
//! field on the renderer, it can be flipped **at runtime** (an editor menu, a
//! player hotkey) with the same zero-cost-when-off guarantee.
//!
//! Two rolling aggregators (CPU + GPU) hold `last/ema/min/max/count` per named
//! scope, keyed by the small fixed set of scope names → they never grow. Read
//! them with [`cpu_timing_stats`] / [`gpu_timing_stats`]; the editor surfaces
//! them in `memory_stats` and the shared `web-shared` perf HUD renders them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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
}

impl TimingStat {
    fn first(ms: f64) -> Self {
        Self {
            last_ms: ms,
            ema_ms: ms,
            min_ms: ms,
            max_ms: ms,
            count: 1,
        }
    }
    fn fold(&mut self, ms: f64) {
        self.last_ms = ms;
        self.ema_ms = EMA_ALPHA * ms + (1.0 - EMA_ALPHA) * self.ema_ms;
        self.min_ms = self.min_ms.min(ms);
        self.max_ms = self.max_ms.max(ms);
        self.count += 1;
    }
}

const EMA_ALPHA: f64 = 0.1;

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
    crate::web_global::performance()
        .map(|p| p.now())
        .unwrap_or(0.0)
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
        let end = now_ms();
        record(&CPU_TIMINGS, self.name, end - self.start);
        if devtools_measure_enabled() {
            measure_range(self.name, self.start, end);
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
const GPU_MAX_SLOTS: u32 = 32;
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

    /// Allocate a begin/end timestamp pair for a pass and return the
    /// [`RenderTimestampWrites`] to splice into its descriptor. `None` when the
    /// query set is full for this frame. Call inline in the pass descriptor,
    /// only when the pass actually runs.
    pub fn writes_for(&self, name: &'static str) -> Option<RenderTimestampWrites<'_>> {
        let mut f = self.frame.borrow_mut();
        if f.next_slot + 2 > GPU_MAX_SLOTS {
            return None;
        }
        let base = f.next_slot;
        f.next_slot += 2;
        f.records.push((name, base));
        Some(RenderTimestampWrites {
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
                    for (name, base) in records {
                        let b = base as usize * 8;
                        let e = b + 8;
                        if e + 8 > buf.len() {
                            continue;
                        }
                        let begin = u64::from_le_bytes(buf[b..b + 8].try_into().unwrap());
                        let end = u64::from_le_bytes(buf[e..e + 8].try_into().unwrap());
                        if end > begin {
                            let ms = (end - begin) as f64 / 1_000_000.0;
                            record(&GPU_TIMINGS, name, ms);
                        }
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
