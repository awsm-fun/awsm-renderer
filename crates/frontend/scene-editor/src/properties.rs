//! Right-sidebar properties panel. Follows the current selection:
//! - 0 selected → hint
//! - 1 selected → full editor (name, transform, kind)
//! - 2+ selected → summary + note to narrow

pub mod asset_editor;
pub mod custom_materials_pane;
pub mod history_input;
pub mod kind_editor;
pub mod prefab;
pub mod transform;

use crate::prelude::*;
use crate::scene::{mutate, Node, NodeId};
use crate::state::app_state;
use web_sys::HtmlInputElement;

pub fn render() -> Dom {
    static CONTAINER: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("padding", "0.5rem")
            .style("gap", "0.75rem")
        }
    });

    let state = app_state();
    let selected_assets = state.selected_assets.clone();

    html!("div", {
        .class(&*CONTAINER)
        // Non-empty asset selection takes priority over the scene-node
        // selection. Exactly-one routes to the asset editor; multiple
        // routes to a batch summary with a "Delete selected" button.
        // Empty falls through to the node-based inspector.
        .child_signal(selected_assets.signal_cloned().map(move |set| {
            Some(match set.len() {
                0 => render_for_node_selection(),
                1 => asset_editor::render(*set.iter().next().unwrap()),
                _ => asset_editor::render_batch(&set),
            })
        }))
    })
}

fn render_for_node_selection() -> Dom {
    let state = app_state();
    let selected = state.selected.clone();
    html!("div", {
        .child_signal(selected.signal_ref(|set| set.len()).dedupe().map(move |count| {
            Some(match count {
                0 => render_empty(),
                1 => render_single_or_empty(),
                n => render_multi(n),
            })
        }))
    })
}

fn render_empty() -> Dom {
    html!("div", {
        .style("color", ColorText::Byline.value())
        .style("font-size", "0.85rem")
        .style("line-height", "1.4")
        .text("Nothing selected. Click a node in the tree to inspect its properties.")
    })
}

fn render_multi(n: usize) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.45rem")
        .child(html!("div", {
            .style("font-weight", "600")
            .style("font-size", "0.95rem")
            .text(&format!("{n} nodes selected"))
        }))
        .child(html!("div", {
            .style("font-size", "0.8rem")
            .style("line-height", "1.4")
            .style("color", ColorText::Byline.value())
            .text("Select a single node to edit its properties. Use the Object tab or right-click for bulk actions.")
        }))
    })
}

fn render_single_or_empty() -> Dom {
    // Re-look up the single id each time the selection changes (we're
    // inside the .signal_ref(|set| set.len()) branch but still need the
    // actual id).
    let state = app_state();
    let id_signal = state.selected.signal_ref(|set| set.iter().next().copied());

    html!("div", {
        .child_signal(id_signal.map(|id| id.map(render_single)))
    })
}

fn render_single(id: NodeId) -> Dom {
    let state = app_state();
    let Some(node) = mutate::find_by_id(&state.scene, id) else {
        return render_empty();
    };

    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.25rem")
        .child(render_name_input(node.clone()))
        .child(prefab::render(node.clone()))
        .child(transform::render(node.clone()))
        .child(kind_editor::render(node))
    })
}

fn render_name_input(node: Arc<Node>) -> Dom {
    static INPUT: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("width", "100%")
            .style("padding", "0.4rem 0.55rem")
            .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
            .style("border-radius", "0.3rem")
            .style("background-color", ColorRaw::Darkest.value())
            .style("color", ColorText::SidebarHeader.value())
            .style("font-size", "0.95rem")
            .style("font-weight", "600")
        }
    });

    let name = node.name.clone();
    let editing = Mutable::new(false);

    html!("input" => HtmlInputElement, {
        .class(&*INPUT)
        .attr("type", "text")
        .with_node!(input => {
            .future(clone!(name, editing, input => {
                name.signal_cloned().for_each(move |n| {
                    if !editing.get() {
                        input.set_value(&n);
                    }
                    async {}
                })
            }))
            .event(clone!(editing => move |_: events::FocusIn| {
                editing.set_neq(true);
            }))
            .event(clone!(editing, input, name => move |_: events::FocusOut| {
                editing.set_neq(false);
                let new_value = input.value();
                if new_value != name.get_cloned() {
                    let state = app_state();
                    let previous = state.snapshot_scene();
                    name.set(new_value);
                    state.scene.bump_revision();
                    state.commit_history(previous);
                }
            }))
        })
    })
}
