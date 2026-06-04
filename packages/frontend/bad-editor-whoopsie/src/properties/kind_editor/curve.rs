// ─────────────────────────────────────────────────────────────────────
// Curve editor
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
        .child(section_header("Curve"))
        .child(field_row("Closed", curve_closed_input(node.clone())))
        .child(field_row("Tension", curve_tension_input(node.clone())))
        .child(field_row("Samples", curve_samples_input(node.clone())))
        .child(field_row("Points", html!("div", {
            .style("font-size", "0.8rem")
            .style("color", ColorText::Byline.value())
            .text_signal(node.kind.signal_cloned().map(|k| match k {
                NodeKind::Curve(c) => format!("{} control points (edit via project.json)", c.control_points.len()),
                _ => String::new(),
            }))
        })))
    })
}

fn curve_closed_input(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "checkbox")
        .style("cursor", "pointer")
        .future(clone!(kind => {
            let kind = kind.clone();
            kind.signal_cloned().for_each(move |_| async {})
        }))
        .with_node!(input => {
            .future(clone!(kind, input => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::Curve(c) = k {
                        if input.checked() != c.closed {
                            input.set_checked(c.closed);
                        }
                    }
                    async {}
                })
            }))
            .event(clone!(kind, input => move |_: events::Change| {
                let mut k = kind.get_cloned();
                if let NodeKind::Curve(ref mut c) = k {
                    c.closed = input.checked();
                    kind.set(k);
                }
            }))
        })
    })
}

fn curve_tension_input(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(|k| match k {
        NodeKind::Curve(c) => c.tension,
        _ => 0.5,
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::Curve(ref mut c) = k {
            c.tension = new_value.clamp(0.0, 2.0);
            kind.set(k);
        }
    })
}

fn curve_samples_input(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(|k| match k {
        NodeKind::Curve(c) => c.sample_count as f32,
        _ => 64.0,
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::Curve(ref mut c) = k {
            c.sample_count = (new_value.max(2.0) as u32).clamp(2, 4096);
            kind.set(k);
        }
    })
}
