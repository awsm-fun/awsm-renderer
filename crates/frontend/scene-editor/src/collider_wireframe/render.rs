//! Per-frame wireframe lines for editor-only overlays — collider shapes,
//! camera frustums, selection gizmos.
//!
//! Replaces the prior bespoke line-list pipeline (B-2) with calls into the
//! renderer's `add_line_segments` / `update_line_segments` API. One
//! cumulative `LineKey` holds every editor wireframe; each frame the
//! geometry is re-packed and uploaded, so the fat-line pipeline handles
//! depth-test + screen-space width uniformly with `NodeKind::Line` nodes.

use std::sync::Mutex;

use crate::scene::{CameraConfig, CameraProjection, ColliderShape, NodeId, NodeKind};
use awsm_renderer::{
    bounds::Aabb, render_passes::lines::LineKey, transforms::TransformKey, AwsmRenderer,
};
use glam::{Mat4, Vec3, Vec4};

/// Line width in CSS pixels for the editor wireframe overlay.
const WIRE_WIDTH_PX: f32 = 1.5;

/// Solid alpha component for every wireframe color. The bespoke pipeline
/// previously baked 0.8 into the fragment shader.
const WIRE_ALPHA: f32 = 0.8;

const COLOR_BOX: [f32; 3] = [0.2, 0.9, 0.3];
const COLOR_SPHERE: [f32; 3] = [0.3, 0.5, 0.95];
const COLOR_CAPSULE: [f32; 3] = [0.95, 0.5, 0.8];
const COLOR_CYLINDER: [f32; 3] = [0.9, 0.8, 0.3];
const COLOR_CONE: [f32; 3] = [0.95, 0.4, 0.4];
const COLOR_ELLIPSOID: [f32; 3] = [0.5, 0.85, 0.9];
const COLOR_CAMERA: [f32; 3] = [0.9, 0.43, 1.0];
const COLOR_SELECTION_BBOX: [f32; 3] = [1.0, 0.75, 0.25];
const SPHERE_SEGMENTS: usize = 48;
/// Segments around the cylindrical part of capsules + cylinders + cone
/// bases. 32 is plenty for a wireframe at typical zoom levels.
const CAP_SEGMENTS: usize = 32;

const SELECTION_GIZMO_HALF_LEN: f32 = 0.5;
const COLOR_X: [f32; 3] = [1.0, 0.3, 0.48];
const COLOR_Y: [f32; 3] = [0.49, 1.0, 0.65];
const COLOR_Z: [f32; 3] = [0.4, 0.66, 1.0];

static WIREFRAME_KEY: Mutex<Option<LineKey>> = Mutex::new(None);

/// Sink that accumulates line-list pairs as `(a, b, color)` for every
/// editor-only wireframe shape we draw this frame.
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

#[inline]
fn rgb_to_vec4(rgb: &[f32; 3]) -> Vec4 {
    Vec4::new(rgb[0], rgb[1], rgb[2], WIRE_ALPHA)
}

