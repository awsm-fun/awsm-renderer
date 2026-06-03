// ─────────────────────────────────────────────────────────────────────
// Sprite editor
// ─────────────────────────────────────────────────────────────────────

use crate::prelude::*;
use crate::properties::transform::number_input;
use crate::scene::{Node, NodeKind};
use awsm_scene_schema::{BillboardMode, SpriteAlphaMode};

use super::{field_row, section_header, texture_ref_select};

const BB_VALUE_NONE: &str = "none";
const BB_VALUE_Y: &str = "y";
const BB_VALUE_FULL: &str = "full";
const ALPHA_VALUE_OPAQUE: &str = "opaque";
const ALPHA_VALUE_MASK: &str = "mask";
const ALPHA_VALUE_BLEND: &str = "blend";

pub fn render(node: Arc<Node>) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(section_header("Sprite"))
        .child(field_row("Width", sprite_size_input(node.clone(), 0)))
        .child(field_row("Height", sprite_size_input(node.clone(), 1)))
        .child(field_row("Billboard", sprite_billboard_select(node.clone())))
        .child(field_row("Alpha mode", sprite_alpha_select(node.clone())))
        .child(field_row("Texture", texture_ref_select(
            node.clone(),
            |k| match k {
                NodeKind::Sprite(s) => s.texture,
                _ => None,
            },
            |k, new_ref| {
                if let NodeKind::Sprite(s) = k {
                    s.texture = new_ref;
                }
            },
        )))
    })
}

fn sprite_size_input(node: Arc<Node>, axis: usize) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| match k {
        NodeKind::Sprite(s) => s.size[axis],
        _ => 1.0,
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::Sprite(ref mut s) = k {
            s.size[axis] = new_value.max(0.001);
            kind.set(k);
        }
    })
}

fn sprite_billboard_select(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .child(html!("option", { .attr("value", BB_VALUE_NONE).text("None") }))
        .child(html!("option", { .attr("value", BB_VALUE_Y).text("Y-axis") }))
        .child(html!("option", { .attr("value", BB_VALUE_FULL).text("Full") }))
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::Sprite(s) = k {
                        let want = match s.billboard {
                            BillboardMode::None => BB_VALUE_NONE,
                            BillboardMode::YAxis => BB_VALUE_Y,
                            BillboardMode::Full => BB_VALUE_FULL,
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
                if let NodeKind::Sprite(ref mut s) = k {
                    s.billboard = match select.value().as_str() {
                        BB_VALUE_NONE => BillboardMode::None,
                        BB_VALUE_Y => BillboardMode::YAxis,
                        _ => BillboardMode::Full,
                    };
                    kind.set(k);
                }
            }))
        })
    })
}

fn sprite_alpha_select(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .child(html!("option", { .attr("value", ALPHA_VALUE_OPAQUE).text("Opaque") }))
        .child(html!("option", { .attr("value", ALPHA_VALUE_MASK).text("Mask") }))
        .child(html!("option", { .attr("value", ALPHA_VALUE_BLEND).text("Blend") }))
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::Sprite(s) = k {
                        let want = match s.alpha_mode {
                            SpriteAlphaMode::Opaque => ALPHA_VALUE_OPAQUE,
                            SpriteAlphaMode::Mask { .. } => ALPHA_VALUE_MASK,
                            SpriteAlphaMode::Blend => ALPHA_VALUE_BLEND,
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
                if let NodeKind::Sprite(ref mut s) = k {
                    let new_mode = match select.value().as_str() {
                        ALPHA_VALUE_OPAQUE => SpriteAlphaMode::Opaque,
                        ALPHA_VALUE_MASK => SpriteAlphaMode::Mask { cutoff_x1000: 500 },
                        _ => SpriteAlphaMode::Blend,
                    };
                    let same_variant = std::mem::discriminant(&s.alpha_mode)
                        == std::mem::discriminant(&new_mode);
                    if !same_variant {
                        s.alpha_mode = new_mode;
                        kind.set(k);
                    }
                }
            }))
        })
    })
}
