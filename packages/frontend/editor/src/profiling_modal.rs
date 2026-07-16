//! Runtime profiling controls, gathered into one modal (opened from the overflow
//! menu → "Profiling…"). Everything here mutates the *live* renderer, so you can
//! turn profiling on, read the perf HUD, and turn it back off to a genuine
//! no-op — no reload. See `docs/PROFILING.md`.

use awsm_renderer::debug::TimingTier;
use awsm_renderer_web_shared::perf_hud;

use crate::prelude::*;

/// Local mirrors of the renderer's live tiers, so the segmented controls can
/// show the active option. Synced from the renderer in [`open`].
static CPU_TIER: LazyLock<Mutable<TimingTier>> = LazyLock::new(|| Mutable::new(TimingTier::Off));
static GPU_TIER: LazyLock<Mutable<TimingTier>> = LazyLock::new(|| Mutable::new(TimingTier::Off));
static DEVTOOLS: LazyLock<Mutable<bool>> = LazyLock::new(|| Mutable::new(false));

/// Open the Profiling modal, first syncing the local mirrors from the live
/// renderer state.
pub fn open() {
    DEVTOOLS.set_neq(awsm_renderer::profiling::devtools_measure_enabled());
    spawn_local(async {
        let (cpu, gpu) =
            crate::engine::context::with_renderer_mut(|r| (r.logging.cpu, r.logging.gpu)).await;
        CPU_TIER.set_neq(cpu);
        GPU_TIER.set_neq(gpu);
    });
    Modal::open(content);
}

fn set_cpu_tier(t: TimingTier) {
    CPU_TIER.set_neq(t);
    spawn_local(async move {
        crate::engine::context::with_renderer_mut(|r| r.logging.cpu = t).await;
    });
    if t != TimingTier::Off {
        perf_hud::set_visible(true);
    }
}

fn set_gpu_tier(t: TimingTier) {
    GPU_TIER.set_neq(t);
    spawn_local(async move {
        crate::engine::context::with_renderer_mut(|r| r.logging.gpu = t).await;
    });
    if t != TimingTier::Off {
        perf_hud::set_visible(true);
    }
}

fn tier_label(t: TimingTier) -> &'static str {
    match t {
        TimingTier::Off => "Off",
        TimingTier::Frame => "Frame",
        TimingTier::SubFrame => "Sub-frame",
    }
}

/// A label + a 3-way segmented [Off | Frame | Sub-frame] control bound to
/// `tier`, calling `set` on change.
fn tier_row(label: &str, tier: &'static LazyLock<Mutable<TimingTier>>, set: fn(TimingTier)) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("justify-content", "space-between")
        .style("gap", "12px")
        .child(html!("span", {
            .style("font-size", "13px")
            .style("color", "var(--text-1)")
            .text(label)
        }))
        .child_signal(tier.signal().map(move |cur| Some(html!("div", {
            .style("display", "flex")
            .style("gap", "6px")
            .children([TimingTier::Off, TimingTier::Frame, TimingTier::SubFrame].into_iter().map(move |t| {
                Btn::new()
                    .label(tier_label(t))
                    .variant(if t == cur { BtnVariant::Primary } else { BtnVariant::Ghost })
                    .on_click(move || set(t))
                    .render()
            }))
        }))))
    })
}

/// A label + an on/off toggle button bound to `state`, calling `set` on click.
fn toggle_row(label: &str, state: &'static LazyLock<Mutable<bool>>, set: fn(bool)) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("justify-content", "space-between")
        .style("gap", "12px")
        .child(html!("span", {
            .style("font-size", "13px")
            .style("color", "var(--text-1)")
            .text(label)
        }))
        .child_signal(state.signal().map(move |on| Some(
            Btn::new()
                .label(if on { "On" } else { "Off" })
                .variant(if on { BtnVariant::Primary } else { BtnVariant::Ghost })
                .on_click(move || set(!on))
                .render()
        )))
    })
}

fn set_devtools(on: bool) {
    DEVTOOLS.set_neq(on);
    awsm_renderer::profiling::set_devtools_measure(on);
}

fn set_hud(on: bool) {
    perf_hud::set_visible(on);
}

fn content() -> Dom {
    ModalCard::new("Profiling")
        .width(420.0)
        .child(html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("gap", "14px")
            .child(html!("p", {
                .style("margin", "0")
                .style("font-size", "12px")
                .style("color", "var(--text-2)")
                .style("line-height", "1.5")
                .text("All off = a complete no-op. Turning a tier on shows the perf HUD over the canvas. GPU timing needs the timestamp-query device feature. See docs/PROFILING.md.")
            }))
            .child(tier_row("CPU timing", &CPU_TIER, set_cpu_tier))
            .child(tier_row("GPU timing", &GPU_TIER, set_gpu_tier))
            .child(toggle_row("Perf HUD", perf_hud_visible_mirror(), set_hud))
            .child(toggle_row("DevTools flame chart", &DEVTOOLS, set_devtools))
        }))
        .footer(Btn::new().label("Close").variant(BtnVariant::Primary).on_click(Modal::close).render())
        .render()
}

/// The HUD visibility `Mutable` is owned by `web-shared`; expose it through the
/// `&'static LazyLock` shape the rows expect via a thin local wrapper.
fn perf_hud_visible_mirror() -> &'static LazyLock<Mutable<bool>> {
    static MIRROR: LazyLock<Mutable<bool>> = LazyLock::new(perf_hud::visible);
    &MIRROR
}
