//! Editor-side authored-camera preview.
//!
//! Mirrors the player's `scene::camera_driver::drive` but produces a
//! `CameraMatrices` (instead of writing to a renderer transform): the
//! editor's free-fly camera state is preserved on the AppState side,
//! and the render loop swaps in the driven matrices when the user has
//! picked an authored camera from the header.
//!
//! All four `CameraBehavior` variants are handled:
//! - `Static` — read the authored camera node's current world transform
//!   from the renderer.
//! - `Follow` — target's world pos + authored offset (+optional look-at).
//! - `OrbitTarget` — spherical pose around the target, auto-rotated by
//!   wall-clock time.
//! - `RailAlongCurve` — time-driven traversal of the referenced curve
//!   (looped every 30 s), looking either at a target or ahead along the
//!   curve.
//!
//! Projection is read from the authored `CameraConfig` (perspective
//! `fov_y_rad` or orthographic `half_height`), with the editor canvas's
//! aspect ratio applied at evaluation time.
//!
//! Callers: the render loop swaps the driver's output in when
//! `AppState::editor_camera_target` is `Some(node_id)`; the canvas /
//! free-fly state is left untouched so switching back to "Free Fly"
//! restores the exact prior view.
//!
//! Cycle protection: `Follow` / `OrbitTarget` / `RailAlongCurve.target`
//! chains can in principle reference each other in a loop. The runtime
//! driver short-circuits gracefully (a missing target → skip the
//! evaluation, the previous matrices stand). Edit-time validation is
//! tracked separately in the camera-behavior inspector form.

use awsm_curves::{CatmullRomCurve, Curve3};
use awsm_renderer::{camera::CameraMatrices, AwsmRenderer};
use awsm_scene_schema::{CameraBehavior, CameraConfig, CameraProjection, NodeId};
use glam::{Mat4, Quat, Vec3};
use std::collections::HashSet;
use std::sync::Arc;

use crate::scene::{Node, NodeKind};
use crate::state::app_state;

/// Evaluate the driven `CameraMatrices` for the authored camera node
/// `node_id`. Returns `None` if:
/// - the node doesn't exist in the scene, or
/// - it isn't a `NodeKind::Camera`, or
/// - the behavior chain is broken (e.g. a `Follow` target was deleted)
///   — caller should fall back to the previous matrices.
///
/// `wall_clock_ms` is the `requestAnimationFrame` timestamp, used as
/// the t-source for orbit auto-rotation + rail traversal.
pub fn evaluate(
    node_id: NodeId,
    renderer: &AwsmRenderer,
    wall_clock_ms: f64,
) -> Option<CameraMatrices> {
    let state = app_state();
    let scene_nodes = state.scene.nodes.lock_ref();
    let node = find_node_recursive(&scene_nodes, node_id)?;
    let cfg = match &*node.kind.lock_ref() {
        NodeKind::Camera(c) => c.clone(),
        _ => return None,
    };
    drop(scene_nodes);

    let t_secs = (wall_clock_ms / 1000.0) as f32;
    let (camera_pos, camera_rot) = match &cfg.behavior {
        CameraBehavior::Static => behavior_static(node_id, renderer)?,
        CameraBehavior::Follow {
            target,
            offset,
            look_at_target,
        } => behavior_follow(*target, *offset, *look_at_target, renderer)?,
        CameraBehavior::OrbitTarget {
            target,
            distance,
            pitch,
            yaw,
            auto_rotate_speed,
        } => behavior_orbit(
            *target,
            *distance,
            *pitch,
            *yaw,
            *auto_rotate_speed,
            t_secs,
            renderer,
        )?,
        CameraBehavior::RailAlongCurve {
            curve,
            look_ahead_distance,
            target,
        } => behavior_rail(*curve, *look_ahead_distance, *target, t_secs, renderer)?,
    };

    let (viewport_w, viewport_h) = renderer.gpu.canvas_size(false);
    let aspect = if viewport_h > 0.0 {
        (viewport_w / viewport_h) as f32
    } else {
        16.0 / 9.0
    };
    let projection = projection_matrix(&cfg, aspect);
    let view = view_matrix(camera_pos, camera_rot);

    Some(CameraMatrices {
        view,
        projection,
        position_world: camera_pos,
        // Defaults from FreeCamera::new_aabb — the authored camera schema
        // doesn't model aperture/focus distance yet, so use the same DOF
        // defaults the free-fly camera does.
        focus_distance: 10.0,
        aperture: 5.6,
    })
}

