//! Kind-specific inspector. For single-selection only.
//!
//! Variant switching (Point ↔ Spot, Box ↔ Sphere) is deferred — it's
//! editable polish. Today you delete + re-insert with the desired variant.

pub mod camera;
pub mod curve;
pub mod instances;
pub mod line;
pub mod material;
pub mod mesh;
pub mod particle;
pub mod primitive;
pub mod sprite;
pub mod sweep;

use crate::prelude::*;
use crate::properties::transform::{format_number, labeled_axis_input, number_input};
use crate::scene::{AssetId, AssetSource, ColliderShape, LightConfig, LightKind, Node, NodeKind};
use crate::state::app_state;
use awsm_scene_schema::{NodeId, TextureDef, TextureRef};

/// Top-level discriminant for `NodeKind`. The kind-editor's outer
/// `child_signal` dedupes on this so a value change inside the same
/// kind (e.g. radius tweak inside a Sphere) doesn't tear the inspector
/// down and rebuild it from scratch — which would detach inputs
/// mid-drag, mid-typing, etc.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NodeKindTag {
    Group,
    Model,
    Light,
    Collider,
    Camera,
    Primitive,
    Mesh,
    Curve,
    Sweep,
    Instances,
    Line,
    Sprite,
    Particle,
}

fn node_kind_tag(k: &NodeKind) -> NodeKindTag {
    match k {
        NodeKind::Group => NodeKindTag::Group,
        NodeKind::Model(_) => NodeKindTag::Model,
        NodeKind::Light(_) => NodeKindTag::Light,
        NodeKind::Collider(_) => NodeKindTag::Collider,
        NodeKind::Camera(_) => NodeKindTag::Camera,
        NodeKind::Primitive { .. } => NodeKindTag::Primitive,
        NodeKind::Mesh { .. } => NodeKindTag::Mesh,
        NodeKind::Curve(_) => NodeKindTag::Curve,
        NodeKind::SweepAlongCurve { .. } => NodeKindTag::Sweep,
        NodeKind::InstancesAlongCurve(_) => NodeKindTag::Instances,
        NodeKind::Line(_) => NodeKindTag::Line,
        NodeKind::Sprite(_) => NodeKindTag::Sprite,
        NodeKind::ParticleEmitter(_) => NodeKindTag::Particle,
    }
}

/// Variant tag for `ColliderShape`. Used by the collision-editor's
/// `child_signal` so a *value* change inside the same shape variant
/// (e.g. dragging the radius) does NOT trigger a rebuild of the input
/// element — that would detach the input mid-drag and silently break
/// pointer capture. Only an actual variant flip (Box ↔ Sphere) needs
/// the input swap.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColliderVariantTag {
    Box,
    Sphere,
    Capsule,
    Cylinder,
    Cone,
    Ellipsoid,
}

fn collider_variant_tag(k: &NodeKind) -> Option<ColliderVariantTag> {
    match k {
        NodeKind::Collider(ColliderShape::Box { .. }) => Some(ColliderVariantTag::Box),
        NodeKind::Collider(ColliderShape::Sphere { .. }) => Some(ColliderVariantTag::Sphere),
        NodeKind::Collider(ColliderShape::Capsule { .. }) => Some(ColliderVariantTag::Capsule),
        NodeKind::Collider(ColliderShape::Cylinder { .. }) => Some(ColliderVariantTag::Cylinder),
        NodeKind::Collider(ColliderShape::Cone { .. }) => Some(ColliderVariantTag::Cone),
        NodeKind::Collider(ColliderShape::Ellipsoid { .. }) => Some(ColliderVariantTag::Ellipsoid),
        _ => None,
    }
}

/// Same idea as `collider_variant_tag` but for lights — only Point /
/// Spot have variant-specific fields, and Directional has none. Using
/// `LightConfig::kind()` (already `Eq + Copy`) plus a dedupe stops the
/// Range / Inner-angle / Outer-angle inputs from being rebuilt on
/// every drag-step.
fn light_variant_tag(k: &NodeKind) -> Option<LightKind> {
    match k {
        NodeKind::Light(cfg) => Some(cfg.kind()),
        _ => None,
    }
}

