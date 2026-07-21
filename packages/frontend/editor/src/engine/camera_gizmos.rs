//! Camera **frustum gizmo** — a wireframe frustum drawn at every scene `Camera`
//! node so a camera's placement, orientation and coverage are visible in the
//! viewport (previously a camera node had no viewport indication at all).
//!
//! Follows the `light_icons` settings pattern (`SetViewOptions.camera_gizmos` →
//! `settings.camera_gizmos` → this module's `per_frame_update`) and the
//! `skeleton_viz` drawing pattern: ONE persistent fat-line `LineKey` carries
//! every camera's frustum edges, updated in place each frame (GPU buffer
//! reused), with the CPU gather buffers held in a thread-local `Scratch` so an
//! enabled overlay allocates nothing at steady state.
//!
//! The frustum is computed from the same source the renderer WOULD render with
//! if that camera were active: the animatable `Cameras` store params when the
//! node's slot is materialized (falling back to the node config), the node's
//! world transform for the pose, and the LIVE surface aspect (a scene camera
//! renders at the viewport's aspect, so that is the truth to draw). The
//! ACTIVE camera's own frustum is skipped — when looking through a camera you
//! are inside its frustum and the lines would only smear across the screen.

use std::cell::{Cell, RefCell};

use awsm_renderer::camera::CameraProjectionParams;
use awsm_renderer::render_passes::lines::LineKey;
use awsm_renderer::AwsmRenderer;
use glam::{Mat4, Vec3, Vec4};

use crate::engine::bridge::bridge;
use crate::engine::scene::{NodeId, NodeKind};

thread_local! {
    static FRUSTA: Cell<Option<LineKey>> = const { Cell::new(None) };
    /// Per-frame gather buffers, reused across frames (cleared, capacity
    /// retained) — zero heap allocation at steady state, like `skeleton_viz`.
    static SCRATCH: RefCell<Scratch> = const { RefCell::new(Scratch::new()) };
}

/// Reused per-frame working set.
struct Scratch {
    /// Camera node ids (collected so the `nodes` lock is held only once).
    cameras: Vec<(NodeId, CameraSnapshot)>,
    positions: Vec<Vec3>,
    colors: Vec<Vec4>,
}

impl Scratch {
    const fn new() -> Self {
        Self {
            cameras: Vec::new(),
            positions: Vec::new(),
            colors: Vec::new(),
        }
    }
    fn clear(&mut self) {
        self.cameras.clear();
        self.positions.clear();
        self.colors.clear();
    }
}

/// One camera's live pose + projection params, snapshotted per frame.
struct CameraSnapshot {
    world: Mat4,
    projection: CameraProjectionParams,
    near: f32,
    far: f32,
}

/// Cool steel-blue, distinct from the bone-orange skeleton overlay and the
/// amber light rays. (Saturation, not luminance, survives the fat-line
/// target's per-channel clamp — see `skeleton_viz::BONE_COLOR`.)
const FRUSTUM_COLOR: Vec4 = Vec4::new(0.25, 0.65, 0.95, 1.0);
/// The near rectangle + apex edges render at full strength; the far plane and
/// its connecting edges are dimmed so a long frustum doesn't shout across the
/// whole scene.
const FRUSTUM_COLOR_FAR: Vec4 = Vec4::new(0.25, 0.65, 0.95, 0.35);
const FRUSTUM_WIDTH: f32 = 2.0;

/// Display clamp for the FAR plane (world units). The frustum is drawn to the
/// camera's real far plane when it is closer than this; an authored far of
/// hundreds of metres would otherwise draw a gizmo spanning the entire scene.
const FAR_DISPLAY_CAP: f32 = 60.0;

/// The 4 corners of the view rectangle at view-space depth `dist` (camera
/// looks down local -Z), in the camera's LOCAL space, transformed to world by
/// `world`. Order: bottom-left, bottom-right, top-right, top-left.
fn rect_corners(
    world: &Mat4,
    projection: &CameraProjectionParams,
    aspect: f32,
    dist: f32,
    out: &mut [Vec3; 4],
) {
    let half_h = match projection {
        CameraProjectionParams::Perspective { fov_y_rad } => (fov_y_rad * 0.5).tan() * dist,
        CameraProjectionParams::Orthographic { half_height } => *half_height,
    };
    let half_w = half_h * aspect;
    let corners = [
        Vec3::new(-half_w, -half_h, -dist),
        Vec3::new(half_w, -half_h, -dist),
        Vec3::new(half_w, half_h, -dist),
        Vec3::new(-half_w, half_h, -dist),
    ];
    for (o, c) in out.iter_mut().zip(corners) {
        *o = world.transform_point3(c);
    }
}