fn behavior_static(node_id: NodeId, renderer: &AwsmRenderer) -> Option<(Vec3, Quat)> {
    let tk = transform_key_for(node_id)?;
    let world = renderer.transforms.get_world(tk).ok().copied()?;
    let (_scale, rot, pos) = world.to_scale_rotation_translation();
    Some((pos, rot))
}

fn behavior_follow(
    target: NodeId,
    offset: [f32; 3],
    look_at_target: bool,
    renderer: &AwsmRenderer,
) -> Option<(Vec3, Quat)> {
    let target_pos = world_position(renderer, target)?;
    let pos = target_pos + Vec3::from_array(offset);
    let rot = if look_at_target {
        look_at_rotation(pos, target_pos, Vec3::Y)
    } else {
        Quat::IDENTITY
    };
    Some((pos, rot))
}

#[allow(clippy::too_many_arguments)]
fn behavior_orbit(
    target: NodeId,
    distance: f32,
    pitch: f32,
    yaw: f32,
    auto_rotate_speed: f32,
    t_secs: f32,
    renderer: &AwsmRenderer,
) -> Option<(Vec3, Quat)> {
    let target_pos = world_position(renderer, target)?;
    let yaw_now = yaw + auto_rotate_speed * t_secs;
    let cos_p = pitch.cos();
    let offset = Vec3::new(yaw_now.sin() * cos_p, pitch.sin(), yaw_now.cos() * cos_p) * distance;
    let pos = target_pos + offset;
    let rot = look_at_rotation(pos, target_pos, Vec3::Y);
    Some((pos, rot))
}

fn behavior_rail(
    curve_id: NodeId,
    look_ahead_distance: f32,
    target: Option<NodeId>,
    t_secs: f32,
    renderer: &AwsmRenderer,
) -> Option<(Vec3, Quat)> {
    let curve_def = lookup_curve_def(curve_id)?;
    let curve = CatmullRomCurve::new(
        curve_def
            .control_points
            .iter()
            .map(|p| Vec3::from_array(*p))
            .collect(),
        curve_def.closed,
    );
    // 30-second loop, matching the player driver's pacing.
    let phase = (t_secs * 0.0333).fract();
    let pos = curve.point_at(phase);
    let look = if let Some(tn) = target {
        world_position(renderer, tn).unwrap_or(pos + curve.tangent_at(phase))
    } else {
        let look_t = (phase + look_ahead_distance.max(0.001) * 0.05).fract();
        curve.point_at(look_t)
    };
    let rot = look_at_rotation(pos, look, Vec3::Y);
    Some((pos, rot))
}

fn world_position(renderer: &AwsmRenderer, target: NodeId) -> Option<Vec3> {
    let tk = transform_key_for(target)?;
    renderer
        .transforms
        .get_world(tk)
        .ok()
        .map(|m| m.w_axis.truncate())
}

fn transform_key_for(node_id: NodeId) -> Option<awsm_renderer::transforms::TransformKey> {
    let bridge = app_state().renderer_bridge.clone();
    let nodes = bridge.nodes.lock().unwrap();
    nodes.get(&node_id).map(|e| e.transform_key)
}