pub fn render(node: Arc<Node>) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .style("padding", "0.5rem 0")
        // Dedupe on the outer kind tag so the inspector subtree only
        // gets rebuilt when the user genuinely changes the node's kind
        // (Group ↔ Model ↔ Light ↔ Collision). Without this, every
        // value tweak that flows through `kind.set` (radius drag, light
        // intensity drag, etc.) would tear down the entire inspector
        // and the input element with it — which is what was killing
        // pointer drag *and* mid-typing of decimals like "7.5".
        .child_signal(node.kind.signal_ref(node_kind_tag).dedupe().map(clone!(node => move |tag| {
            Some(match tag {
                NodeKindTag::Group => render_group_stub(),
                NodeKindTag::Model => render_model_editor(node.clone()),
                NodeKindTag::Light => render_light_editor(node.clone()),
                NodeKindTag::Collider => render_collider_editor(node.clone()),
                NodeKindTag::Camera => camera::render(node.clone()),
                NodeKindTag::Primitive => primitive::render(node.clone()),
                NodeKindTag::Mesh => mesh::render(node.clone()),
                NodeKindTag::Curve => curve::render(node.clone()),
                NodeKindTag::Sweep => sweep::render(node.clone()),
                NodeKindTag::Instances => instances::render(node.clone()),
                NodeKindTag::Line => line::render(node.clone()),
                NodeKindTag::Sprite => sprite::render(node.clone()),
                NodeKindTag::Particle => particle::render(node.clone()),
            })
        })))
    })
}

/// Section header label. Accepts both `&'static str` and runtime-built
/// strings (e.g. a label with an asset id baked in) — both inputs route
/// through `AsRef<str>` so callers don't pay anything beyond a borrow.
pub(super) fn section_header(label: impl AsRef<str>) -> Dom {
    html!("div", {
        .style("font-size", "0.75rem")
        .style("font-weight", "600")
        .style("text-transform", "uppercase")
        .style("letter-spacing", "0.05em")
        .style("color", ColorText::Byline.value())
        .text(label.as_ref())
    })
}

/// "Capture as Mesh asset" button for procedural-mesh kinds (F10).
/// Calls `actions::object::capture_as_mesh_asset` on the node — that
/// action handles every part of the capture (asset insert, bytes into
/// pending + mesh_cache, kind rewrite, history commit).
pub(super) fn capture_as_mesh_button(node: Arc<Node>) -> Dom {
    use awsm_web_shared::atoms::buttons::{Button, ButtonSize, ButtonStyle};
    let node_id = node.id;
    html!("div", {
        .style("display", "flex")
        .style("justify-content", "flex-start")
        .style("padding-top", "0.25rem")
        .child(Button::new()
            .with_text("Capture as Mesh asset")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_on_click(move || {
                let _ = crate::actions::object::capture_as_mesh_asset(node_id);
            })
            .render())
    })
}

pub(super) fn field_row(label: &'static str, control: Dom) -> Dom {
    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "7rem 1fr")
        .style("gap", "0.5rem")
        .style("align-items", "center")
        .child(html!("span", {
            .style("font-size", "0.8rem")
            .style("color", ColorText::Byline.value())
            .text(label)
        }))
        .child(control)
    })
}

fn render_group_stub() -> Dom {
    html!("div", {
        .style("font-size", "0.8rem")
        .style("color", ColorText::Byline.value())
        .text("Group — no additional properties.")
    })
}

// All NodeKind variants now have their own `render_*_editor`; the
// generic-stub helper was retired alongside the last placeholder.

fn render_model_editor(node: Arc<Node>) -> Dom {
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
                    .filename(r.asset_id)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("<missing: {}>", r.asset_id)),
                _ => String::new(),
            }))
        })))
        .child(field_row("Node index", model_node_index_input(node)))
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

