// ─────────────────────────────────────────────────────────────────────
// InstancesAlongCurve editor
// ─────────────────────────────────────────────────────────────────────

use crate::prelude::*;
use crate::properties::transform::number_input;
use crate::scene::{Node, NodeKind};

use super::{field_row, node_id_select, section_header};

pub fn render(node: Arc<Node>) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(section_header("Instances Along Curve"))
        .child(field_row("Curve node", node_id_select(
            node.clone(),
            |k| matches!(k, NodeKind::Curve(_)),
            |k| match k {
                NodeKind::InstancesAlongCurve(def) => Some(def.curve_node),
                _ => None,
            },
            |k, new_id| {
                if let NodeKind::InstancesAlongCurve(def) = k {
                    def.curve_node = new_id;
                }
            },
        )))
        .child(field_row("Source node", node_id_select(
            node.clone(),
            |k| matches!(k, NodeKind::Primitive { .. }),
            |k| match k {
                NodeKind::InstancesAlongCurve(def) => Some(def.source_node),
                _ => None,
            },
            |k, new_id| {
                if let NodeKind::InstancesAlongCurve(def) = k {
                    def.source_node = new_id;
                }
            },
        )))
        .child(field_row("Spacing", instances_f32_input(node.clone(), InstancesField::Spacing)))
        .child(field_row("Side offset", instances_f32_input(node.clone(), InstancesField::SideOffset)))
        .child(field_row("Orient", instances_bool_input(node)))
    })
}

#[derive(Clone, Copy)]
enum InstancesField {
    Spacing,
    SideOffset,
}

fn instances_f32_input(node: Arc<Node>, field: InstancesField) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| match k {
        NodeKind::InstancesAlongCurve(def) => match field {
            InstancesField::Spacing => def.spacing,
            InstancesField::SideOffset => def.side_offset,
        },
        _ => 0.0,
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::InstancesAlongCurve(ref mut def) = k {
            match field {
                InstancesField::Spacing => def.spacing = new_value.max(0.05),
                InstancesField::SideOffset => def.side_offset = new_value,
            }
            kind.set(k);
        }
    })
}

fn instances_bool_input(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "checkbox")
        .style("cursor", "pointer")
        .with_node!(input => {
            .future(clone!(kind, input => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::InstancesAlongCurve(def) = k {
                        if input.checked() != def.orient_to_tangent {
                            input.set_checked(def.orient_to_tangent);
                        }
                    }
                    async {}
                })
            }))
            .event(clone!(kind, input => move |_: events::Change| {
                let mut k = kind.get_cloned();
                if let NodeKind::InstancesAlongCurve(ref mut def) = k {
                    def.orient_to_tangent = input.checked();
                    kind.set(k);
                }
            }))
        })
    })
}
