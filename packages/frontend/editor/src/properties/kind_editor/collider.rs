use crate::prelude::*;
use crate::properties::transform::{labeled_axis_input, number_input};
use crate::scene::{ColliderShape, Node, NodeKind};

use super::dispatch::{collider_variant_tag, ColliderVariantTag};
use super::helpers::section_header;

pub(super) fn render_collider_editor(node: Arc<Node>) -> Dom {
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