fn render_light_editor(node: Arc<Node>) -> Dom {
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

fn render_collider_editor(node: Arc<Node>) -> Dom {
    let header = match node.kind.get_cloned() {
        NodeKind::Collider(ColliderShape::Box { .. }) => "Collider Box",
        NodeKind::Collider(ColliderShape::Sphere { .. }) => "Collider Sphere",
        NodeKind::Collider(ColliderShape::Capsule { .. }) => "Collider Capsule",
        NodeKind::Collider(ColliderShape::Cylinder { .. }) => "Collider Cylinder",
        NodeKind::Collider(ColliderShape::Cone { .. }) => "Collider Cone",
        NodeKind::Collider(ColliderShape::Ellipsoid { .. }) => "Collider Ellipsoid",
        _ => "Collision",
    };

    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "0.5rem")
        .child(section_header(header))
        // Dedupe on the variant tag so the input subtree is only rebuilt
        // on shape-variant flips, not on every value tweak inside the
        // same shape — otherwise the input element gets detached
        // mid-drag.
        .child_signal(node.kind.signal_ref(collider_variant_tag).dedupe().map(clone!(node => move |variant| {
            match variant {
                Some(ColliderVariantTag::Box) => Some(collision_box_fields(node.clone())),
                Some(ColliderVariantTag::Sphere) => Some(collision_sphere_fields(node.clone())),
                Some(ColliderVariantTag::Capsule) => Some(collision_capsule_fields(node.clone())),
                Some(ColliderVariantTag::Cylinder) => Some(collision_cylinder_fields(node.clone())),
                Some(ColliderVariantTag::Cone) => Some(collision_cone_fields(node.clone())),
                Some(ColliderVariantTag::Ellipsoid) => Some(collision_ellipsoid_fields(node.clone())),
                None => None,
            }
        })))
    })
}

fn collision_box_fields(node: Arc<Node>) -> Dom {
    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "7rem 1fr 1fr 1fr")
        .style("gap", "0.5rem")
        .style("align-items", "center")
        .child(html!("span", {
            .style("font-size", "0.8rem")
            .style("color", ColorText::Byline.value())
            .text("Half extents")
        }))
        .child(half_extent_input(node.clone(), 0))
        .child(half_extent_input(node.clone(), 1))
        .child(half_extent_input(node, 2))
    })
}

fn half_extent_input(node: Arc<Node>, component: usize) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| match k {
        NodeKind::Collider(ColliderShape::Box { half_extents }) => half_extents[component],
        _ => 0.0,
    });
    labeled_axis_input(component, value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::Collider(ColliderShape::Box {
            ref mut half_extents,
        }) = k
        {
            half_extents[component] = new_value.max(0.001);
            kind.set(k);
        }
    })
}

fn collision_sphere_fields(node: Arc<Node>) -> Dom {
    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "7rem 1fr")
        .style("gap", "0.5rem")
        .style("align-items", "center")
        .child(html!("span", {
            .style("font-size", "0.8rem")
            .style("color", ColorText::Byline.value())
            .text("Radius")
        }))
        .child(sphere_radius_input(node))
    })
}

fn sphere_radius_input(node: Arc<Node>) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(|k| match k {
        NodeKind::Collider(ColliderShape::Sphere { radius }) => radius,
        _ => 0.0,
    });
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::Collider(ColliderShape::Sphere { ref mut radius }) = k {
            *radius = new_value.max(0.001);
            kind.set(k);
        }
    })
}

fn collision_capsule_fields(node: Arc<Node>) -> Dom {
    cylindrical_fields(
        node,
        |k| match k {
            NodeKind::Collider(ColliderShape::Capsule {
                half_height,
                radius,
            }) => Some((*half_height, *radius)),
            _ => None,
        },
        |k, half_height, radius| {
            if let NodeKind::Collider(ColliderShape::Capsule {
                half_height: ref mut hh,
                radius: ref mut r,
            }) = k
            {
                *hh = half_height;
                *r = radius;
            }
        },
    )
}

fn collision_cylinder_fields(node: Arc<Node>) -> Dom {
    cylindrical_fields(
        node,
        |k| match k {
            NodeKind::Collider(ColliderShape::Cylinder {
                half_height,
                radius,
            }) => Some((*half_height, *radius)),
            _ => None,
        },
        |k, half_height, radius| {
            if let NodeKind::Collider(ColliderShape::Cylinder {
                half_height: ref mut hh,
                radius: ref mut r,
            }) = k
            {
                *hh = half_height;
                *r = radius;
            }
        },
    )
}

fn collision_cone_fields(node: Arc<Node>) -> Dom {
    cylindrical_fields(
        node,
        |k| match k {
            NodeKind::Collider(ColliderShape::Cone {
                half_height,
                radius,
            }) => Some((*half_height, *radius)),
            _ => None,
        },
        |k, half_height, radius| {
            if let NodeKind::Collider(ColliderShape::Cone {
                half_height: ref mut hh,
                radius: ref mut r,
            }) = k
            {
                *hh = half_height;
                *r = radius;
            }
        },
    )
}

