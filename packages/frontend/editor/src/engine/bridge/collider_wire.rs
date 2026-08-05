//! Collider wireframe geometry — builds line-list segments for each
//! [`ColliderShape`] so a `NodeKind::Collider` renders an editor overlay
//! wireframe via the renderer's fat-line pipeline. Ported (one-shot, world-baked
//! per node) from the archived editor's per-frame `collider_wireframe`.

use awsm_renderer_editor_protocol::ColliderShape;
use glam::{Mat4, Vec3, Vec4};

const WIRE_ALPHA: f32 = 0.8;
const SPHERE_SEGMENTS: usize = 48;
const CAP_SEGMENTS: usize = 32;

const COLOR_BOX: [f32; 3] = [0.2, 0.9, 0.3];
const COLOR_SPHERE: [f32; 3] = [0.3, 0.5, 0.95];
const COLOR_CAPSULE: [f32; 3] = [0.95, 0.5, 0.8];
const COLOR_CYLINDER: [f32; 3] = [0.9, 0.8, 0.3];
const COLOR_CONE: [f32; 3] = [0.95, 0.4, 0.4];
const COLOR_ELLIPSOID: [f32; 3] = [0.5, 0.85, 0.9];
const COLOR_HULL: [f32; 3] = [1.0, 0.65, 0.15];

#[derive(Default)]
struct WireBuf {
    positions: Vec<Vec3>,
    colors: Vec<Vec4>,
}

impl WireBuf {
    fn push_segment(&mut self, a: Vec3, b: Vec3, color: Vec4) {
        self.positions.push(a);
        self.positions.push(b);
        self.colors.push(color);
        self.colors.push(color);
    }
}

fn rgb_to_vec4(rgb: &[f32; 3]) -> Vec4 {
    Vec4::new(rgb[0], rgb[1], rgb[2], WIRE_ALPHA)
}

/// World-baked line-list segments (positions in pairs, color per vertex) for the
/// given collider shape. Feed to `add_line_segments`.
pub fn build(shape: &ColliderShape, world: &Mat4) -> (Vec<Vec3>, Vec<Vec4>) {
    let mut buf = WireBuf::default();
    match shape {
        ColliderShape::Box { half_extents } => push_box(&mut buf, world, half_extents, &COLOR_BOX),
        ColliderShape::Sphere { radius } => push_sphere(&mut buf, world, *radius, &COLOR_SPHERE),
        ColliderShape::Capsule {
            half_height,
            radius,
        } => push_capsule(&mut buf, world, *half_height, *radius, &COLOR_CAPSULE),
        ColliderShape::Cylinder {
            half_height,
            radius,
        } => push_cylinder(&mut buf, world, *half_height, *radius, &COLOR_CYLINDER),
        ColliderShape::Cone {
            half_height,
            radius,
        } => push_cone(&mut buf, world, *half_height, *radius, &COLOR_CONE),
        ColliderShape::Ellipsoid { half_extents } => {
            push_ellipsoid(&mut buf, world, half_extents, &COLOR_ELLIPSOID)
        }
        ColliderShape::ConvexHull { points } => push_hull(&mut buf, world, points, &COLOR_HULL),
    }
    (buf.positions, buf.colors)
}

fn push_box(buf: &mut WireBuf, world: &Mat4, half_extents: &[f32; 3], color: &[f32; 3]) {
    let [hx, hy, hz] = *half_extents;
    let corners = [
        Vec3::new(-hx, -hy, -hz),
        Vec3::new(hx, -hy, -hz),
        Vec3::new(hx, hy, -hz),
        Vec3::new(-hx, hy, -hz),
        Vec3::new(-hx, -hy, hz),
        Vec3::new(hx, -hy, hz),
        Vec3::new(hx, hy, hz),
        Vec3::new(-hx, hy, hz),
    ];
    let edges: [(usize, usize); 12] = [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];
    let c = rgb_to_vec4(color);
    for &(a, b) in &edges {
        buf.push_segment(
            world.transform_point3(corners[a]),
            world.transform_point3(corners[b]),
            c,
        );
    }
}

