//! Cast / receive shadow toggle pair for a mesh-bearing node
//! (`Primitive`, `Mesh`, `SweepAlongCurve`, `InstancesAlongCurve`,
//! `Model`). Reads / writes `MeshShadowConfig` through `node.kind`.
//!
//! Sprites, lines, and particles deliberately don't expose these
//! toggles — they have hardcoded no-cast / no-receive behaviour in v1.

use awsm_scene_schema::MeshShadowConfig;

use crate::prelude::*;
use crate::properties::kind_editor::{field_row, section_header};
use crate::scene::{Node, NodeKind};
use crate::state::app_state;

/// Render the Shadows sub-panel for a mesh-bearing node. The closures
/// pick out the `MeshShadowConfig` for the node's specific variant so
/// this module is variant-agnostic.
pub(super) fn render<R, M>(node: Arc<Node>, read: R, mutate: M) -> Dom
where
    R: Fn(&NodeKind) -> Option<MeshShadowConfig> + Clone + 'static,
    M: Fn(&mut NodeKind, MeshShadowConfig) + Clone + 'static,
{
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .style("margin-top", "0.5rem")
        .style("padding-top", "0.5rem")
        .style("border-top", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .child(section_header("Shadows"))
        .child(field_row("Cast", toggle(node.clone(), read.clone(), mutate.clone(), ShadowFlag::Cast)))
        .child(field_row("Receive", toggle(node, read, mutate, ShadowFlag::Receive)))
    })
}

#[derive(Clone, Copy)]
enum ShadowFlag {
    Cast,
    Receive,
}

fn toggle<R, M>(node: Arc<Node>, read: R, mutate: M, flag: ShadowFlag) -> Dom
where
    R: Fn(&NodeKind) -> Option<MeshShadowConfig> + Clone + 'static,
    M: Fn(&mut NodeKind, MeshShadowConfig) + 'static,
{
    let kind = node.kind.clone();
    let read_for_signal = read.clone();
    let checked = kind.signal_cloned().map(move |k| {
        read_for_signal(&k)
            .map(|c| match flag {
                ShadowFlag::Cast => c.cast,
                ShadowFlag::Receive => c.receive,
            })
            .unwrap_or(false)
    });

    html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "checkbox")
        .style("width", "1rem")
        .style("height", "1rem")
        .style("cursor", "pointer")
        .with_node!(input => {
            .future(clone!(input => {
                checked.for_each(move |c| {
                    if input.checked() != c {
                        input.set_checked(c);
                    }
                    async {}
                })
            }))
            .event(clone!(input => move |_: events::Change| {
                let v = input.checked();
                let mut k = kind.get_cloned();
                if let Some(mut c) = read(&k) {
                    match flag {
                        ShadowFlag::Cast => c.cast = v,
                        ShadowFlag::Receive => c.receive = v,
                    }
                    mutate(&mut k, c);
                    let state = app_state();
                    let previous = state.snapshot_scene();
                    kind.set(k);
                    state.scene.bump_revision();
                    state.commit_history(previous);
                }
            }))
        })
    })
}
