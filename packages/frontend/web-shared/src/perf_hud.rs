//! Shared, opt-in perf overlay for any awsm-renderer frontend (editor,
//! model-tests, real players via `lockstep-frontend-shared`).
//!
//! It renders the renderer's CPU + GPU timing aggregators
//! ([`awsm_renderer::profiling`]) as a compact fixed-position panel. Visibility
//! is a runtime [`Mutable<bool>`] seeded from `?perfhud` and toggleable from a
//! menu — **independent of whether profiling is capturing**. When profiling is
//! off the tables are simply empty (with a hint); when it's on they fill live.
//!
//! Zero cost when hidden: the sampling loop only runs while [`visible`] is true.
//!
//! Above the timing tables it always shows the frame pacing, profiling or not:
//!
//! - **display**: the refresh rate of the browser's frame clock. There is no
//!   web API for it, so it is measured: `requestAnimationFrame` timestamps are
//!   whole refresh periods apart, and the period is the typical gap among the
//!   fastest frames seen in the last few seconds (see [`FrameMeter`]). The
//!   fastest, not the median, because this loop shares the thread with the
//!   renderer: if most frames are slow, the median gap is not the display's.
//! - **fps**: frames the browser actually produced, per second.
//! - **missed**: refreshes that went by without a new frame (a gap of `n`
//!   periods misses `n − 1`), per second and in total since the HUD was shown.
//!   Any steady count is visible stutter.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

use gloo_timers::future::TimeoutFuture;

use crate::prelude::*;

/// One aggregator row, flattened for display.
#[derive(Clone, Debug)]
struct Row {
    name: String,
    ema_ms: f64,
    last_ms: f64,
    max_ms: f64,
    count: u64,
}

#[derive(Clone, Debug, Default)]
struct HudData {
    frames: FrameLine,
    cpu: Vec<Row>,
    gpu: Vec<Row>,
}

/// The frame-pacing line, ready for display.
#[derive(Clone, Debug, Default)]
struct FrameLine {
    /// Display refresh rate (Hz); `None` while still measuring.
    display_hz: Option<f64>,
    fps: f64,
    missed_per_s: u64,
    missed_total: u64,
}

/// Frame gaps kept for the period estimate (~2 s at 120 Hz).
const GAP_SAMPLES: usize = 240;
/// Gaps needed before there is any estimate.
const GAP_MIN_SAMPLES: usize = 30;
/// Which of the sorted gaps stands for "the fastest frames": the 10th
/// percentile, so a stray short gap (two callbacks bunched together) does not
/// set the period, while a renderer that makes 1 refresh in 10 still does.
const GAP_PERCENTILE: f64 = 0.10;
/// Gaps within this share of that percentile are single refreshes; their mean
/// is the window's period.
const GAP_INLIER: f64 = 0.15;
/// Window estimates kept (one per sample, so 10 s at 4 Hz). The period shown is
/// the smallest of them: a stretch of slow frames does not halve the displayed
/// rate, and moving the window to a slower screen shows within 10 s.
const PERIOD_HISTORY: usize = 40;
/// A gap longer than this is a pause (hidden tab, debugger, a long load), not
/// missed frames.
const PAUSE_MS: f64 = 250.0;
/// Samples the per-second rates are taken over (1 s at 4 Hz).
const RATE_SAMPLES: usize = 4;

/// Measures the browser's frame clock from `requestAnimationFrame` timestamps.
#[derive(Default)]
struct FrameMeter {
    last_ts: Option<f64>,
    /// Gaps between frames (ms), newest last.
    gaps: VecDeque<f64>,
    /// Gaps not yet turned into counts by [`FrameMeter::sample`].
    fresh: Vec<f64>,
    /// Per-window period estimates, newest last.
    periods: VecDeque<f64>,
    /// (frames, missed, ms) of the last few samples, for the per-second rates.
    recent: VecDeque<(u64, u64, f64)>,
    missed_total: u64,
}

impl FrameMeter {
    fn frame(&mut self, ts: f64) {
        if let Some(last) = self.last_ts.replace(ts) {
            let gap = ts - last;
            if gap > 0.0 && gap < PAUSE_MS {
                if self.gaps.len() == GAP_SAMPLES {
                    self.gaps.pop_front();
                }
                self.gaps.push_back(gap);
                self.fresh.push(gap);
            }
        }
    }

    /// The period of the latest window of gaps, if there are enough.
    fn window_period(&self) -> Option<f64> {
        if self.gaps.len() < GAP_MIN_SAMPLES {
            return None;
        }
        let mut sorted: Vec<f64> = self.gaps.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        let fast = sorted[((sorted.len() as f64) * GAP_PERCENTILE) as usize];
        let (sum, n) = sorted
            .iter()
            .filter(|g| (**g - fast).abs() <= fast * GAP_INLIER)
            .fold((0.0, 0u32), |(s, n), g| (s + g, n + 1));
        (n > 0).then_some(sum / f64::from(n))
    }

