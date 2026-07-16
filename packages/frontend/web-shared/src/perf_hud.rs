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

use std::sync::atomic::{AtomicBool, Ordering};

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
    cpu: Vec<Row>,
    gpu: Vec<Row>,
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
    wasm_bindgen_futures::spawn_local(async {
        while VISIBLE.get() {
            SNAPSHOT.set(HudData {
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

fn content(data: &HudData) -> Dom {
    html!("div", {
        .child(html!("div", {
            .style("font-weight", "700")
            .style("margin-bottom", "6px")
            .text("PERF")
        }))
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