fn push_sphere(buf: &mut WireBuf, world: &Mat4, radius: f32, color: &[f32; 3]) {
    let n = SPHERE_SEGMENTS;
    push_circle(buf, world, radius, color, n, |a| {
        Vec3::new(a.cos(), a.sin(), 0.0)
    });
    push_circle(buf, world, radius, color, n, |a| {
        Vec3::new(a.cos(), 0.0, a.sin())
    });
    push_circle(buf, world, radius, color, n, |a| {
        Vec3::new(0.0, a.cos(), a.sin())
    });
}

fn push_circle(
    buf: &mut WireBuf,
    world: &Mat4,
    radius: f32,
    color: &[f32; 3],
    segments: usize,
    point_fn: impl Fn(f32) -> Vec3,
) {
    let step = core::f32::consts::TAU / segments as f32;
    let c = rgb_to_vec4(color);
    for i in 0..segments {
        let a = i as f32 * step;
        let b = (i + 1) as f32 * step;
        buf.push_segment(
            world.transform_point3(point_fn(a) * radius),
            world.transform_point3(point_fn(b) * radius),
            c,
        );
    }
}

fn push_circle_offset(
    buf: &mut WireBuf,
    world: &Mat4,
    center: Vec3,
    radius: f32,
    color: &[f32; 3],
    segments: usize,
    point_fn: impl Fn(f32) -> Vec3,
) {
    let step = core::f32::consts::TAU / segments as f32;
    let c = rgb_to_vec4(color);
    for i in 0..segments {
        let a = i as f32 * step;
        let b = (i + 1) as f32 * step;
        buf.push_segment(
            world.transform_point3(center + point_fn(a) * radius),
            world.transform_point3(center + point_fn(b) * radius),
            c,
        );
    }
}

fn push_half_circle(
    buf: &mut WireBuf,
    world: &Mat4,
    center: Vec3,
    radius: f32,
    color: &[f32; 3],
    point_fn: impl Fn(f32) -> Vec3,
) {
    let segments = SPHERE_SEGMENTS / 2;
    let step = core::f32::consts::PI / segments as f32;
    let c = rgb_to_vec4(color);
    for i in 0..segments {
        let a = i as f32 * step;
        let b = (i + 1) as f32 * step;
        buf.push_segment(
            world.transform_point3(center + point_fn(a) * radius),
            world.transform_point3(center + point_fn(b) * radius),
            c,
        );
    }
}