    /// Turns the gaps since the previous call into counts; returns the line.
    fn sample(&mut self) -> FrameLine {
        if let Some(p) = self.window_period() {
            if self.periods.len() == PERIOD_HISTORY {
                self.periods.pop_front();
            }
            self.periods.push_back(p);
        }
        let period = self.periods.iter().copied().reduce(f64::min);

        let fresh = std::mem::take(&mut self.fresh);
        let frames = fresh.len() as u64;
        let ms: f64 = fresh.iter().sum();
        let missed = period.map_or(0, |p| {
            fresh
                .iter()
                .map(|g| ((g / p).round() as u64).saturating_sub(1))
                .sum()
        });
        self.missed_total += missed;
        if self.recent.len() == RATE_SAMPLES {
            self.recent.pop_front();
        }
        self.recent.push_back((frames, missed, ms));

        let (f, m, t) = self
            .recent
            .iter()
            .fold((0, 0, 0.0), |(f, m, t), (a, b, c)| (f + a, m + b, t + c));
        let secs = t / 1000.0;
        FrameLine {
            display_hz: period.map(|p| 1000.0 / p),
            fps: if secs > 0.0 { f as f64 / secs } else { 0.0 },
            missed_per_s: if secs > 0.0 {
                (m as f64 / secs).round() as u64
            } else {
                0
            },
            missed_total: self.missed_total,
        }
    }
}

thread_local! {
    static METER: RefCell<FrameMeter> = RefCell::new(FrameMeter::default());
}

/// The rAF callback, held in a cell so it can schedule itself again.
type FrameCallback = Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>>;

/// Asks for the next animation frame for `cb`.
fn request_frame(cb: &FrameCallback) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let cb = cb.borrow();
    if let Some(f) = cb.as_ref() {
        let _ = window.request_animation_frame(f.as_ref().unchecked_ref());
    }
}

/// Runs a `requestAnimationFrame` loop feeding [`METER`] while the HUD is
/// visible. When the HUD is hidden the loop stops asking for frames. (The
/// closure is not dropped from inside its own call, which wasm-bindgen
/// forbids; it is a few bytes per show/hide of a debug overlay.)
fn start_frame_loop() {
    METER.with(|m| *m.borrow_mut() = FrameMeter::default());
    let cb: FrameCallback = Rc::new(RefCell::new(None));
    let next = cb.clone();
    *cb.borrow_mut() = Some(Closure::new(move |ts: f64| {
        if !VISIBLE.get() {
            return;
        }
        METER.with(|m| m.borrow_mut().frame(ts));
        request_frame(&next);
    }));
    request_frame(&cb);
}

static VISIBLE: LazyLock<Mutable<bool>> = LazyLock::new(|| Mutable::new(false));
static SNAPSHOT: LazyLock<Mutable<HudData>> = LazyLock::new(|| Mutable::new(HudData::default()));
static DRIVER_RUNNING: AtomicBool = AtomicBool::new(false);

/// Sample interval (ms) — 4 Hz is plenty for a human-read overlay.
const SAMPLE_MS: u32 = 250;

/// Seed HUD visibility from the page URL (`?perfhud`). Call once at boot. Safe
/// to call regardless of whether profiling is enabled.
pub fn init_from_url() {
    set_visible(crate::perf::query_flag("perfhud"));
}

/// The shared visibility handle — bind menu toggles / hotkeys to it.
pub fn visible() -> Mutable<bool> {
    VISIBLE.clone()
}

/// Show or hide the overlay. Starts the sampling loop on first show.
pub fn set_visible(on: bool) {
    VISIBLE.set_neq(on);
    if on {
        start_driver();
    }
}

/// Flip the overlay's visibility.
pub fn toggle() {
    set_visible(!VISIBLE.get());
}

