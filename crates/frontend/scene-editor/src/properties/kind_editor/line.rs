// ─────────────────────────────────────────────────────────────────────
// Line editor
// ─────────────────────────────────────────────────────────────────────

use crate::prelude::*;
use crate::properties::transform::number_input;
use crate::scene::{Node, NodeKind};

use super::{field_row, section_header};

pub fn render(node: Arc<Node>) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(section_header("Line"))
        .child(field_row("Width (px)", line_width_input(node.clone())))
        .child(field_row("Depth always", line_depth_always_input(node.clone())))
        .child(field_row("Points", html!("div", {
            .style("font-size", "0.8rem")
            .style("color", ColorText::Byline.value())
            .text_signal(node.kind.signal_cloned().map(|k| match k {
                NodeKind::Line(l) => format!("{} vertices (edit via project.json)", l.points.len()),
                _ => String::new(),
            }))
        })))
    })
}

fn line_width_input(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(|k| match k {
        NodeKind::Line(l) => l.width_px,
        _ => 2.5,
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::Line(ref mut l) = k {
            l.width_px = new_value.max(0.5);
            kind.set(k);
        }
    })
}

fn line_depth_always_input(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "checkbox")
        .style("cursor", "pointer")
        .with_node!(input => {
            .future(clone!(kind, input => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::Line(l) = k {
                        if input.checked() != l.depth_test_always {
                            input.set_checked(l.depth_test_always);
                        }
                    }
                    async {}
                })
            }))
            .event(clone!(kind, input => move |_: events::Change| {
                let mut k = kind.get_cloned();
                if let NodeKind::Line(ref mut l) = k {
                    l.depth_test_always = input.checked();
                    kind.set(k);
                }
            }))
        })
    })
}
