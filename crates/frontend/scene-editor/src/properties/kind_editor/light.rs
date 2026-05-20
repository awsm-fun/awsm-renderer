use crate::prelude::*;
use crate::properties::transform::{format_number, number_input};
use crate::scene::{LightConfig, LightKind, Node, NodeKind};
use crate::state::app_state;

use super::dispatch::light_variant_tag;
use super::helpers::{field_row, section_header};
use super::light_shadow;

pub(super) fn render_light_editor(node: Arc<Node>) -> Dom {
    let header = match node.kind.get_cloned() {
        NodeKind::Light(LightConfig::Directional { .. }) => "Directional Light",
        NodeKind::Light(LightConfig::Point { .. }) => "Point Light",
        NodeKind::Light(LightConfig::Spot { .. }) => "Spot Light",
        _ => "Light",
    };

    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(section_header(header))
        .child(field_row("Color", render_color_input(node.clone())))
        .child(field_row("Intensity", light_scalar_input(node.clone(), LightField::Intensity)))
        // Same dedupe trick as the collision editor: the variant-specific
        // inputs are only rebuilt when the user actually flips
        // Directional / Point / Spot, not on every range / angle drag.
        .child_signal(node.kind.signal_ref(light_variant_tag).dedupe().map(clone!(node => move |variant| {
            match variant {
                Some(LightKind::Directional) => None,
                Some(LightKind::Point) => Some(html!("div", {
                    .child(field_row("Range", light_scalar_input(node.clone(), LightField::Range)))
                })),
                Some(LightKind::Spot) => Some(html!("div", {
                    .style("display", "flex")
                    .style("flex-direction", "column")
                    .style("gap", "0.5rem")
                    .child(field_row("Range", light_scalar_input(node.clone(), LightField::Range)))
                    .child(field_row("Inner angle (rad)", light_scalar_input(node.clone(), LightField::InnerAngle)))
                    .child(field_row("Outer angle (rad)", light_scalar_input(node.clone(), LightField::OuterAngle)))
                })),
                None => None,
            }
        })))
        // Shadow knobs sit below the variant-specific block so undoable
        // edits there don't tear down the rest of the inspector. The
        // panel itself dedupes internally on the variant tag for its
        // directional-only / point-only sub-blocks.
        .child(light_shadow::render(node.clone()))
    })
}

#[derive(Clone, Copy)]
enum LightField {
    Intensity,
    Range,
    InnerAngle,
    OuterAngle,
}

fn light_scalar_input(node: Arc<Node>, field: LightField) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| match (field, &k) {
        (LightField::Intensity, NodeKind::Light(LightConfig::Directional { intensity, .. })) => {
            *intensity
        }
        (LightField::Intensity, NodeKind::Light(LightConfig::Point { intensity, .. })) => {
            *intensity
        }
        (LightField::Intensity, NodeKind::Light(LightConfig::Spot { intensity, .. })) => *intensity,
        (LightField::Range, NodeKind::Light(LightConfig::Point { range, .. })) => *range,
        (LightField::Range, NodeKind::Light(LightConfig::Spot { range, .. })) => *range,
        (LightField::InnerAngle, NodeKind::Light(LightConfig::Spot { inner_angle, .. })) => {
            *inner_angle
        }
        (LightField::OuterAngle, NodeKind::Light(LightConfig::Spot { outer_angle, .. })) => {
            *outer_angle
        }
        _ => 0.0,
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::Light(ref mut cfg) = k {
            apply_light_field(cfg, field, new_value);
            kind.set(k);
        }
    })
}

fn apply_light_field(cfg: &mut LightConfig, field: LightField, v: f32) {
    match (field, cfg) {
        (LightField::Intensity, LightConfig::Directional { intensity, .. })
        | (LightField::Intensity, LightConfig::Point { intensity, .. })
        | (LightField::Intensity, LightConfig::Spot { intensity, .. }) => *intensity = v,
        (LightField::Range, LightConfig::Point { range, .. })
        | (LightField::Range, LightConfig::Spot { range, .. }) => *range = v,
        (LightField::InnerAngle, LightConfig::Spot { inner_angle, .. }) => *inner_angle = v,
        (LightField::OuterAngle, LightConfig::Spot { outer_angle, .. }) => *outer_angle = v,
        _ => {}
    }
}

fn render_color_input(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();

    let input = html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "color")
        .style("width", "3rem")
        .style("height", "1.8rem")
        .style("border", "0")
        .style("background", "transparent")
        .with_node!(input => {
            .future(clone!(kind, input => {
                kind.signal_cloned().for_each(move |k| {
                    if let NodeKind::Light(cfg) = k {
                        let c = light_color(&cfg);
                        input.set_value(&hex_from_rgb(c));
                    }
                    async {}
                })
            }))
            .event(clone!(kind, input => move |_: events::Change| {
                let hex = input.value();
                if let Some(rgb) = rgb_from_hex(&hex) {
                    let state = app_state();
                    let previous = state.snapshot_scene();
                    let mut k = kind.get_cloned();
                    if let NodeKind::Light(ref mut cfg) = k {
                        set_light_color(cfg, rgb);
                        kind.set(k);
                        state.scene.bump_revision();
                        state.commit_history(previous);
                    }
                }
            }))
        })
    });

    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "0.5rem")
        .child(input)
        .child(html!("span", {
            .style("font-size", "0.75rem")
            .style("font-family", "monospace")
            .style("color", ColorText::Byline.value())
            .text_signal(kind.signal_cloned().map(|k| match k {
                NodeKind::Light(cfg) => {
                    let c = light_color(&cfg);
                    format!("{} {} {}", format_number(c[0]), format_number(c[1]), format_number(c[2]))
                }
                _ => String::new(),
            }))
        }))
    })
}

fn light_color(cfg: &LightConfig) -> [f32; 3] {
    match cfg {
        LightConfig::Directional { color, .. }
        | LightConfig::Point { color, .. }
        | LightConfig::Spot { color, .. } => *color,
    }
}

fn set_light_color(cfg: &mut LightConfig, rgb: [f32; 3]) {
    match cfg {
        LightConfig::Directional { color, .. }
        | LightConfig::Point { color, .. }
        | LightConfig::Spot { color, .. } => *color = rgb,
    }
}

fn hex_from_rgb(rgb: [f32; 3]) -> String {
    let to_byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02X}{:02X}{:02X}",
        to_byte(rgb[0]),
        to_byte(rgb[1]),
        to_byte(rgb[2]),
    )
}

fn rgb_from_hex(hex: &str) -> Option<[f32; 3]> {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0])
}
