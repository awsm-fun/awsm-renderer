//! Live, reactive tree view of the scene. Rendered inside `SidebarLeft`.

pub mod context_menu;
pub mod drag;
pub mod icons;
pub mod rows;

use crate::prelude::*;
use crate::state::app_state;

pub fn render() -> Dom {
    static CONTAINER: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("width", "100%")
            .style("height", "100%")
            .style("min-width", "max-content")
        }
    });

    static ROWS: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("flex", "1 1 0")
            .style("min-height", "0")
            .style("overflow-y", "auto")
            .style("padding", "0.35rem 0")
        }
    });

    let state = app_state();
    let scene = state.scene.clone();

    html!("div", {
        .class(&*CONTAINER)
        .child(context_menu::render_overlay())
        .child(render_panel_header())
        .child(render_filter())
        .child(html!("div", {
            .class(&*ROWS)
            .child_signal(scene.nodes.signal_vec_cloned().len().map(|len| {
                if len == 0 {
                    Some(render_empty_hint())
                } else {
                    None
                }
            }))
            .children_signal_vec(scene.nodes.signal_vec_cloned().map(|node| {
                rows::render_subtree(node, 0)
            }))
        }))
    })
}

/// "Outliner" panel header — kicker title + an add (+) button.
fn render_panel_header() -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("height", "34px")
        .style("padding", "0 8px 0 12px")
        .style("border-bottom", "1px solid var(--line-soft)")
        .style("flex", "0 0 auto")
        .child(html!("span", { .class("kicker").text("Outliner") }))
        .child(html!("button", {
            .class(["t", &*ADD_BTN])
            .style("margin-left", "auto")
            .attr("title", "Add object")
            .text("+")
            .event(|_: events::Click| crate::actions::insert::empty())
        }))
    })
}

static ADD_BTN: LazyLock<String> = LazyLock::new(|| {
    class! {
        .style("display", "inline-flex")
        .style("align-items", "center")
        .style("justify-content", "center")
        .style("width", "22px")
        .style("height", "22px")
        .style("border-radius", "var(--r1)")
        .style("cursor", "pointer")
        .style("color", "var(--text-2)")
        .style("font-size", "16px")
        .style("line-height", "1")
        .pseudo!(":hover", {
            .style("background", "var(--bg-hover)")
            .style("color", "var(--text-0)")
        })
    }
});

/// Filter input — writes `tree_filter`; leaf rows react via a name-match signal.
fn render_filter() -> Dom {
    let filter = app_state().tree_filter.clone();
    html!("div", {
        .style("padding", "6px 8px")
        .style("flex", "0 0 auto")
        .child(html!("input" => web_sys::HtmlInputElement, {
            .style("width", "100%")
            .style("box-sizing", "border-box")
            .style("height", "26px")
            .style("padding", "0 9px")
            .style("background", "var(--bg-3)")
            .style("border", "1px solid var(--line-soft)")
            .style("border-radius", "var(--r2)")
            .style("outline", "none")
            .style("color", "var(--text-0)")
            .style("font-size", "12px")
            .attr("placeholder", "Filter…")
            .with_node!(input => {
                .event(clone!(filter => move |_: events::Input| {
                    filter.set_neq(input.value());
                }))
            })
        }))
    })
}

fn render_empty_hint() -> Dom {
    html!("div", {
        .style("padding", "0.75rem 0.65rem")
        .style("color", ColorText::Byline.value())
        .style("font-size", "0.85rem")
        .style("line-height", "1.4")
        .text("Scene is empty. Use Insert Model or Insert ▾ in the header to add nodes.")
    })
}
