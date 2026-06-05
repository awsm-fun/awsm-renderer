//! Collider wireframe geometry — builds line-list segments for each
//! [`ColliderShape`] so a `NodeKind::Collider` renders an editor overlay
//! wireframe via the renderer's fat-line pipeline. Ported (one-shot, world-baked
//! per node) from the archived editor's per-frame `collider_wireframe`.

use awsm_scene_schema::ColliderShape;
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
    let mesh = awsm_scene_schema::ellipsoid_hull_mesh();
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
