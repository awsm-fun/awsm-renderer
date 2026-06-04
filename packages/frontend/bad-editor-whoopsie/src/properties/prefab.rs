//! Static / Prefab dropdown for the currently-selected node.
//!
//! The flag is **root-only** in the data model — descendants of a prefab
//! don't inherit the marker, and any descendant may itself be marked
//! Prefab to create a nested prefab. Toggling here just flips
//! `Node::prefab` and commits a history entry; nothing cascades.
//!
//! See `docs/game-editor-player.md` for the full prefab model.

use crate::prelude::*;
use crate::scene::Node;
use crate::state::app_state;
use web_sys::HtmlSelectElement;

const VALUE_STATIC: &str = "static";
const VALUE_PREFAB: &str = "prefab";

pub fn render(node: Arc<Node>) -> Dom {
    static SECTION: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "grid")
            .style("grid-template-columns", "7rem 1fr")
            .style("gap", "0.5rem")
            .style("align-items", "center")
            .style("padding", "0.5rem 0")
        }
    });

    static LABEL: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("font-size", "0.75rem")
            .style("font-weight", "600")
            .style("text-transform", "uppercase")
            .style("letter-spacing", "0.05em")
            .style("color", ColorText::Byline.value())
        }
    });

    html!("div", {
        .class(&*SECTION)
        .child(html!("span", {
            .class(&*LABEL)
            .text("Kind")
        }))
        .child(render_dropdown(node))
    })
}

fn render_dropdown(node: Arc<Node>) -> Dom {
    static SELECT: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("width", "100%")
            .style("padding", "0.35rem 0.5rem")
            .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
            .style("border-radius", "0.3rem")
            .style("background-color", ColorRaw::Darkest.value())
            .style("color", ColorText::SidebarHeader.value())
            .style("font-size", "0.85rem")
            .style("cursor", "pointer")
        }
    });

    let prefab = node.prefab.clone();

    html!("select" => HtmlSelectElement, {
        .class(&*SELECT)
        .child(html!("option", {
            .attr("value", VALUE_STATIC)
            .text("Static")
        }))
        .child(html!("option", {
            .attr("value", VALUE_PREFAB)
            .text("Prefab")
        }))
        .with_node!(select => {
            // Sync the displayed value with the underlying signal so undo /
            // redo / external sets are reflected here. `set_value` is a
            // no-op when the value already matches, so we don't fight the
            // user's in-progress click.
            .future(clone!(prefab, select => {
                prefab.signal().for_each(move |is_prefab| {
                    let target = if is_prefab { VALUE_PREFAB } else { VALUE_STATIC };
                    if select.value() != target {
                        select.set_value(target);
                    }
                    async {}
                })
            }))
            .event(clone!(prefab, select => move |_: events::Change| {
                let new_value = select.value();
                let new_is_prefab = new_value == VALUE_PREFAB;
                if new_is_prefab == prefab.get() {
                    return;
                }
                let state = app_state();
                let previous = state.snapshot_scene();
                prefab.set(new_is_prefab);
                state.scene.bump_revision();
                state.commit_history(previous);
            }))
        })
    })
}