/// Called once per frame from the editor's render loop, before
/// `renderer.render(...)`. Rebuilds the editor's overlay wireframe lines
/// against the live scene snapshot and uploads them into the renderer's
/// line registry.
pub fn sync_editor_wireframes(renderer: &mut AwsmRenderer) {
    let shapes = collect_shapes();
    let cameras = collect_cameras();
    let selection_origins = collect_selection_origins();
    let selection_bboxes = collect_selection_model_bboxes(renderer);

    let mut buf = WireBuf::default();

    for (key, cfg) in &cameras {
        let world = renderer
            .transforms
            .get_world(*key)
            .copied()
            .unwrap_or(Mat4::IDENTITY);
        push_camera_frustum_wireframe(&mut buf, &world, cfg, &COLOR_CAMERA);
    }

    for (key, shape) in &shapes {
        let world = renderer
            .transforms
            .get_world(*key)
            .copied()
            .unwrap_or(Mat4::IDENTITY);

        match shape {
            ColliderShape::Box { half_extents } => {
                push_box_wireframe(&mut buf, &world, half_extents, &COLOR_BOX);
            }
            ColliderShape::Sphere { radius } => {
                push_sphere_wireframe(&mut buf, &world, *radius, &COLOR_SPHERE);
            }
            ColliderShape::Capsule {
                half_height,
                radius,
            } => {
                push_capsule_wireframe(&mut buf, &world, *half_height, *radius, &COLOR_CAPSULE);
            }
            ColliderShape::Cylinder {
                half_height,
                radius,
            } => {
                push_cylinder_wireframe(&mut buf, &world, *half_height, *radius, &COLOR_CYLINDER);
            }
            ColliderShape::Cone {
                half_height,
                radius,
            } => {
                push_cone_wireframe(&mut buf, &world, *half_height, *radius, &COLOR_CONE);
            }
            ColliderShape::Ellipsoid { half_extents } => {
                push_ellipsoid_wireframe(&mut buf, &world, half_extents, &COLOR_ELLIPSOID);
            }
        }
    }

    for key in &selection_origins {
        if let Ok(world) = renderer.transforms.get_world(*key) {
            push_selection_gizmo(&mut buf, world);
        }
    }

    for bbox in &selection_bboxes {
        push_axis_aligned_box(&mut buf, bbox, &COLOR_SELECTION_BBOX);
    }

    let mut key_lock = WIREFRAME_KEY.lock().unwrap();
    match *key_lock {
        Some(key) => {
            if let Err(err) = renderer.update_line_segments(key, &buf.positions, &buf.colors) {
                tracing::warn!("sync_editor_wireframes: update_line_segments failed: {err}");
            }
        }
        None => {
            if buf.positions.is_empty() {
                return;
            }
            match renderer.add_line_segments(&buf.positions, &buf.colors, WIRE_WIDTH_PX, false) {
                Ok(Some(k)) => *key_lock = Some(k),
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!("sync_editor_wireframes: add_line_segments failed: {err}");
                }
            }
        }
    }
}

/// Walk the bridge's node map, emit `(transform_key, shape)` pairs for
/// every Collision node whose effective visibility is on. Hidden ones
/// get silently skipped.
fn collect_shapes() -> Vec<(TransformKey, ColliderShape)> {
    let bridge = crate::renderer_bridge::bridge();
    let nodes = bridge.nodes.lock().unwrap();
    let mut out = Vec::new();
    for entry in nodes.values() {
        if !*entry.effective_visible.lock().unwrap() {
            continue;
        }
        if let NodeKind::Collider(shape) = entry.node.kind.get_cloned() {
            out.push((entry.transform_key, shape));
        }
    }
    out
}

fn collect_cameras() -> Vec<(TransformKey, CameraConfig)> {
    let bridge = crate::renderer_bridge::bridge();
    let nodes = bridge.nodes.lock().unwrap();
    let mut out = Vec::new();
    for entry in nodes.values() {
        if !*entry.effective_visible.lock().unwrap() {
            continue;
        }
        if let NodeKind::Camera(cfg) = entry.node.kind.get_cloned() {
            out.push((entry.transform_key, cfg));
        }
    }
    out
}

fn collect_selection_origins() -> Vec<TransformKey> {
    let state = crate::state::app_state();
    let selected = state.selected.lock_ref();
    if selected.is_empty() {
        return Vec::new();
    }
    let bridge = crate::renderer_bridge::bridge();
    let nodes = bridge.nodes.lock().unwrap();
    selected
        .iter()
        .filter_map(|id| nodes.get(id).map(|n| n.transform_key))
        .collect()
}

fn collect_selection_model_bboxes(renderer: &AwsmRenderer) -> Vec<Aabb> {
    let state = crate::state::app_state();
    let selected: Vec<_> = state.selected.lock_ref().iter().copied().collect();
    if selected.is_empty() {
        return Vec::new();
    }
    let bridge = crate::renderer_bridge::bridge();
    let nodes = bridge.nodes.lock().unwrap();
    let child_order = bridge.child_order.lock().unwrap();
    let mut out = Vec::new();
    for root_id in selected {
        let mut bbox: Option<Aabb> = None;
        let mut stack: Vec<NodeId> = vec![root_id];
        while let Some(id) = stack.pop() {
            if let Some(entry) = nodes.get(&id) {
                for mesh_key in entry.model_meshes.lock().unwrap().iter() {
                    let Ok(mesh) = renderer.meshes.get(*mesh_key) else {
                        continue;
                    };
                    let Some(world_aabb) = mesh.world_aabb.clone() else {
                        continue;
                    };
                    match &mut bbox {
                        Some(b) => b.extend(&world_aabb),
                        None => bbox = Some(world_aabb),
                    }
                }
            }
            if let Some(children) = child_order.get(&Some(id)) {
                stack.extend(children.iter().copied());
            }
        }
        if let Some(b) = bbox {
            out.push(pad_aabb(&b));
        }
    }
    out
}

