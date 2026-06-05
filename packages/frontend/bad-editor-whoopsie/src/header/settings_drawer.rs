//! Settings drawer — a right slide-out replacing the old "Editor" ribbon tab.
//! Holds the viewport toggles (Grid / Gizmo / MSAA / Light Heatmap); each
//! routes through `actions::view::*` so the renderer + `AppState` stay in sync.

use crate::{actions, prelude::*, state};

static CLOSE_BTN: LazyLock<String> = LazyLock::new(|| {
    class! {
        .style("display", "inline-flex")
        .style("align-items", "center")
        .style("justify-content", "center")
        .style("width", "26px")
        .style("height", "26px")
        .style("border-radius", "var(--r2)")
        .style("cursor", "pointer")
        .style("color", "var(--text-2)")
        .style("font-size", "16px")
        .pseudo!(":hover", {
            .style("background", "var(--bg-hover)")
        })
    }
});

/// Mounts the drawer (renders nothing until `settings_open` is true).
pub fn render() -> Dom {
    let open = state::app_state().settings_open.clone();
    html!("div", {
        .child_signal(open.signal().map(clone!(open => move |is_open| {
            if is_open { Some(render_drawer(open.clone())) } else { None }
        })))
    })
}

fn render_drawer(open: Mutable<bool>) -> Dom {
    html!("div", {
        // backdrop
        .child(html!("div", {
            .style("position", "fixed")
            .style("inset", "0")
            .style("background", "oklch(0 0 0 / 0.4)")
            .style("z-index", "200")
            .event(clone!(open => move |_: events::Click| open.set_neq(false)))
        }))
        // panel
        .child(html!("div", {
            .style("position", "fixed")
            .style("top", "0")
            .style("right", "0")
            .style("bottom", "0")
            .style("width", "320px")
            .style("background", "var(--bg-1)")
            .style("border-left", "1px solid var(--line)")
            .style("box-shadow", "var(--shadow-3)")
            .style("z-index", "201")
            .style("display", "flex")
            .style("flex-direction", "column")
            .child(html!("div", {
                .style("display", "flex")
                .style("align-items", "center")
                .style("height", "44px")
                .style("padding", "0 10px 0 16px")
                .style("border-bottom", "1px solid var(--line-soft)")
                .child(html!("span", {
                    .style("font-size", "13px")
                    .style("font-weight", "620")
                    .style("color", "var(--text-0)")
                    .text("Settings")
                }))
                .child(html!("button", {
                    .class(["t", &*CLOSE_BTN])
                    .style("margin-left", "auto")
                    .text("✕")
                    .event(clone!(open => move |_: events::Click| open.set_neq(false)))
                }))
            }))
            .child(html!("div", {
                .style("flex", "1 1 0")
                .style("overflow-y", "auto")
                .style("padding", "16px")
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("gap", "14px")
                .child(section_label("Viewport"))
                .child(toggle_grid())
                .child(toggle_gizmo())
                .child(toggle_msaa())
                .child(toggle_heatmap())
            }))
        }))
    })
}

fn section_label(text: &str) -> Dom {
    html!("div", {
        .class("kicker")
        .style("margin-bottom", "2px")
        .text(text)
    })
}

fn toggle_grid() -> Dom {
    let grid_enabled = state::app_state().grid_enabled.clone();
    Checkbox::new(CheckboxStyle::Dark)
        .with_selected_signal(grid_enabled.signal())
        .with_content_after(html!("span", { .text("Show Grid") }))
        .with_on_click(clone!(grid_enabled => move || {
            actions::view::set_grid_enabled(!grid_enabled.get());
        }))
        .render()
}

fn toggle_gizmo() -> Dom {
    let gizmo_enabled = state::app_state().gizmo_enabled.clone();
    Checkbox::new(CheckboxStyle::Dark)
        .with_selected_signal(gizmo_enabled.signal())
        .with_content_after(html!("span", { .text("Show Gizmo") }))
        .with_on_click(clone!(gizmo_enabled => move || {
            actions::view::set_gizmo_enabled(!gizmo_enabled.get());
        }))
        .render()
}

fn toggle_msaa() -> Dom {
    let anti_aliasing = state::app_state().anti_aliasing.clone();
    Checkbox::new(CheckboxStyle::Dark)
        .with_selected_signal(anti_aliasing.signal_ref(|aa| aa.msaa_sample_count.is_some()))
        .with_content_after(html!("span", { .text("MSAA Anti-Aliasing") }))
        .with_on_click(|| {
            actions::view::toggle_msaa();
        })
        .render()
}

fn toggle_heatmap() -> Dom {
    let debug_light_heatmap = state::app_state().debug_light_heatmap.clone();
    Checkbox::new(CheckboxStyle::Dark)
        .with_selected_signal(debug_light_heatmap.signal())
        .with_content_after(html!("span", { .text("Light Heatmap") }))
        .with_on_click(|| {
            actions::view::toggle_light_heatmap();
        })
        .render()
}