/// Append one camera's frustum wireframe: near rect, (display-clamped) far
/// rect, the 4 connecting edges, the 4 apex→near edges, and an "up" triangle
/// above the near plane's top edge (the DCC orientation cue — which way is up).
fn append_frustum(
    snap: &CameraSnapshot,
    aspect: f32,
    positions: &mut Vec<Vec3>,
    colors: &mut Vec<Vec4>,
) {
    let near = snap.near.max(1e-3);
    let far = snap.far.clamp(near * 1.001, FAR_DISPLAY_CAP);

    let mut near_c = [Vec3::ZERO; 4];
    let mut far_c = [Vec3::ZERO; 4];
    rect_corners(&snap.world, &snap.projection, aspect, near, &mut near_c);
    rect_corners(&snap.world, &snap.projection, aspect, far, &mut far_c);

    let mut seg = |a: Vec3, b: Vec3, color: Vec4| {
        positions.push(a);
        positions.push(b);
        colors.push(color);
        colors.push(color);
    };

    let apex = snap.world.transform_point3(Vec3::ZERO);
    for i in 0..4 {
        let j = (i + 1) % 4;
        // Near + far rectangles.
        seg(near_c[i], near_c[j], FRUSTUM_COLOR);
        seg(far_c[i], far_c[j], FRUSTUM_COLOR_FAR);
        // Connecting edges + apex→near (the apex edges are what read as "a
        // camera" when the near plane sits millimetres from the origin).
        seg(near_c[i], far_c[i], FRUSTUM_COLOR_FAR);
        seg(apex, near_c[i], FRUSTUM_COLOR);
    }

    // Up-triangle above the near plane's top edge (near_c[3] .. near_c[2]).
    let top_mid = (near_c[2] + near_c[3]) * 0.5;
    let up = (snap.world.transform_vector3(Vec3::Y)).normalize_or_zero();
    let edge = (near_c[2] - near_c[3]).length();
    let tip = top_mid + up * edge * 0.35;
    let base_half = (near_c[2] - near_c[3]) * 0.25;
    seg(top_mid - base_half, top_mid + base_half, FRUSTUM_COLOR);
    seg(top_mid - base_half, tip, FRUSTUM_COLOR);
    seg(top_mid + base_half, tip, FRUSTUM_COLOR);
}