fn pad_aabb(aabb: &Aabb) -> Aabb {
    const PAD_FRACTION: f32 = 0.05;
    const PAD_MIN: f32 = 0.01;
    let extent = aabb.max - aabb.min;
    let pad = (extent.max_element() * PAD_FRACTION).max(PAD_MIN);
    let pad_vec = Vec3::splat(pad);
    Aabb {
        min: aabb.min - pad_vec,
        max: aabb.max + pad_vec,
    }
}

/// Emit 12 edges of an axis-aligned bounding box (line-list pairs).
fn push_axis_aligned_box(buf: &mut WireBuf, aabb: &Aabb, color: &[f32; 3]) {
    let mn = aabb.min;
    let mx = aabb.max;
    let corners = [
        Vec3::new(mn.x, mn.y, mn.z),
        Vec3::new(mx.x, mn.y, mn.z),
        Vec3::new(mx.x, mx.y, mn.z),
        Vec3::new(mn.x, mx.y, mn.z),
        Vec3::new(mn.x, mn.y, mx.z),
        Vec3::new(mx.x, mn.y, mx.z),
        Vec3::new(mx.x, mx.y, mx.z),
        Vec3::new(mn.x, mx.y, mx.z),
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
        buf.push_segment(corners[a], corners[b], c);
    }
}

fn push_selection_gizmo(buf: &mut WireBuf, world: &Mat4) {
    let origin = world.transform_point3(Vec3::ZERO);
    let x_axis = world.transform_vector3(Vec3::X * SELECTION_GIZMO_HALF_LEN);
    let y_axis = world.transform_vector3(Vec3::Y * SELECTION_GIZMO_HALF_LEN);
    let z_axis = world.transform_vector3(Vec3::Z * SELECTION_GIZMO_HALF_LEN);
    buf.push_segment(origin - x_axis, origin + x_axis, rgb_to_vec4(&COLOR_X));
    buf.push_segment(origin - y_axis, origin + y_axis, rgb_to_vec4(&COLOR_Y));
    buf.push_segment(origin - z_axis, origin + z_axis, rgb_to_vec4(&COLOR_Z));
}