fn collision_ellipsoid_fields(node: Arc<Node>) -> Dom {
    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "7rem 1fr 1fr 1fr")
        .style("gap", "0.5rem")
        .style("align-items", "center")
        .child(html!("span", {
            .style("font-size", "0.8rem")
            .style("color", ColorText::Byline.value())
            .text("Half extents")
        }))
        .child(ellipsoid_extent_input(node.clone(), 0))
        .child(ellipsoid_extent_input(node.clone(), 1))
        .child(ellipsoid_extent_input(node, 2))
    })
}

fn ellipsoid_extent_input(node: Arc<Node>, component: usize) -> Dom {
    let kind = node.kind.clone();
    let value_signal = kind.signal_cloned().map(move |k| match k {
        NodeKind::Collider(ColliderShape::Ellipsoid { half_extents }) => half_extents[component],
        _ => 0.0,
    });
    labeled_axis_input(component, value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let NodeKind::Collider(ColliderShape::Ellipsoid {
            ref mut half_extents,
        }) = k
        {
            half_extents[component] = new_value.max(0.001);
            kind.set(k);
        }
    })
}

/// Shared input shape for cylindrical primitives (Capsule, Cylinder,
/// Cone) — all three expose the same `half_height` + `radius` fields;
/// only the variant the read/write closures touch differs.
fn cylindrical_fields(
    node: Arc<Node>,
    read: impl Fn(&NodeKind) -> Option<(f32, f32)> + Clone + 'static,
    write: impl Fn(&mut NodeKind, f32, f32) + Clone + 'static,
) -> Dom {
    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "7rem 1fr")
        .style("gap", "0.5rem")
        .style("align-items", "center")
        .child(html!("span", {
            .style("font-size", "0.8rem")
            .style("color", ColorText::Byline.value())
            .text("Half height")
        }))
        .child(half_height_input(node.clone(), read.clone(), write.clone()))
        .child(html!("span", {
            .style("font-size", "0.8rem")
            .style("color", ColorText::Byline.value())
            .text("Radius")
        }))
        .child(cap_radius_input(node, read, write))
    })
}

fn half_height_input(
    node: Arc<Node>,
    read: impl Fn(&NodeKind) -> Option<(f32, f32)> + Clone + 'static,
    write: impl Fn(&mut NodeKind, f32, f32) + Clone + 'static,
) -> Dom {
    let kind = node.kind.clone();
    let read_clone = read.clone();
    let value_signal = kind
        .signal_cloned()
        .map(move |k| read_clone(&k).map(|(hh, _)| hh).unwrap_or(0.0));
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let Some((_, r)) = read(&k) {
            write(&mut k, new_value.max(0.001), r);
            kind.set(k);
        }
    })
}

fn cap_radius_input(
    node: Arc<Node>,
    read: impl Fn(&NodeKind) -> Option<(f32, f32)> + Clone + 'static,
    write: impl Fn(&mut NodeKind, f32, f32) + Clone + 'static,
) -> Dom {
    let kind = node.kind.clone();
    let read_clone = read.clone();
    let value_signal = kind
        .signal_cloned()
        .map(move |k| read_clone(&k).map(|(_, r)| r).unwrap_or(0.0));
    number_input(value_signal, move |new_value| {
        let mut k = kind.get_cloned();
        if let Some((hh, _)) = read(&k) {
            write(&mut k, hh, new_value.max(0.001));
            kind.set(k);
        }
    })
}

// ─────────────────────────────────────────────────────────────────────
// NodeId picker (snapshot-based; rebuilds when the host kind changes)
// ─────────────────────────────────────────────────────────────────────

/// Collect `(NodeId, display_name)` pairs from the live scene tree that
/// satisfy the given `predicate` on each node's current `NodeKind`. The
/// caller renders a `<select>` from the result — reactive updates when
/// the host kind signal changes are sufficient for v1 (adding a new
/// curve node and immediately reassigning a sweep's `curve_node` will
/// require switching the sweep node first to re-snapshot).
pub(super) fn collect_nodes_matching<F>(predicate: F) -> Vec<(NodeId, String)>
where
    F: Fn(&NodeKind) -> bool,
{
    fn walk<F>(nodes: &[Arc<crate::scene::Node>], predicate: &F, out: &mut Vec<(NodeId, String)>)
    where
        F: Fn(&NodeKind) -> bool,
    {
        for n in nodes.iter() {
            if predicate(&n.kind.lock_ref()) {
                out.push((n.id, n.name.get_cloned()));
            }
            let children = n.children.lock_ref();
            walk(&children, predicate, out);
        }
    }
    let scene = app_state().scene.clone();
    let nodes = scene.nodes.lock_ref();
    let mut out = Vec::new();
    walk(&nodes, &predicate, &mut out);
    out
}