fn push_capsule(buf: &mut WireBuf, world: &Mat4, half_height: f32, radius: f32, color: &[f32; 3]) {
    let top = Vec3::new(0.0, half_height, 0.0);
    let bot = Vec3::new(0.0, -half_height, 0.0);
    let c = rgb_to_vec4(color);
    push_half_circle(buf, world, top, radius, color, |t| {
        Vec3::new(t.cos(), t.sin(), 0.0)
    });
    push_half_circle(buf, world, top, radius, color, |t| {
        Vec3::new(0.0, t.sin(), t.cos())
    });
    push_half_circle(buf, world, bot, radius, color, |t| {
        Vec3::new(t.cos(), -t.sin(), 0.0)
    });
    push_half_circle(buf, world, bot, radius, color, |t| {
        Vec3::new(0.0, -t.sin(), t.cos())
    });
    push_circle_offset(buf, world, top, radius, color, CAP_SEGMENTS, |t| {
        Vec3::new(t.cos(), 0.0, t.sin())
    });
    push_circle_offset(buf, world, bot, radius, color, CAP_SEGMENTS, |t| {
        Vec3::new(t.cos(), 0.0, t.sin())
    });
    for (dx, dz) in &[(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        let a = Vec3::new(radius * dx, half_height, radius * dz);
        let b = Vec3::new(radius * dx, -half_height, radius * dz);
        buf.push_segment(world.transform_point3(a), world.transform_point3(b), c);
    }
}

fn push_cylinder(buf: &mut WireBuf, world: &Mat4, half_height: f32, radius: f32, color: &[f32; 3]) {
    let top = Vec3::new(0.0, half_height, 0.0);
    let bot = Vec3::new(0.0, -half_height, 0.0);
    push_circle_offset(buf, world, top, radius, color, CAP_SEGMENTS, |t| {
        Vec3::new(t.cos(), 0.0, t.sin())
    });
    push_circle_offset(buf, world, bot, radius, color, CAP_SEGMENTS, |t| {
        Vec3::new(t.cos(), 0.0, t.sin())
    });
    let c = rgb_to_vec4(color);
    for (dx, dz) in &[(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        let a = Vec3::new(radius * dx, half_height, radius * dz);
        let b = Vec3::new(radius * dx, -half_height, radius * dz);
        buf.push_segment(world.transform_point3(a), world.transform_point3(b), c);
    }
}

fn push_cone(buf: &mut WireBuf, world: &Mat4, half_height: f32, radius: f32, color: &[f32; 3]) {
    let apex = Vec3::new(0.0, half_height, 0.0);
    let base = Vec3::new(0.0, -half_height, 0.0);
    push_circle_offset(buf, world, base, radius, color, CAP_SEGMENTS, |t| {
        Vec3::new(t.cos(), 0.0, t.sin())
    });
    let c = rgb_to_vec4(color);
    for (dx, dz) in &[(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        let b = Vec3::new(radius * dx, -half_height, radius * dz);
        buf.push_segment(world.transform_point3(apex), world.transform_point3(b), c);
    }
}

fn push_ellipsoid(buf: &mut WireBuf, world: &Mat4, half_extents: &[f32; 3], color: &[f32; 3]) {
    let mesh = awsm_renderer_editor_protocol::ellipsoid_hull_mesh();
    let [rx, ry, rz] = *half_extents;
    let c = rgb_to_vec4(color);
    for &(a, b) in &mesh.edges {
        let va = mesh.vertices[a as usize];
        let vb = mesh.vertices[b as usize];
        buf.push_segment(
            world.transform_point3(Vec3::new(va[0] * rx, va[1] * ry, va[2] * rz)),
            world.transform_point3(Vec3::new(vb[0] * rx, vb[1] * ry, vb[2] * rz)),
            c,
        );
    }
}

/// Convex hull wireframe: re-solve the hull from the authored point cloud
/// (the same solve the physics host performs — one source of truth, no
/// stored face table to drift) and draw its unique edges.
///
/// Cached per point-cloud content hash: `materialize_collider` re-bakes on
/// every transform change, and the hull solve is the one collider whose
/// geometry costs more than arithmetic.
fn push_hull(buf: &mut WireBuf, world: &Mat4, points: &[[f32; 3]], color: &[f32; 3]) {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    thread_local! {
        static EDGE_CACHE: RefCell<HashMap<u64, Rc<Vec<([f32; 3], [f32; 3])>>>> =
            RefCell::new(HashMap::new());
    }

    fn cloud_hash(points: &[[f32; 3]]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for p in points {
            for c in p {
                c.to_bits().hash(&mut h);
            }
        }
        h.finish()
    }

    fn solve_edges(points: &[[f32; 3]]) -> Vec<([f32; 3], [f32; 3])> {
        // parry3d re-exports its own glam; build its Vec3 from arrays so a
        // glam version skew between the workspace and parry can't bite.
        let cloud: Vec<parry3d::math::Vec3> = points
            .iter()
            .map(|p| parry3d::math::Vec3::new(p[0], p[1], p[2]))
            .collect();
        let Ok((verts, faces)) = parry3d::transformation::try_convex_hull(cloud.as_slice()) else {
            // Degenerate cloud (coplanar, < 4 points): draw the raw point
            // cloud as tiny crosses so the broken collider is still visible.
            return points
                .iter()
                .map(|p| {
                    (
                        [p[0] - 0.02, p[1], p[2]],
                        [p[0] + 0.02, p[1], p[2]],
                    )
                })
                .collect();
        };
        let mut edges = std::collections::BTreeSet::new();
        for f in &faces {
            for (a, b) in [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
                edges.insert((a.min(b), a.max(b)));
            }
        }
        edges
            .into_iter()
            .map(|(a, b)| {
                let va = verts[a as usize];
                let vb = verts[b as usize];
                ([va.x, va.y, va.z], [vb.x, vb.y, vb.z])
            })
            .collect()
    }

    let key = cloud_hash(points);
    let edges = EDGE_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        if cache.len() > 64 {
            cache.clear();
        }
        cache
            .entry(key)
            .or_insert_with(|| Rc::new(solve_edges(points)))
            .clone()
    });

    let col = rgb_to_vec4(color);
    for (a, b) in edges.iter() {
        buf.push_segment(
            world.transform_point3(Vec3::from_array(*a)),
            world.transform_point3(Vec3::from_array(*b)),
            col,
        );
    }
}