/// Per-frame: rebuild the frustum overlay from the live camera nodes. Called
/// from the render loop alongside the other overlays.
pub fn per_frame_update(renderer: &mut AwsmRenderer) {
    let enabled = crate::controller::controller().settings.camera_gizmos.get();
    // Looking THROUGH a camera → skip that camera's own frustum.
    let active = crate::controller::controller().active_camera.get();

    SCRATCH.with(|scratch| {
        let scratch = &mut *scratch.borrow_mut();
        scratch.clear();

        if enabled {
            // Phase 1: snapshot candidates under the nodes lock, then release
            // it — `effective_visible` walks ancestors and takes the same lock
            // internally (holding it here would deadlock).
            {
                let b = bridge();
                let nodes = b.nodes.lock().unwrap();
                for (id, entry) in nodes.iter() {
                    if Some(*id) == active {
                        continue;
                    }
                    let cfg = match entry.node.kind.get_cloned() {
                        NodeKind::Camera(c) => c,
                        _ => continue,
                    };
                    let Ok(world) = renderer.transforms.get_world(entry.transform_key) else {
                        continue;
                    };
                    // Same param source as the render loop's scene-camera path:
                    // the animatable store slot when materialized, else the config.
                    let camera_key = *entry.camera_key.lock().unwrap();
                    let params = match camera_key.and_then(|key| renderer.cameras.get(key)) {
                        Some(p) => *p,
                        None => awsm_renderer_scene_loader::camera::camera_params_from_config(&cfg),
                    };
                    scratch.cameras.push((
                        *id,
                        CameraSnapshot {
                            world: *world,
                            projection: params.projection,
                            near: params.near,
                            far: params.far,
                        },
                    ));
                }
            }
            // Phase 2: a hidden camera (or one inside a hidden group) draws no
            // gizmo, matching how lights contribute nothing when hidden.
            scratch
                .cameras
                .retain(|(id, _)| crate::engine::bridge::node_sync::effective_visible(*id));
        }

        if !scratch.cameras.is_empty() {
            // Stable order (the bridge map iterates in hash order).
            scratch.cameras.sort_by_key(|(id, _)| id.0);
            // A scene camera renders at the live surface aspect — that is the
            // frustum it would actually cover.
            let (w, h) = renderer.gpu.canvas_size(false);
            let aspect = if h > 0.0 { (w / h) as f32 } else { 1.0 };
            let Scratch {
                cameras,
                positions,
                colors,
            } = scratch;
            for (_, snap) in cameras.iter() {
                append_frustum(snap, aspect, positions, colors);
            }
        }

        // Update the ONE persistent overlay line in place (GPU buffer reused;
        // see skeleton_viz's day-3 churn fix for why not remove+add).
        let key = FRUSTA.with(|c| c.get());
        if scratch.positions.is_empty() {
            if let Some(key) = key {
                renderer.remove_line(key);
                FRUSTA.with(|c| c.set(None));
            }
            return;
        }
        match key {
            Some(key) if renderer.has_line(key) => {
                if let Err(err) =
                    renderer.update_line_segments(key, &scratch.positions, &scratch.colors)
                {
                    tracing::warn!("camera_gizmos: update_line_segments failed: {err}");
                }
            }
            _ => match renderer.add_line_segments(
                &scratch.positions,
                &scratch.colors,
                FRUSTUM_WIDTH,
                true,
            ) {
                Ok(key) => FRUSTA.with(|c| c.set(key)),
                Err(err) => tracing::warn!("camera_gizmos: add_line_segments failed: {err}"),
            },
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(
        projection: CameraProjectionParams,
        near: f32,
        far: f32,
        world: Mat4,
    ) -> CameraSnapshot {
        CameraSnapshot {
            world,
            projection,
            near,
            far,
        }
    }

    /// Perspective: the near rectangle's half-height must be tan(fov/2)·near
    /// and the half-width must follow the aspect — checked in world space
    /// through a non-identity transform.
    #[test]
    fn perspective_rect_scales_with_fov_and_aspect() {
        let world = Mat4::from_translation(Vec3::new(3.0, 2.0, 1.0));
        let fov: f32 = 1.0;
        let (aspect, dist) = (2.0, 4.0);
        let mut c = [Vec3::ZERO; 4];
        rect_corners(
            &world,
            &CameraProjectionParams::Perspective { fov_y_rad: fov },
            aspect,
            dist,
            &mut c,
        );
        let half_h = (fov * 0.5).tan() * dist;
        let expect_tr = Vec3::new(3.0 + half_h * aspect, 2.0 + half_h, 1.0 - dist);
        assert!(
            c[2].abs_diff_eq(expect_tr, 1e-5),
            "top-right {:?} != {expect_tr:?}",
            c[2]
        );
    }

    /// Orthographic: the rectangle is depth-independent (same half-extents at
    /// near and far — a box, not a pyramid).
    #[test]
    fn orthographic_rect_is_depth_independent() {
        let proj = CameraProjectionParams::Orthographic { half_height: 2.0 };
        let (mut a, mut b) = ([Vec3::ZERO; 4], [Vec3::ZERO; 4]);
        rect_corners(&Mat4::IDENTITY, &proj, 1.5, 1.0, &mut a);
        rect_corners(&Mat4::IDENTITY, &proj, 1.5, 10.0, &mut b);
        for (pa, pb) in a.iter().zip(b.iter()) {
            assert!((pa.x - pb.x).abs() < 1e-6 && (pa.y - pb.y).abs() < 1e-6);
        }
        assert!((a[2].y - 2.0).abs() < 1e-6 && (a[2].x - 3.0).abs() < 1e-6);
    }

    /// The far plane draws display-clamped; a kilometre-far camera must not
    /// produce kilometre-long gizmo lines.
    #[test]
    fn far_plane_is_display_clamped() {
        let s = snap(
            CameraProjectionParams::Perspective { fov_y_rad: 1.0 },
            0.1,
            5000.0,
            Mat4::IDENTITY,
        );
        let (mut positions, mut colors) = (Vec::new(), Vec::new());
        append_frustum(&s, 1.0, &mut positions, &mut colors);
        assert_eq!(positions.len(), colors.len());
        let max_dist = positions.iter().map(|p| p.length()).fold(0.0f32, f32::max);
        // Far corners sit at |z| = FAR_DISPLAY_CAP (plus half-extents).
        assert!(
            max_dist < FAR_DISPLAY_CAP * 2.0,
            "display frustum leaked past the cap: {max_dist}"
        );
        assert!(
            max_dist >= FAR_DISPLAY_CAP,
            "far plane should reach the cap for a distant far: {max_dist}"
        );
    }
}