/// A `<select>` whose options are every node currently in the scene tree
/// whose kind satisfies `predicate`. `read_current` extracts the
/// currently-selected `NodeId` from the host kind; `write_new` mutates
/// the host kind in place with the picked `NodeId`.
pub(super) fn node_id_select(
    node: Arc<Node>,
    predicate: fn(&NodeKind) -> bool,
    read_current: fn(&NodeKind) -> Option<NodeId>,
    write_new: fn(&mut NodeKind, NodeId),
) -> Dom {
    let kind = node.kind.clone();
    let candidates = collect_nodes_matching(predicate);
    let mut options = vec![html!("option", {
        .attr("value", "")
        .text("(none)")
    })];
    for (id, name) in &candidates {
        options.push(html!("option", {
            .attr("value", &id.0.to_string())
            .text(name)
        }));
    }

    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .children(options)
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    let want = read_current(&k)
                        .map(|id| id.0.to_string())
                        .unwrap_or_default();
                    if select.value() != want {
                        select.set_value(&want);
                    }
                    async {}
                })
            }))
            .event(clone!(kind, select => move |_: events::Change| {
                let value = select.value();
                if value.is_empty() {
                    return;
                }
                let Ok(parsed) = uuid::Uuid::parse_str(&value) else {
                    return;
                };
                let new_id = NodeId(parsed);
                let mut k = kind.get_cloned();
                write_new(&mut k, new_id);
                kind.set(k);
            }))
        })
    })
}

/// Collects every `AssetSource::Texture` entry in the live asset table,
/// keyed by `AssetId`, paired with a human-readable label derived from
/// the texture's `TextureDef` variant.
pub(super) fn collect_textures() -> Vec<(AssetId, String)> {
    use awsm_scene_schema::ProceduralTextureDef;
    let scene = app_state().scene.clone();
    let assets = scene.assets.lock().unwrap();
    let mut out: Vec<(AssetId, String)> = Vec::new();
    for (id, entry) in assets.entries.iter() {
        if let AssetSource::Texture(def) = &entry.source {
            let label = match def {
                TextureDef::Raster { filename } => filename.clone(),
                TextureDef::Procedural(ProceduralTextureDef::Checker { .. }) => {
                    "Procedural: Checker".to_string()
                }
                TextureDef::Procedural(ProceduralTextureDef::Gradient { .. }) => {
                    "Procedural: Gradient".to_string()
                }
                TextureDef::Procedural(ProceduralTextureDef::Noise { .. }) => {
                    "Procedural: Noise".to_string()
                }
            };
            out.push((*id, label));
        }
    }
    // Stable order — sort by label so the dropdown is predictable across
    // repaints. AssetId is a UUID and not naturally ordered.
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// A `<select>` whose options are every `AssetSource::Texture` entry in the
/// scene's asset table. `read_current` returns the host kind's currently-
/// referenced texture (if any); `write_new` mutates the host kind in place
/// with the picked (or cleared) reference.
///
/// Mirrors `node_id_select`'s shape so Sprite + Particle inspectors can wire
/// per-`TextureRef` slots without duplicating the dropdown machinery.
pub(super) fn texture_ref_select(
    node: Arc<Node>,
    read_current: fn(&NodeKind) -> Option<TextureRef>,
    write_new: fn(&mut NodeKind, Option<TextureRef>),
) -> Dom {
    // The option list snapshots `collect_textures()` at construction; if
    // we built the `<select>` once and held onto it, adding a new
    // procedural-texture asset wouldn't appear in the dropdown until the
    // inspector re-rendered for some other reason. Mirroring
    // `material_ref_select`, wrap the body in a revision-driven
    // `child_signal` so any `scene.bump_revision()` rebuilds the options.
    let revision = app_state().scene.revision.clone();
    html!("div", {
        .child_signal(revision.signal().map(clone!(node => move |_rev| {
            Some(texture_ref_select_inner(node.clone(), read_current, write_new))
        })))
    })
}

fn texture_ref_select_inner(
    node: Arc<Node>,
    read_current: fn(&NodeKind) -> Option<TextureRef>,
    write_new: fn(&mut NodeKind, Option<TextureRef>),
) -> Dom {
    let kind = node.kind.clone();
    let candidates = collect_textures();
    let mut options = vec![html!("option", {
        .attr("value", "")
        .text("(none)")
    })];
    for (id, label) in &candidates {
        options.push(html!("option", {
            .attr("value", &id.0.to_string())
            .text(label)
        }));
    }

    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .children(options)
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    let want = read_current(&k)
                        .map(|r| r.0.0.to_string())
                        .unwrap_or_default();
                    if select.value() != want {
                        select.set_value(&want);
                    }
                    async {}
                })
            }))
            .event(clone!(kind, select => move |_: events::Change| {
                let value = select.value();
                let new_ref = if value.is_empty() {
                    None
                } else {
                    let Ok(parsed) = uuid::Uuid::parse_str(&value) else {
                        return;
                    };
                    Some(TextureRef(AssetId(parsed)))
                };
                let mut k = kind.get_cloned();
                write_new(&mut k, new_ref);
                kind.set(k);
            }))
        })
    })
}

