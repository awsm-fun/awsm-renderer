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
            .style("min-width", "max-content")
            .style("padding", "0.35rem 0")
        }
    });

    let state = app_state();
    let scene = state.scene.clone();

    html!("div", {
        .class(&*CONTAINER)
        .child(context_menu::render_overlay())
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
