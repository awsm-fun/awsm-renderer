//! USD's intrinsic shapes (`Cube`, `Sphere`, `Cylinder`, `Capsule`, `Cone`,
//! `Plane`) as sidecar primitive geoms.
//!
//! The sidecar speaks MuJoCo's primitive vocabulary — half-extents, radii and
//! half-lengths along local **Z** — so each USD shape's own parameters (full
//! `size`, full `height`, an `axis` token) and its transform's scale are folded
//! into that form. The geom keeps a real body-relative offset; only meshes get
//! their offset baked into vertices.

use awsm_renderer_mujoco_format::sidecar::GeomKind;
use glam::{DQuat, DVec3};
use openusd::usd::Prim;

use crate::stage::{value, value_f64, value_token};

/// A primitive in sidecar terms: the kind, MuJoCo's three size slots, and an
/// extra rotation that turns the shape's own axis onto local Z.
pub struct Primitive {
    pub kind: GeomKind,
    pub size: [f64; 3],
    pub axis_rotation: DQuat,
    /// A shape we can only approximate (a cone drawn as a cylinder).
    pub approximate: bool,
}

/// Map `prim` onto a primitive, given the scale of its transform relative to
/// its body. `None` when `prim` is not an intrinsic shape.
pub fn from_prim(prim: &Prim, scale: DVec3) -> Option<Primitive> {
    let ty = prim.type_name().ok()??;
    let f = |name: &str, default: f64| {
        value(&prim.attribute(name))
            .and_then(|v| value_f64(&v))
            .unwrap_or(default)
    };
    let axis = value(&prim.attribute("axis"))
        .and_then(|v| value_token(&v))
        .unwrap_or_else(|| "Z".into());
    // The shape's own axis in its local frame, and the rotation carrying local
    // Z onto it (the sidecar's shapes run along Z).
    let (axis_rotation, along, across) = match axis.as_str() {
        "X" => (
            DQuat::from_rotation_y(std::f64::consts::FRAC_PI_2),
            scale.x,
            scale.y.max(scale.z),
        ),
        "Y" => (
            DQuat::from_rotation_x(-std::f64::consts::FRAC_PI_2),
            scale.y,
            scale.x.max(scale.z),
        ),
        _ => (DQuat::IDENTITY, scale.z, scale.x.max(scale.y)),
    };
    let prim_of = |kind, size| Primitive {
        kind,
        size,
        axis_rotation: DQuat::IDENTITY,
        approximate: false,
    };
    Some(match ty.as_str() {
        "Cube" => {
            let half = f("size", 2.0) / 2.0;
            prim_of(
                GeomKind::Box,
                [half * scale.x, half * scale.y, half * scale.z],
            )
        }
        "Sphere" => {
            let r = f("radius", 1.0);
            let uniform = (scale.x - scale.y).abs() < 1e-9 && (scale.y - scale.z).abs() < 1e-9;
            if uniform {
                prim_of(GeomKind::Sphere, [r * scale.x, 0.0, 0.0])
            } else {
                prim_of(GeomKind::Ellipsoid, [r * scale.x, r * scale.y, r * scale.z])
            }
        }
        // USD `height` is the full length of the straight part (a capsule's caps
        // are extra); MuJoCo wants half of it.
        "Cylinder" | "Capsule" | "Cone" => {
            let (r_default, h_default) = if ty.as_str() == "Capsule" {
                (0.5, 1.0)
            } else {
                (1.0, 2.0)
            };
            let r = f("radius", r_default) * across;
            let half = f("height", h_default) / 2.0 * along;
            let kind = if ty.as_str() == "Capsule" {
                GeomKind::Capsule
            } else {
                GeomKind::Cylinder
            };
            Primitive {
                kind,
                // A cone has no MuJoCo form; a cylinder at its mean radius is
                // the closest silhouette, and it is reported as approximate.
                size: [if ty.as_str() == "Cone" { r / 2.0 } else { r }, half, 0.0],
                axis_rotation,
                approximate: ty.as_str() == "Cone",
            }
        }
        "Plane" => {
            let w = f("width", 2.0) / 2.0;
            let l = f("length", 2.0) / 2.0;
            Primitive {
                kind: GeomKind::Plane,
                size: [w * scale.x, l * scale.y, 0.0],
                axis_rotation,
                approximate: false,
            }
        }
        _ => return None,
    })
}
