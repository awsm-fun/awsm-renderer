//! Quota progress bar. Used by the profile Plan tab and the admin
//! quota-usage panel.

use crate::prelude::*;

/// Render a labeled `current / max` progress bar. `current` may exceed `max`
/// (overflow visualized at 100%); `max == 0` is treated as "unlimited" and
/// renders without a fill.
pub fn render_progress_bar(label: &str, current: u64, max: u64) -> Dom {
    let pct = if max == 0 {
        None
    } else {
        Some(((current as f64 / max as f64) * 100.0).clamp(0.0, 100.0))
    };

    let fill_color = match pct {
        None => "rgba(120, 200, 130, 0.55)",
        Some(p) if p >= 95.0 => "rgba(220, 90, 90, 0.65)",
        Some(p) if p >= 80.0 => "rgba(220, 150, 60, 0.65)",
        _ => "rgba(80, 130, 200, 0.65)",
    };

    let summary = if max == 0 {
        format!("{current} (unlimited)")
    } else {
        format!("{current} / {max}")
    };

    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.25rem")
        .child(html!("div", {
            .style("display", "flex")
            .style("justify-content", "space-between")
            .style("font-size", FontSize::Sm.value())
            .child(html!("span", { .text(label) }))
            .child(html!("span", {
                .style("color", ColorRaw::MidGrey.value())
                .text(&summary)
            }))
        }))
        .child(html!("div", {
            .style("position", "relative")
            .style("width", "100%")
            .style("height", "0.45rem")
            .style("background", "rgba(255, 255, 255, 0.07)")
            .style("border-radius", "999px")
            .style("overflow", "hidden")
            .apply(|dom| match pct {
                Some(p) => dom.child(html!("div", {
                    .style("position", "absolute")
                    .style("top", "0")
                    .style("left", "0")
                    .style("height", "100%")
                    .style("width", &format!("{:.1}%", p))
                    .style("background", fill_color)
                })),
                None => dom,
            })
        }))
    })
}
