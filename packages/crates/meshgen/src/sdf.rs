//! SDF/CSG graph evaluation (Phase 5). Our value-add is the *distance graph*
//! ([`eval_sdf`]) — pure, agent-composable, and natively tested; the surface
//! extraction (graph → triangles) is delegated to a surface-nets mesher (see
//! [`mesh_sdf`]).
//!
//! Smooth combinators (`smooth > 0`) give rounded/blended booleans for free —
//! which mesh booleans cannot do, the deliberate reason SDF is the chosen CSG
//! paradigm.

use crate::recipe::{SdfNode, SdfPrimitive};
use glam::{Quat, Vec2, Vec3};

/// Signed distance from `p` to the surface described by `node` (negative inside).
pub fn eval_sdf(node: &SdfNode, p: Vec3) -> f32 {
    match node {
        SdfNode::Primitive(prim) => eval_primitive(prim, p),
        SdfNode::Union { smooth, children } => combine(children, p, *smooth, Op::Union),
        SdfNode::Intersect { smooth, children } => combine(children, p, *smooth, Op::Intersect),
        SdfNode::Subtract { smooth, children } => {
            let mut it = children.iter();
            let Some(first) = it.next() else {
                return f32::INFINITY;
            };
            let mut d = eval_sdf(first, p);
            for c in it {
                d = smax(d, -eval_sdf(c, p), *smooth);
            }
            d
        }
        SdfNode::Transform { trs, child } => {
            // Inverse-transform the sample point (assumes uniform scale for a
            // valid distance metric; non-uniform scale is approximate).
            let t = Vec3::from_array(trs.translation);
            let r = Quat::from_array(trs.rotation);
            let s = (trs.scale[0] + trs.scale[1] + trs.scale[2]) / 3.0;
            let s = if s.abs() < 1e-6 { 1.0 } else { s };
            let local = r.inverse() * (p - t) / s;
            eval_sdf(child, local) * s
        }
    }
}

enum Op {
    Union,
    Intersect,
}

fn combine(children: &[SdfNode], p: Vec3, smooth: f32, op: Op) -> f32 {
    let mut it = children.iter();
    let Some(first) = it.next() else {
        return f32::INFINITY;
    };
    let mut d = eval_sdf(first, p);
    for c in it {
        let dc = eval_sdf(c, p);
        d = match op {
            Op::Union => smin(d, dc, smooth),
            Op::Intersect => smax(d, dc, smooth),
        };
    }
    d
}

fn eval_primitive(prim: &SdfPrimitive, p: Vec3) -> f32 {
    match *prim {
        SdfPrimitive::Sphere { radius } => p.length() - radius,
        SdfPrimitive::Box { half } => {
            let q = p.abs() - Vec3::from_array(half);
            q.max(Vec3::ZERO).length() + q.x.max(q.y.max(q.z)).min(0.0)
        }
        SdfPrimitive::Cylinder { radius, height } => {
            let d = Vec2::new(
                Vec2::new(p.x, p.z).length() - radius,
                p.y.abs() - height * 0.5,
            );
            d.x.max(d.y).min(0.0) + d.max(Vec2::ZERO).length()
        }
        SdfPrimitive::Torus { major, minor } => {
            let q = Vec2::new(Vec2::new(p.x, p.z).length() - major, p.y);
            q.length() - minor
        }
        SdfPrimitive::Capsule { radius, height } => {
            let y = p.y.clamp(-height * 0.5, height * 0.5);
            (p - Vec3::new(0.0, y, 0.0)).length() - radius
        }
    }
}

/// Polynomial smooth-min (k = 0 → hard `min`).
fn smin(a: f32, b: f32, k: f32) -> f32 {
    if k <= 0.0 {
        return a.min(b);
    }
    let h = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0);
    lerp(b, a, h) - k * h * (1.0 - h)
}