fn push_box_wireframe(buf: &mut WireBuf, world: &Mat4, half_extents: &[f32; 3], color: &[f32; 3]) {
    let hx = half_extents[0];
    let hy = half_extents[1];
    let hz = half_extents[2];

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

fn push_sphere_wireframe(buf: &mut WireBuf, world: &Mat4, radius: f32, color: &[f32; 3]) {
    let n = SPHERE_SEGMENTS;
    push_circle(buf, world, radius, color, n, |angle| {
        Vec3::new(angle.cos(), angle.sin(), 0.0)
    });
    push_circle(buf, world, radius, color, n, |angle| {
        Vec3::new(angle.cos(), 0.0, angle.sin())
    });
    push_circle(buf, world, radius, color, n, |angle| {
        Vec3::new(0.0, angle.cos(), angle.sin())
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

/// Capsule along local +Y.
fn push_capsule_wireframe(
    buf: &mut WireBuf,
    world: &Mat4,
    half_height: f32,
    radius: f32,
    color: &[f32; 3],
) {
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

fn push_cylinder_wireframe(
    buf: &mut WireBuf,
    world: &Mat4,
    half_height: f32,
    radius: f32,
    color: &[f32; 3],
) {
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

fn push_cone_wireframe(
    buf: &mut WireBuf,
    world: &Mat4,
    half_height: f32,
    radius: f32,
    color: &[f32; 3],
) {
    let apex = Vec3::new(0.0, half_height, 0.0);
    let base_center = Vec3::new(0.0, -half_height, 0.0);
    push_circle_offset(buf, world, base_center, radius, color, CAP_SEGMENTS, |t| {
        Vec3::new(t.cos(), 0.0, t.sin())
    });
    let c = rgb_to_vec4(color);
    for (dx, dz) in &[(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        let base = Vec3::new(radius * dx, -half_height, radius * dz);
        buf.push_segment(
            world.transform_point3(apex),
            world.transform_point3(base),
            c,
        );
    }
}

/// Axis-aligned ellipsoid with per-axis half-extents `[rx, ry, rz]`.
fn push_ellipsoid_wireframe(
    buf: &mut WireBuf,
    world: &Mat4,
    half_extents: &[f32; 3],
    color: &[f32; 3],
) {
    let mesh = awsm_scene_schema::ellipsoid_hull_mesh();
    let rx = half_extents[0];
    let ry = half_extents[1];
    let rz = half_extents[2];
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

/// Camera frustum gizmo. awsm-renderer's pin-hole camera looks down its
/// local -Z, +Y up, +X right. Near rect at `z=-near`, far at `z=-far`.
fn push_camera_frustum_wireframe(
    buf: &mut WireBuf,
    world: &Mat4,
    cfg: &CameraConfig,
    color: &[f32; 3],
) {
    let near = cfg.near;
    let far = cfg.far;
    if near <= 0.0 || far <= near {
        return;
    }

    let aspect = editor_aspect_ratio();

    let (near_hw, near_hh, far_hw, far_hh) = match cfg.projection {
        CameraProjection::Perspective { fov_y_rad } => {
            let half_fov = (fov_y_rad * 0.5).clamp(0.01, std::f32::consts::FRAC_PI_2 - 0.01);
            let tan_half = half_fov.tan();
            let near_hh = near * tan_half;
            let far_hh = far * tan_half;
            (near_hh * aspect, near_hh, far_hh * aspect, far_hh)
        }
        CameraProjection::Orthographic { half_height } => {
            let hh = half_height.max(0.001);
            (hh * aspect, hh, hh * aspect, hh)
        }
    };

    let near_corners = [
        Vec3::new(-near_hw, near_hh, -near),
        Vec3::new(near_hw, near_hh, -near),
        Vec3::new(near_hw, -near_hh, -near),
        Vec3::new(-near_hw, -near_hh, -near),
    ];
    let far_corners = [
        Vec3::new(-far_hw, far_hh, -far),
        Vec3::new(far_hw, far_hh, -far),
        Vec3::new(far_hw, -far_hh, -far),
        Vec3::new(-far_hw, -far_hh, -far),
    ];
    let c = rgb_to_vec4(color);

    for i in 0..4 {
        buf.push_segment(
            world.transform_point3(near_corners[i]),
            world.transform_point3(near_corners[(i + 1) % 4]),
            c,
        );
    }
    for i in 0..4 {
        buf.push_segment(
            world.transform_point3(far_corners[i]),
            world.transform_point3(far_corners[(i + 1) % 4]),
            c,
        );
    }
    for i in 0..4 {
        buf.push_segment(
            world.transform_point3(near_corners[i]),
            world.transform_point3(far_corners[i]),
            c,
        );
    }

    let body_size = (near_hh * 0.6).clamp(0.05, 0.4);
    push_box_wireframe(buf, world, &[body_size, body_size, body_size], color);

    let shoe_h = near_hh * 0.4;
    let shoe_top = world.transform_point3(Vec3::new(0.0, near_hh + shoe_h, -near));
    let shoe_base = world.transform_point3(Vec3::new(0.0, near_hh, -near));
    buf.push_segment(shoe_base, shoe_top, c);
}

fn editor_aspect_ratio() -> f32 {
    if let Some(window) = web_sys::window() {
        let w = window
            .inner_width()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(16.0);
        let h = window
            .inner_height()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(9.0);
        if h > 0.0 {
            return (w / h) as f32;
        }
    }
    16.0 / 9.0
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