fn lookup_curve_def(node_id: NodeId) -> Option<awsm_scene_schema::CurveDef> {
    let state = app_state();
    let scene_nodes = state.scene.nodes.lock_ref();
    let node = find_node_recursive(&scene_nodes, node_id)?;
    let kind = node.kind.get_cloned();
    match kind {
        NodeKind::Curve(c) => Some(c),
        _ => None,
    }
}

fn find_node_recursive(nodes: &[Arc<Node>], target: NodeId) -> Option<Arc<Node>> {
    for n in nodes.iter() {
        if n.id == target {
            return Some(n.clone());
        }
        let children = n.children.lock_ref();
        if let Some(found) = find_node_recursive(&children, target) {
            return Some(found);
        }
    }
    None
}

fn look_at_rotation(eye: Vec3, target: Vec3, up: Vec3) -> Quat {
    let forward = (target - eye).normalize_or_zero();
    if forward.length_squared() < 1.0e-6 {
        return Quat::IDENTITY;
    }
    let right = forward.cross(up).normalize_or_zero();
    let actual_up = right.cross(forward).normalize_or_zero();
    let mat = glam::Mat3::from_cols(right, actual_up, -forward);
    Quat::from_mat3(&mat)
}

fn view_matrix(pos: Vec3, rot: Quat) -> Mat4 {
    // Camera looks down -Z in its local frame. Build the world →
    // camera transform: invert the world transform.
    let world = Mat4::from_rotation_translation(rot, pos);
    world.inverse()
}

fn projection_matrix(cfg: &CameraConfig, aspect: f32) -> Mat4 {
    match cfg.projection {
        CameraProjection::Perspective { fov_y_rad } => {
            Mat4::perspective_rh(fov_y_rad, aspect.max(0.01), cfg.near, cfg.far)
        }
        CameraProjection::Orthographic { half_height } => {
            let h = half_height.max(0.001);
            let w = h * aspect.max(0.01);
            Mat4::orthographic_rh(-w, w, -h, h, cfg.near, cfg.far)
        }
    }
}

/// Enumerate every `NodeKind::Camera` in the scene, returning
/// `(node_id, display_name)` pairs in scene-order. Used by the header
/// dropdown to populate the authored-camera picker.
pub fn list_authored_cameras() -> Vec<(NodeId, String)> {
    let state = app_state();
    let scene_nodes = state.scene.nodes.lock_ref();
    let mut out = Vec::new();
    walk_collect(&scene_nodes, &mut out);
    out
}

fn walk_collect(nodes: &[Arc<Node>], out: &mut Vec<(NodeId, String)>) {
    for n in nodes.iter() {
        if matches!(&*n.kind.lock_ref(), NodeKind::Camera(_)) {
            out.push((n.id, n.name.lock_ref().clone()));
        }
        let children = n.children.lock_ref();
        walk_collect(&children, out);
    }
}

/// Detect a cycle in this camera's behavior chain. Mirrors the
/// player's `validate_no_cycles` walk; used at evaluation time to
/// avoid infinite recursion if a user authored a cyclic chain.
#[allow(dead_code)]
fn would_cycle(start: NodeId, current: NodeId, visited: &mut HashSet<NodeId>) -> bool {
    if !visited.insert(current) {
        return false;
    }
    let state = app_state();
    let scene_nodes = state.scene.nodes.lock_ref();
    let Some(node) = find_node_recursive(&scene_nodes, current) else {
        return false;
    };
    let next: Option<NodeId> = match &*node.kind.lock_ref() {
        NodeKind::Camera(cfg) => match &cfg.behavior {
            CameraBehavior::Static => None,
            CameraBehavior::Follow { target, .. } => Some(*target),
            CameraBehavior::OrbitTarget { target, .. } => Some(*target),
            CameraBehavior::RailAlongCurve { target, .. } => *target,
        },
        _ => None,
    };
    let Some(next) = next else { return false };
    drop(scene_nodes);
    if next == start {
        return true;
    }
    would_cycle(start, next, visited)
}
