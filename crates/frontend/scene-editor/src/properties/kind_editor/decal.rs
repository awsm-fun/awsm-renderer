// ─────────────────────────────────────────────────────────────────────
// Decal editor — Cluster 6.4 / plan §16.4.B.
// ─────────────────────────────────────────────────────────────────────

use crate::prelude::*;
use crate::properties::transform::number_input;
use crate::scene::{Node, NodeKind};
use awsm_scene_schema::DecalBlendMode;

use super::{field_row, section_header, texture_ref_select};

const BLEND_VALUE_ALPHA: &str = "alpha_blend";

pub fn render(node: Arc<Node>) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(section_header("Decal"))
        .child(field_row("Texture", texture_ref_select(
            node.clone(),
            |k| match k {
                NodeKind::Decal(d) => d.texture,
                _ => None,
            },
            |k, new_ref| {
                if let NodeKind::Decal(d) = k {
                    d.texture = new_ref;
                }
            },
        )))
        .child(field_row("Alpha", decal_alpha_input(node.clone())))
        .child(field_row("Blend", decal_blend_select(node.clone())))
    })
}

fn decal_alpha_input(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| match k {
        NodeKind::Decal(d) => d.alpha,
        _ => 1.0,
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::Decal(ref mut d) = k {
            d.alpha = new_value.clamp(0.0, 1.0);
            kind.set(k);
        }
    })
}

fn decal_blend_select(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        // v1: only Alpha Blend. Additive / Multiply reserved on the
        // schema enum; add options here once the runtime supports them.
        .child(html!("option", { .attr("value", BLEND_VALUE_ALPHA).text("Alpha Blend") }))
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::Decal(d) = k {
                        let want = match d.blend_mode {
                            DecalBlendMode::AlphaBlend => BLEND_VALUE_ALPHA,
                        };
                        if select.value() != want {
                            select.set_value(want);
                        }
                    }
                    async {}
                })
            }))
            .event(clone!(kind, select => move |_: events::Change| {
                let mut k = kind.get_cloned();
                if let NodeKind::Decal(ref mut d) = k {
                    d.blend_mode = match select.value().as_str() {
                        _ => DecalBlendMode::AlphaBlend,
                    };
                    kind.set(k);
                }
            }))
        })
    })
}