fn map_rows(stats: Vec<(&'static str, awsm_renderer::profiling::TimingStat)>) -> Vec<Row> {
    stats
        .into_iter()
        .map(|(name, s)| Row {
            name: name.to_string(),
            ema_ms: s.ema_ms,
            last_ms: s.last_ms,
            max_ms: s.max_ms,
            count: s.count,
        })
        .collect()
}

/// Start the 4 Hz sampling loop if not already running. It samples the renderer
/// aggregators into [`SNAPSHOT`] and exits as soon as the HUD is hidden, so a
/// hidden HUD costs nothing.
fn start_driver() {
    if DRIVER_RUNNING.swap(true, Ordering::Relaxed) {
        return; // already running
    }
    start_frame_loop();
    wasm_bindgen_futures::spawn_local(async {
        while VISIBLE.get() {
            SNAPSHOT.set(HudData {
                frames: METER.with(|m| m.borrow_mut().sample()),
                cpu: map_rows(awsm_renderer::profiling::cpu_timing_stats()),
                gpu: map_rows(awsm_renderer::profiling::gpu_timing_stats()),
            });
            TimeoutFuture::new(SAMPLE_MS).await;
        }
        DRIVER_RUNNING.store(false, Ordering::Relaxed);
    });
}

fn fmt_ms(v: f64) -> String {
    format!("{v:.2}")
}

fn table(title: &str, rows: &[Row]) -> Dom {
    html!("div", {
        .style("margin-bottom", "6px")
        .child(html!("div", {
            .style("opacity", "0.6")
            .style("margin-bottom", "2px")
            .text(title)
        }))
        .children(rows.iter().map(|r| {
            html!("div", {
                .style("display", "flex")
                .style("justify-content", "space-between")
                .style("gap", "10px")
                .child(html!("span", { .text(&r.name) }))
                .child(html!("span", {
                    .style("opacity", "0.85")
                    .text(&format!(
                        "{} ms  (last {}, max {}, ×{})",
                        fmt_ms(r.ema_ms), fmt_ms(r.last_ms), fmt_ms(r.max_ms), r.count
                    ))
                }))
            })
        }))
    })
}

fn frame_line(f: &FrameLine) -> Dom {
    let display = f
        .display_hz
        .map_or_else(|| "measuring…".to_string(), |hz| format!("{hz:.0} Hz"));
    html!("div", {
        .style("margin-bottom", "6px")
        .child(html!("div", { .text(&format!("display {display} · {:.0} fps", f.fps)) }))
        .child(html!("div", {
            .style("color", if f.missed_per_s > 0 { "#ff8a7a" } else { "inherit" })
            .text(&format!("missed {}/s · {} total", f.missed_per_s, f.missed_total))
        }))
    })
}

fn content(data: &HudData) -> Dom {
    html!("div", {
        .child(html!("div", {
            .style("font-weight", "700")
            .style("margin-bottom", "6px")
            .text("PERF")
        }))
        .child(frame_line(&data.frames))
        .apply(|b| {
            if data.cpu.is_empty() && data.gpu.is_empty() {
                b.child(html!("div", {
                    .style("opacity", "0.6")
                    .style("max-width", "220px")
                    .text("no samples — enable ?trace / ?gputime (or the Profiling menu)")
                }))
            } else {
                b.child(table("CPU (ms/frame)", &data.cpu))
                    .child(table("GPU (ms/frame)", &data.gpu))
            }
        })
    })
}

/// The overlay `Dom`. Mount it inside a **positioned** container (e.g. the
/// editor viewport) — it anchors to that container's top-left corner so it sits
/// over the canvas rather than the app chrome. Shows/hides itself via the shared
/// [`visible`] signal and updates while shown; renders nothing when hidden.
pub fn render() -> Dom {
    html!("div", {
        .child_signal(VISIBLE.signal().map(|vis| {
            if !vis {
                return None;
            }
            Some(html!("div", {
                .style("position", "absolute")
                .style("top", "8px")
                .style("left", "8px")
                .style("z-index", "40")
                .style("pointer-events", "none")
                .style("font-family", "ui-monospace, SFMono-Regular, Menlo, monospace")
                .style("font-size", "11px")
                .style("line-height", "1.4")
                .style("color", "#e6e6e6")
                .style("background", "rgba(12,12,14,0.82)")
                .style("border", "1px solid rgba(255,255,255,0.12)")
                .style("border-radius", "6px")
                .style("padding", "8px 10px")
                .style("min-width", "180px")
                .child_signal(SNAPSHOT.signal_cloned().map(|d| Some(content(&d))))
            }))
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds frames at the given gaps (ms), starting at t = 0.
    fn feed(meter: &mut FrameMeter, gaps: impl IntoIterator<Item = f64>) {
        let mut t = meter.last_ts.unwrap_or(0.0);
        if meter.last_ts.is_none() {
            meter.frame(t);
        }
        for gap in gaps {
            t += gap;
            meter.frame(t);
        }
    }

    #[test]
    fn steady_frames_read_as_the_display_rate_with_nothing_missed() {
        let mut meter = FrameMeter::default();
        feed(&mut meter, std::iter::repeat_n(1000.0 / 120.0, 120));
        let line = meter.sample();
        assert_eq!(line.display_hz.map(f64::round), Some(120.0));
        assert_eq!(line.missed_total, 0);
        assert!((line.fps - 120.0).abs() < 0.5);
    }

    #[test]
    fn a_double_gap_misses_one_refresh_and_a_pause_misses_none() {
        let p = 1000.0 / 60.0;
        let mut meter = FrameMeter::default();
        feed(&mut meter, std::iter::repeat_n(p, 60));
        meter.sample();
        // One frame took two refreshes, one took three, then a tab-hide pause.
        feed(&mut meter, [2.0 * p, 3.0 * p, 5000.0, p, p]);
        let line = meter.sample();
        assert_eq!(line.missed_total, 3);
    }

    #[test]
    fn mostly_slow_frames_do_not_halve_the_display_rate() {
        // A renderer that makes only 1 refresh in 5 still shows the display's
        // rate, and the slow frames count as missed.
        let p = 1000.0 / 120.0;
        let mut meter = FrameMeter::default();
        let gaps = (0..200).map(|i| if i % 5 == 0 { p } else { 2.0 * p });
        feed(&mut meter, gaps);
        let line = meter.sample();
        assert_eq!(line.display_hz.map(f64::round), Some(120.0));
        assert_eq!(line.missed_total, 160);
    }
}