/// Collects every `AssetSource::Material(MaterialDef)` entry in the
/// live asset table. Uses the authored `MaterialDef.label` when set;
/// falls back to the short UUID prefix otherwise.
pub(super) fn collect_materials() -> Vec<(AssetId, String)> {
    let scene = app_state().scene.clone();
    let assets = scene.assets.lock().unwrap();
    let mut out: Vec<(AssetId, String)> = Vec::new();
    for (id, entry) in assets.entries.iter() {
        if let AssetSource::Material(def) = &entry.source {
            let label = if def.label.is_empty() {
                format!("Material {}", &id.0.to_string()[..8])
            } else {
                def.label.clone()
            };
            out.push((*id, label));
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// A `<select>` whose options are every `AssetSource::Material` entry
/// in the scene's asset table, plus `(inline material)` for `None`.
/// Identical shape to `texture_ref_select` so the Primitive / Sweep /
/// Mesh editors can wire `Option<MaterialRef>` slots uniformly.
///
/// The option list is recomputed whenever `scene.revision` ticks —
/// that's how a fresh `+ Material Asset` click shows up here without
/// reloading the inspector.
pub(super) fn material_ref_select(
    node: Arc<Node>,
    read_current: fn(&NodeKind) -> Option<awsm_scene_schema::MaterialRef>,
    write_new: fn(&mut NodeKind, Option<awsm_scene_schema::MaterialRef>),
) -> Dom {
    let revision = app_state().scene.revision.clone();
    html!("div", {
        .child_signal(revision.signal().map(clone!(node => move |_rev| {
            Some(material_ref_select_inner(node.clone(), read_current, write_new))
        })))
    })
}

fn material_ref_select_inner(
    node: Arc<Node>,
    read_current: fn(&NodeKind) -> Option<awsm_scene_schema::MaterialRef>,
    write_new: fn(&mut NodeKind, Option<awsm_scene_schema::MaterialRef>),
) -> Dom {
    use awsm_scene_schema::MaterialRef;
    let kind = node.kind.clone();
    let candidates = collect_materials();
    let mut options = vec![html!("option", {
        .attr("value", "")
        .text("(inline material)")
    })];
    for (id, label) in &candidates {
        options.push(html!("option", {
            .attr("value", &id.0.to_string())
            .text(label)
        }));
    }

    html!("select" => web_sys::HtmlSelectElement, {
        .style("padding", "0.35rem 0.5rem")
        .style("background-color", ColorRaw::Darkest.value())
        .style("color", ColorText::SidebarHeader.value())
        .style("border", &format!("1px solid {}", ColorBackground::UnderlineSecondary.value()))
        .style("border-radius", "0.3rem")
        .style("font-size", "0.85rem")
        .style("cursor", "pointer")
        .children(options)
        .with_node!(select => {
            .future(clone!(kind, select => {
                kind.signal_cloned().for_each(move |k| {
                    let want = read_current(&k)
                        .map(|r| r.0.0.to_string())
                        .unwrap_or_default();
                    if select.value() != want {
                        select.set_value(&want);
                    }
                    async {}
                })
            }))
            .event(clone!(kind, select => move |_: events::Change| {
                let value = select.value();
                let new_ref = if value.is_empty() {
                    None
                } else {
                    let Ok(parsed) = uuid::Uuid::parse_str(&value) else {
                        return;
                    };
                    Some(MaterialRef(AssetId(parsed)))
                };
                let mut k = kind.get_cloned();
                write_new(&mut k, new_ref);
                kind.set(k);
            }))
        })
    })
}
