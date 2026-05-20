use crate::prelude::*;
use crate::scene::{ColliderShape, LightKind, Node, NodeKind};

use super::{
    camera, collider, curve, instances, light, line, mesh, model, particle, primitive, sprite,
    sweep,
};

/// Top-level discriminant for `NodeKind`. The kind-editor's outer
/// `child_signal` dedupes on this so a value change inside the same
/// kind (e.g. radius tweak inside a Sphere) doesn't tear the inspector
/// down and rebuild it from scratch — which would detach inputs
/// mid-drag, mid-typing, etc.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum NodeKindTag {
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

pub(super) fn node_kind_tag(k: &NodeKind) -> NodeKindTag {
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
pub(super) enum ColliderVariantTag {
    Box,
    Sphere,
    Capsule,
    Cylinder,
    Cone,
    Ellipsoid,
}

pub(super) fn collider_variant_tag(k: &NodeKind) -> Option<ColliderVariantTag> {
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
pub(super) fn light_variant_tag(k: &NodeKind) -> Option<LightKind> {
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
                NodeKindTag::Model => model::render_model_editor(node.clone()),
                NodeKindTag::Light => light::render_light_editor(node.clone()),
                NodeKindTag::Collider => collider::render_collider_editor(node.clone()),
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

fn render_group_stub() -> Dom {
    html!("div", {
        .style("font-size", "0.8rem")
        .style("color", ColorText::Byline.value())
        .text("Group — no additional properties.")
    })
}

// All NodeKind variants now have their own `render_*_editor`; the
// generic-stub helper was retired alongside the last placeholder.
