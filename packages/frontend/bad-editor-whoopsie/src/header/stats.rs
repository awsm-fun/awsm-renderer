//! Bottom "scene stats" strip — toggled from the overflow menu.
//! Surfaces per-tick scene counts plus an asset-loading badge while
//! any deferred load is in flight.

use crate::{prelude::*, state};

pub(super) fn render_stats_panel(scene_stats_visible: Mutable<bool>) -> Dom {
    static PANEL: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "flex")
            .style("align-items", "center")
            .style("justify-content", "space-between")
            .style("padding", "0.45rem 0.75rem")
            .style("font-family", "monospace")
            .style("font-size", "0.8rem")
            .style("background-color", ColorRaw::Darkest.value())
            .style("border-top", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        }
    });

    let scene = state::app_state().scene.clone();
    let scene_for_stats = scene.clone();
    let loading_count = state::app_state()
        .renderer_bridge
        .assets
        .loading_count
        .clone();

    html!("div", {
        .class(&*PANEL)
        .child(html!("div", {
            .style("display", "flex")
            .style("gap", "1rem")
            .style("align-items", "center")
            .child(html!("div", {
                .text_signal(scene.revision.signal().map(move |_| scene_for_stats.stats().format()))
            }))
            .child_signal(loading_count.signal().map(|n| {
                if n > 0 {
                    Some(html!("div", {
                        .style("color", ColorRaw::Accent.value())
                        .text(&format!("· {n} loading"))
                    }))
                } else {
                    None
                }
            }))
        }))
        .child(html!("button", {
            .style("border", "0")
            .style("background", "transparent")
            .style("color", ColorText::SidebarHeader.value())
            .style("cursor", "pointer")
            .style("font-size", "0.9rem")
            .text("×")
            .event(clone!(scene_stats_visible => move |_: events::Click| {
                scene_stats_visible.set(false);
            }))
        }))
    })
}
