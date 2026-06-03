use crate::prelude::*;
use crate::properties::transform::number_input;
use crate::scene::{Node, NodeKind};
use crate::state::app_state;

use super::helpers::{field_row, section_header};
use super::mesh_shadow;

pub(super) fn render_model_editor(node: Arc<Node>) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(section_header("Model"))
        .child(field_row("Asset", html!("div", {
            .style("font-family", "monospace")
            .style("font-size", "0.8rem")
            .style("color", ColorText::SidebarHeader.value())
            .style("word-break", "break-all")
            .text_signal(node.kind.signal_cloned().map(|k| match k {
                NodeKind::Model(r) => app_state()
                    .scene
                    .assets
                    .lock()
                    .unwrap()
                    .display_name(r.asset_id)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("<missing: {}>", r.asset_id)),
                _ => String::new(),
            }))
        })))
        .child(field_row("Node index", model_node_index_input(node.clone())))
        .child(mesh_shadow::render(
            node,
            |k| match k {
                NodeKind::Model(r) => Some(r.shadow),
                _ => None,
            },
            |k, new_shadow| {
                if let NodeKind::Model(r) = k {
                    r.shadow = new_shadow;
                }
            },
        ))
    })
}

fn model_node_index_input(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(|k| match k {
        NodeKind::Model(r) => r.node_index as f32,
        _ => 0.0,
    });
    number_input(value_signal, move |new_value| {
        let n = new_value.max(0.0) as u32;
        let mut k = kind.get_cloned();
        if let NodeKind::Model(ref mut r) = k {
            r.node_index = n;
            kind.set(k);
        }
    })
}