/// Smooth-max via `-smin(-a, -b, k)`.
fn smax(a: f32, b: f32, k: f32) -> f32 {
    -smin(-a, -b, k)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Bounds of an SDF graph for sizing the sample grid — a conservative AABB
/// computed structurally (primitive extents, expanded by transforms + smoothing).
pub fn sdf_bounds(node: &SdfNode) -> (Vec3, Vec3) {
    match node {
        SdfNode::Primitive(prim) => {
            let r = match *prim {
                SdfPrimitive::Sphere { radius } => Vec3::splat(radius),
                SdfPrimitive::Box { half } => Vec3::from_array(half),
                SdfPrimitive::Cylinder { radius, height } => {
                    Vec3::new(radius, height * 0.5, radius)
                }
                SdfPrimitive::Torus { major, minor } => {
                    Vec3::new(major + minor, minor, major + minor)
                }
                SdfPrimitive::Capsule { radius, height } => {
                    Vec3::new(radius, height * 0.5 + radius, radius)
                }
            };
            (-r, r)
        }
        SdfNode::Union { smooth, children }
        | SdfNode::Intersect { smooth, children }
        | SdfNode::Subtract { smooth, children } => {
            let mut lo = Vec3::splat(f32::INFINITY);
            let mut hi = Vec3::splat(f32::NEG_INFINITY);
            for c in children {
                let (clo, chi) = sdf_bounds(c);
                lo = lo.min(clo);
                hi = hi.max(chi);
            }
            if children.is_empty() {
                (Vec3::splat(-1.0), Vec3::splat(1.0))
            } else {
                let pad = Vec3::splat(smooth.max(0.0));
                (lo - pad, hi + pad)
            }
        }
        SdfNode::Transform { trs, child } => {
            let (lo, hi) = sdf_bounds(child);
            let t = Vec3::from_array(trs.translation);
            let r = Quat::from_array(trs.rotation);
            let s = Vec3::from_array(trs.scale);
            // Transform the 8 corners and re-bound.
            let mut nlo = Vec3::splat(f32::INFINITY);
            let mut nhi = Vec3::splat(f32::NEG_INFINITY);
            for i in 0..8 {
                let c = Vec3::new(
                    if i & 1 == 0 { lo.x } else { hi.x },
                    if i & 2 == 0 { lo.y } else { hi.y },
                    if i & 4 == 0 { lo.z } else { hi.z },
                );
                let w = t + r * (c * s);
                nlo = nlo.min(w);
                nhi = nhi.max(w);
            }
            (nlo, nhi)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::SdfNode as N;
    use crate::recipe::SdfPrimitive as P;

    fn sphere(r: f32) -> N {
        N::Primitive(P::Sphere { radius: r })
    }

    #[test]
    fn sphere_distance() {
        let s = sphere(1.0);
        assert!((eval_sdf(&s, Vec3::ZERO) + 1.0).abs() < 1e-5); // -1 inside
        assert!((eval_sdf(&s, Vec3::new(2.0, 0.0, 0.0)) - 1.0).abs() < 1e-5); // +1 outside
        assert!(eval_sdf(&s, Vec3::new(1.0, 0.0, 0.0)).abs() < 1e-5); // 0 on surface
    }

    #[test]
    fn union_is_min_subtract_carves() {
        let a = N::Transform {
            trs: awsm_scene::Trs {
                translation: [-0.5, 0.0, 0.0],
                ..awsm_scene::Trs::IDENTITY
            },
            child: Box::new(sphere(1.0)),
        };
        let b = N::Transform {
            trs: awsm_scene::Trs {
                translation: [0.5, 0.0, 0.0],
                ..awsm_scene::Trs::IDENTITY
            },
            child: Box::new(sphere(1.0)),
        };
        let u = N::Union {
            smooth: 0.0,
            children: vec![a.clone(), b.clone()],
        };
        let p = Vec3::new(0.5, 0.0, 0.0);
        assert!((eval_sdf(&u, p) - eval_sdf(&a, p).min(eval_sdf(&b, p))).abs() < 1e-5);

        // Subtract b from a: a point inside b is now outside (positive).
        let sub = N::Subtract {
            smooth: 0.0,
            children: vec![a.clone(), b.clone()],
        };
        assert!(eval_sdf(&sub, Vec3::new(0.5, 0.0, 0.0)) > 0.0);
    }

    #[test]
    fn smooth_union_rounds_below_hard_min() {
        // Near the seam, the smooth union dips below the hard min (rounded fillet).
        let a = sphere(1.0);
        let b = N::Transform {
            trs: awsm_scene::Trs {
                translation: [1.5, 0.0, 0.0],
                ..awsm_scene::Trs::IDENTITY
            },
            child: Box::new(sphere(1.0)),
        };
        let p = Vec3::new(0.75, 0.6, 0.0);
        let hard = eval_sdf(&a, p).min(eval_sdf(&b, p));
        let soft = smin(eval_sdf(&a, p), eval_sdf(&b, p), 0.5);
        assert!(soft < hard);
    }

    #[test]
    fn bounds_cover_a_union() {
        let mug = N::Union {
            smooth: 0.1,
            children: vec![
                sphere(1.0),
                N::Transform {
                    trs: awsm_scene::Trs {
                        translation: [3.0, 0.0, 0.0],
                        ..awsm_scene::Trs::IDENTITY
                    },
                    child: Box::new(sphere(1.0)),
                },
            ],
        };
        let (lo, hi) = sdf_bounds(&mug);
        assert!(lo.x <= -1.0 && hi.x >= 4.0);
    }
}
