//! On-canvas **transform gizmo** — procedurally generated and analytically
//! picked.
//!
//! The gizmo is drawn entirely as **fat lines** (always-on-top, screen-constant
//! size): three translate arrows, three plane handles, three rotation rings, and
//! three scale handles. Nothing is loaded from a `.glb` — the geometry is rebuilt
//! every frame from the selected object's world transform, so look and proportions
//! are fully controlled in code.
//!
//! Crucially, **picking is analytic and decoupled from the rendered thickness**:
//! on grab we cast a camera ray and test it against the *ideal* shapes (axis
//! segments, plane quads, ring circles, a scale tip-box) with a generous
//! screen-space tolerance band ([`TOLERANCE_PX`]). A thin visual line still has a
//! fat clickable region — the way Blender/Unity/Maya gizmos behave — so a handle
//! is easy to grab even when the cursor is several pixels off the line.
//!
//! The drag math (axis/plane translate, axis scale, axis rotate via ray↔plane
//! intersection) is unchanged from the original controller.

use anyhow::Result;
use awsm_renderer::{
    camera::CameraMatrices,
    error::Result as RendererResult,
    render_passes::lines::LineKey,
    transforms::{Transform, TransformKey},
    AwsmRenderer,
};
use glam::{Mat4, Quat, Vec3, Vec4};

// ── tuning ──────────────────────────────────────────────────────────────────

/// On-screen radius of the gizmo, in CSS pixels (the axis/ring extent).
const DESIRED_PIXEL_SIZE: f32 = 100.0;
/// Line width, in CSS pixels.
const LINE_WIDTH_PX: f32 = 2.5;
/// How far (in CSS pixels) the cursor can be from a handle's ideal line and
/// still grab it. This is what makes the thin gizmo easy to grab.
const TOLERANCE_PX: f32 = 11.0;

/// Local-space dimensions (multiplied by the per-frame screen scale).
const AXIS_LEN: f32 = 1.0;
const ARROW_HEAD_LEN: f32 = 0.16;
const ARROW_HEAD_HALF: f32 = 0.06;
const RING_RADIUS: f32 = 0.95;
const RING_SEGMENTS: usize = 56;
const PLANE_OFFSET: f32 = 0.28;
const PLANE_SIZE: f32 = 0.26;
const SCALE_LEN: f32 = 0.84;
const SCALE_BOX_HALF: f32 = 0.07;

const AXES: [Vec3; 3] = [Vec3::X, Vec3::Y, Vec3::Z];
const COLOR_X: Vec4 = Vec4::new(0.92, 0.26, 0.30, 1.0);
const COLOR_Y: Vec4 = Vec4::new(0.46, 0.80, 0.30, 1.0);
const COLOR_Z: Vec4 = Vec4::new(0.30, 0.55, 0.96, 1.0);
const COLOR_HILITE: Vec4 = Vec4::new(1.0, 0.84, 0.20, 1.0);
const AXIS_COLORS: [Vec4; 3] = [COLOR_X, COLOR_Y, COLOR_Z];

// ── public types ────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct TransformController {
    pub selected_object: Option<TransformObject>,
    space: GizmoSpace,
    lines: GizmoLines,
    /// Live placement (origin / orientation / screen-scale), recomputed each
    /// frame in [`Self::zoom_gizmo_transforms`]. Picking reads this.
    placement: Option<Placement>,
    /// Which manipulation modes are currently shown (and thus grabbable).
    visible: ModeVisibility,
    /// Handle under the cursor (highlight only) and the grabbed handle.
    hover: Option<GizmoKind>,
    active: Option<GizmoKind>,
    current_gizmo_kind: Option<GizmoKind>,
    drag_state: Option<DragState>,
}

#[derive(Clone, Debug, Copy, PartialEq, Eq, Hash)]
pub struct TransformObject {
    pub key: TransformKey,
    pub instance: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GizmoKind {
    TranslationX,
    TranslationY,
    TranslationZ,
    /// Plane-translate handles, keyed by their *normal* axis.
    PlaneX,
    PlaneY,
    PlaneZ,
    RotationX,
    RotationY,
    RotationZ,
    ScaleX,
    ScaleY,
    ScaleZ,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GizmoSpace {
    Local,
    #[default]
    Global,
}

// ── internal ────────────────────────────────────────────────────────────────

/// The 12 line resources. Translate arrows + scale handles are segment-topology
/// (disjoint pairs); plane squares + rings are strips (polylines).
#[derive(Clone, Debug)]
struct GizmoLines {
    translate: [LineKey; 3],
    plane: [LineKey; 3],
    ring: [LineKey; 3],
    scale: [LineKey; 3],
}

#[derive(Clone, Copy, Debug)]
struct Placement {
    origin: Vec3,
    orientation: Quat,
    scale: f32,
    world_per_px: f32,
    /// Cached camera unprojection + viewport so analytic picking (grab + hover)
    /// can build a camera ray WITHOUT touching the renderer — hover can then run
    /// on every pointer-move with no renderer lock.
    inv_view_proj: Mat4,
    viewport: (f32, f32),
}

#[derive(Clone, Copy, Debug, Default)]
struct ModeVisibility {
    translation: bool,
    rotation: bool,
    scale: bool,
}

/// State tracked during a drag (unchanged math from the original controller).
#[derive(Clone, Debug)]
struct DragState {
    screen_pos: (f32, f32),
    initial_translation: Vec3,
    initial_scale: Vec3,
    initial_rotation: Quat,
    initial_world_position: Vec3,
    world_axis: Vec3,
    parent_inverse_rotation: Quat,
    plane_normal: Vec3,
    plane_point: Vec3,
    initial_intersection: Vec3,
    initial_angle: f32,
    scale_target_axis: u8,
}

impl TransformController {
    /// Build the gizmo's line resources. No asset load — geometry is generated
    /// procedurally and redrawn each frame.
    pub fn new(renderer: &mut AwsmRenderer, gizmo_space: GizmoSpace) -> Result<Self> {
        // Seed each line with a tiny placeholder so a `LineKey` is allocated
        // (`add_line_*` returns `None` for empty input); real geometry is written
        // by `zoom_gizmo_transforms`, and everything starts hidden.
        let lines = GizmoLines {
            translate: [
                seed_line(renderer, false)?,
                seed_line(renderer, false)?,
                seed_line(renderer, false)?,
            ],
            plane: [
                seed_line(renderer, true)?,
                seed_line(renderer, true)?,
                seed_line(renderer, true)?,
            ],
            ring: [
                seed_line(renderer, true)?,
                seed_line(renderer, true)?,
                seed_line(renderer, true)?,
            ],
            scale: [
                seed_line(renderer, false)?,
                seed_line(renderer, false)?,
                seed_line(renderer, false)?,
            ],
        };
        let me = Self {
            selected_object: None,
            space: gizmo_space,
            lines,
            placement: None,
            visible: ModeVisibility::default(),
            hover: None,
            active: None,
            current_gizmo_kind: None,
            drag_state: None,
        };
        me.hide_all(renderer);
        Ok(me)
    }

    /// Set which modes are shown (and thus grabbable). Cheap — the actual redraw
    /// happens in [`Self::zoom_gizmo_transforms`], called every frame. Fully
    /// hidden (all three) clears the drawn lines IMMEDIATELY: callers that
    /// force-hide (e.g. the editor's "Show gizmo" toggle) return before the
    /// per-frame redraw, so without this the last-drawn handles stayed on
    /// screen forever.
    pub fn set_hidden(
        &mut self,
        renderer: &mut AwsmRenderer,
        translation_hidden: bool,
        rotation_hidden: bool,
        scale_hidden: bool,
    ) -> Result<()> {
        self.visible = ModeVisibility {
            translation: !translation_hidden,
            rotation: !rotation_hidden,
            scale: !scale_hidden,
        };
        if translation_hidden && rotation_hidden && scale_hidden {
            self.hide_all(renderer);
        }
        Ok(())
    }

    /// Switch local/global space.
    pub fn set_space(&mut self, _renderer: &mut AwsmRenderer, space: GizmoSpace) {
        self.space = space;
    }

    /// Update the hovered handle for highlighting (call on pointer-move when not
    /// dragging). Renderer-free — picks against the cached placement — so it's
    /// cheap to call on every move. Returns whether a handle is hovered.
    pub fn update_hover(&mut self, x: i32, y: i32) -> bool {
        let hover = self.pick(x, y);
        self.hover = hover;
        hover.is_some()
    }

    pub fn clear_hover(&mut self) {
        self.hover = None;
    }

    /// Try to grab a handle at `(x, y)`. On success, begins a drag and returns
    /// the grabbed handle; route subsequent `update_transform` calls to it and
    /// `end_drag` on release. Returns `None` if no handle is under the cursor
    /// (the caller should then run its own object pick).
    pub fn try_grab(&mut self, renderer: &mut AwsmRenderer, x: i32, y: i32) -> Option<GizmoKind> {
        let kind = self.pick(x, y)?;
        self.begin_drag(renderer, kind, x, y);
        self.active = Some(kind);
        self.hover = Some(kind);
        Some(kind)
    }

    /// End the current drag (call on pointer-up / cancel).
    pub fn end_drag(&mut self) {
        self.drag_state = None;
        self.current_gizmo_kind = None;
        self.active = None;
    }

    /// Whether a handle is currently grabbed.
    pub fn is_dragging(&self) -> bool {
        self.drag_state.is_some()
    }

    // ── per-frame placement + redraw ─────────────────────────────────────────

    /// Re-anchor the gizmo to the selected object, recompute its screen-constant
    /// scale, and redraw every visible handle. Call once per frame (kept the
    /// historical name so both frontends' render loops need no change).
    pub fn zoom_gizmo_transforms(
        &mut self,
        renderer: &mut AwsmRenderer,
        camera_matrices: &CameraMatrices,
    ) -> RendererResult<()> {
        let Some(selected_object) = self.selected_object else {
            self.placement = None;
            self.hide_all(renderer);
            return Ok(());
        };
        let Some(world_matrix) = get_world_matrix(renderer, selected_object) else {
            self.placement = None;
            self.hide_all(renderer);
            return Ok(());
        };

        let (_, world_rotation, world_position) = world_matrix.to_scale_rotation_translation();
        let orientation = match self.space {
            GizmoSpace::Global => Quat::IDENTITY,
            GizmoSpace::Local => world_rotation,
        };

        let (viewport_x, viewport_y) = renderer.gpu.canvas_size(false);
        let desired_ndc = 2.0 * DESIRED_PIXEL_SIZE / viewport_y as f32;
        let proj11 = camera_matrices.projection.y_axis.y;
        let depth = if camera_matrices.is_orthographic() {
            1.0
        } else {
            (world_position - camera_matrices.position_world).length()
        };
        // World units that the reference size (1.0) spans on screen == DESIRED_PIXEL_SIZE px.
        let scale = (desired_ndc * depth / proj11).max(1e-6);
        let world_per_px = scale / DESIRED_PIXEL_SIZE;

        self.placement = Some(Placement {
            origin: world_position,
            orientation,
            scale,
            world_per_px,
            inv_view_proj: camera_matrices.inv_view_projection(),
            viewport: (viewport_x as f32, viewport_y as f32),
        });

        self.redraw(renderer);
        Ok(())
    }

    fn redraw(&mut self, renderer: &mut AwsmRenderer) {
        let Some(p) = self.placement else {
            self.hide_all(renderer);
            return;
        };
        for i in 0..3 {
            let dir = (p.orientation * AXES[i]).normalize();
            let (u, v) = perp_basis(dir);

            // Translate arrow (segments)
            if self.visible.translation {
                let col = self.color_for(translation_kind(i));
                let pts = arrow_segments(p.origin, dir, u, p.scale);
                update_segments(renderer, self.lines.translate[i], &pts, col);
            } else {
                clear_line(renderer, self.lines.translate[i]);
            }

            // Plane square (strip): in-plane axes are the OTHER two axes.
            if self.visible.translation {
                let a = (p.orientation * AXES[(i + 1) % 3]).normalize();
                let b = (p.orientation * AXES[(i + 2) % 3]).normalize();
                let col = self.color_for(plane_kind(i));
                let pts = plane_strip(p.origin, a, b, p.scale);
                update_strip(renderer, self.lines.plane[i], &pts, col);
            } else {
                clear_line(renderer, self.lines.plane[i]);
            }

            // Rotation ring (strip)
            if self.visible.rotation {
                let col = self.color_for(rotation_kind(i));
                let pts = ring_strip(p.origin, u, v, p.scale);
                update_strip(renderer, self.lines.ring[i], &pts, col);
            } else {
                clear_line(renderer, self.lines.ring[i]);
            }

            // Scale handle (segments)
            if self.visible.scale {
                let col = self.color_for(scale_kind(i));
                let pts = scale_segments(p.origin, dir, u, v, p.scale);
                update_segments(renderer, self.lines.scale[i], &pts, col);
            } else {
                clear_line(renderer, self.lines.scale[i]);
            }
        }
    }

    fn hide_all(&self, renderer: &mut AwsmRenderer) {
        for key in self
            .lines
            .translate
            .iter()
            .chain(self.lines.plane.iter())
            .chain(self.lines.ring.iter())
            .chain(self.lines.scale.iter())
        {
            clear_line(renderer, *key);
        }
    }

    fn color_for(&self, kind: GizmoKind) -> Vec4 {
        if self.active == Some(kind) || self.hover == Some(kind) {
            COLOR_HILITE
        } else {
            AXIS_COLORS[axis_index(kind)]
        }
    }

    // ── analytic picking ─────────────────────────────────────────────────────

    /// Ray-cast the cursor against the visible handles' ideal shapes (with a
    /// screen-space tolerance band) and return the nearest hit handle. Uses the
    /// cached placement (camera unprojection + viewport), so it needs no renderer.
    fn pick(&self, x: i32, y: i32) -> Option<GizmoKind> {
        let p = self.placement?;
        let (w, h) = p.viewport;
        let (ro, rd) = ray_from_screen(x as f32, y as f32, w, h, p.inv_view_proj);
        let tol = (p.world_per_px * TOLERANCE_PX).max(1e-5);

        let mut best: Option<(f32, GizmoKind)> = None;
        let mut consider = |t: f32, kind: GizmoKind| {
            if t > 0.0 && best.map(|(bt, _)| t < bt).unwrap_or(true) {
                best = Some((t, kind));
            }
        };

        // Scale shafts are only a full grab target when scale is the *only*
        // axis mode shown; in universal mode (translation also visible) the
        // shaft belongs to translate and scale is grabbed by its tip-box.
        let scale_shaft = self.visible.scale && !self.visible.translation;

        for i in 0..3 {
            let dir = (p.orientation * AXES[i]).normalize();

            if self.visible.translation {
                let tip = p.origin + dir * (AXIS_LEN * p.scale);
                if let Some(t) = ray_segment_hit(ro, rd, p.origin, tip, tol) {
                    consider(t, translation_kind(i));
                }
                // Plane quad (in the other two axes).
                let a = (p.orientation * AXES[(i + 1) % 3]).normalize();
                let b = (p.orientation * AXES[(i + 2) % 3]).normalize();
                if let Some(t) = ray_plane_quad_hit(ro, rd, p.origin, dir, a, b, p.scale, tol) {
                    consider(t, plane_kind(i));
                }
            }

            if self.visible.rotation {
                if let Some(t) = ray_ring_hit(ro, rd, p.origin, dir, RING_RADIUS * p.scale, tol) {
                    consider(t, rotation_kind(i));
                }
            }

            if self.visible.scale {
                let tip = p.origin + dir * (SCALE_LEN * p.scale);
                // Tip box: a generous sphere around the scale handle's box.
                if let Some(t) = ray_sphere_hit(ro, rd, tip, SCALE_BOX_HALF * p.scale + tol) {
                    consider(t, scale_kind(i));
                }
                if scale_shaft {
                    if let Some(t) = ray_segment_hit(ro, rd, p.origin, tip, tol) {
                        consider(t, scale_kind(i));
                    }
                }
            }
        }

        best.map(|(_, k)| k)
    }

    // ── drag setup (refactored out of the old `start_pick`) ──────────────────

    fn begin_drag(&mut self, renderer: &mut AwsmRenderer, gizmo_kind: GizmoKind, x: i32, y: i32) {
        self.current_gizmo_kind = Some(gizmo_kind);
        self.drag_state = None;

        let Some(selected_object) = self.selected_object else {
            return;
        };
        let (Some(selected_transform), Some(world_matrix), Some(camera_matrices)) = (
            get_local_transform(renderer, selected_object),
            get_world_matrix(renderer, selected_object),
            renderer.camera.last_matrices.as_ref(),
        ) else {
            return;
        };

        let (_world_scale, world_rotation, world_position) =
            world_matrix.to_scale_rotation_translation();
        let camera_pos = camera_matrices.position_world;

        let parent_inverse_rotation =
            if let Some(parent_key) = get_parent_key(renderer, selected_object) {
                if let Ok(parent_world) = renderer.transforms.get_world(parent_key) {
                    let (_, parent_rot, _) = parent_world.to_scale_rotation_translation();
                    parent_rot.inverse()
                } else {
                    Quat::IDENTITY
                }
            } else {
                Quat::IDENTITY
            };

        // Local axis (or plane normal axis) this handle is bound to.
        let local_axis = axis_vec(gizmo_kind);
        let world_axis = match self.space {
            GizmoSpace::Global => local_axis,
            GizmoSpace::Local => world_rotation * local_axis,
        };

        let scale_target_axis = match gizmo_kind {
            GizmoKind::ScaleX | GizmoKind::ScaleY | GizmoKind::ScaleZ => match self.space {
                GizmoSpace::Global => {
                    let lx = world_rotation * Vec3::X;
                    let ly = world_rotation * Vec3::Y;
                    let lz = world_rotation * Vec3::Z;
                    let dx = world_axis.dot(lx).abs();
                    let dy = world_axis.dot(ly).abs();
                    let dz = world_axis.dot(lz).abs();
                    if dx >= dy && dx >= dz {
                        0
                    } else if dy >= dx && dy >= dz {
                        1
                    } else {
                        2
                    }
                }
                GizmoSpace::Local => match gizmo_kind {
                    GizmoKind::ScaleX => 0,
                    GizmoKind::ScaleY => 1,
                    _ => 2,
                },
            },
            _ => 0,
        };

        // Drag plane: for axis translate/scale a plane containing the axis and
        // facing the camera; for plane-translate / rotation the handle's own plane.
        let plane_normal = if is_plane(gizmo_kind) || is_rotation(gizmo_kind) {
            world_axis
        } else {
            let to_camera = (camera_pos - world_position).normalize();
            let normal = (to_camera - world_axis * to_camera.dot(world_axis)).normalize();
            if normal.length_squared() < 0.001 {
                if world_axis.dot(Vec3::Y).abs() < 0.9 {
                    (Vec3::Y - world_axis * Vec3::Y.dot(world_axis)).normalize()
                } else {
                    (Vec3::X - world_axis * Vec3::X.dot(world_axis)).normalize()
                }
            } else {
                normal
            }
        };

        let (width, height) = renderer.gpu.canvas_size(false);
        let Some(intersection) = ray_plane_intersection(
            x as f32,
            y as f32,
            width as f32,
            height as f32,
            camera_matrices,
            world_position,
            plane_normal,
        ) else {
            return;
        };

        let initial_angle = if is_rotation(gizmo_kind) {
            let from_center = intersection - world_position;
            let (basis_u, basis_v) = get_rotation_plane_basis(world_axis);
            from_center.dot(basis_v).atan2(from_center.dot(basis_u))
        } else {
            0.0
        };

        self.drag_state = Some(DragState {
            screen_pos: (x as f32, y as f32),
            initial_translation: selected_transform.translation,
            initial_scale: selected_transform.scale,
            initial_rotation: selected_transform.rotation,
            initial_world_position: world_position,
            world_axis,
            parent_inverse_rotation,
            plane_normal,
            plane_point: world_position,
            initial_intersection: intersection,
            initial_angle,
            scale_target_axis,
        });
    }

    /// Apply a pointer-move delta to the in-flight drag (unchanged math, with an
    /// added plane-translate branch).
    pub fn update_transform(&mut self, renderer: &mut AwsmRenderer, x_delta: i32, y_delta: i32) {
        let Some(drag_state) = self.drag_state.as_mut() else {
            return;
        };
        let Some(selected_object) = self.selected_object else {
            return;
        };
        let Some(gizmo_kind) = self.current_gizmo_kind else {
            return;
        };
        let Some(camera_matrices) = renderer.camera.last_matrices.as_ref() else {
            return;
        };

        drag_state.screen_pos.0 += x_delta as f32;
        drag_state.screen_pos.1 += y_delta as f32;

        let (width, height) = renderer.gpu.canvas_size(false);
        let Some(current_intersection) = ray_plane_intersection(
            drag_state.screen_pos.0,
            drag_state.screen_pos.1,
            width as f32,
            height as f32,
            camera_matrices,
            drag_state.plane_point,
            drag_state.plane_normal,
        ) else {
            return;
        };

        let Some(mut selected_transform) = get_local_transform(renderer, selected_object) else {
            return;
        };
        let world_axis = drag_state.world_axis;

        match gizmo_kind {
            GizmoKind::TranslationX | GizmoKind::TranslationY | GizmoKind::TranslationZ => {
                let world_delta = current_intersection - drag_state.initial_intersection;
                let movement_along_axis = world_delta.dot(world_axis);
                let world_translation_delta = world_axis * movement_along_axis;
                let parent_space_delta =
                    drag_state.parent_inverse_rotation * world_translation_delta;
                selected_transform.translation =
                    drag_state.initial_translation + parent_space_delta;
            }
            GizmoKind::PlaneX | GizmoKind::PlaneY | GizmoKind::PlaneZ => {
                // Plane translate: the whole in-plane delta (the intersection is
                // already constrained to the handle's plane).
                let world_delta = current_intersection - drag_state.initial_intersection;
                let parent_space_delta = drag_state.parent_inverse_rotation * world_delta;
                selected_transform.translation =
                    drag_state.initial_translation + parent_space_delta;
            }
            GizmoKind::ScaleX | GizmoKind::ScaleY | GizmoKind::ScaleZ => {
                let initial_offset =
                    drag_state.initial_intersection - drag_state.initial_world_position;
                let current_offset = current_intersection - drag_state.initial_world_position;
                let initial_dist = initial_offset.dot(world_axis);
                let current_dist = current_offset.dot(world_axis);
                let scale_factor = if initial_dist.abs() > 0.001 {
                    (current_dist / initial_dist).max(0.01)
                } else {
                    let camera_distance = (camera_matrices.position_world
                        - drag_state.initial_world_position)
                        .length();
                    let sensitivity = camera_distance * 0.5;
                    (1.0 + current_dist / sensitivity).max(0.01)
                };
                let mut new_scale = drag_state.initial_scale;
                match drag_state.scale_target_axis {
                    0 => new_scale.x = drag_state.initial_scale.x * scale_factor,
                    1 => new_scale.y = drag_state.initial_scale.y * scale_factor,
                    _ => new_scale.z = drag_state.initial_scale.z * scale_factor,
                }
                selected_transform.scale = new_scale;
            }
            GizmoKind::RotationX | GizmoKind::RotationY | GizmoKind::RotationZ => {
                let from_center = current_intersection - drag_state.plane_point;
                let (basis_u, basis_v) = get_rotation_plane_basis(world_axis);
                let current_angle = from_center.dot(basis_v).atan2(from_center.dot(basis_u));
                let angle_delta = current_angle - drag_state.initial_angle;
                let parent_space_axis = drag_state.parent_inverse_rotation * world_axis;
                let rotation_delta = Quat::from_axis_angle(parent_space_axis, angle_delta);
                selected_transform.rotation =
                    (rotation_delta * drag_state.initial_rotation).normalize();
            }
        }

        let _ = set_local_transform(renderer, selected_object, selected_transform);
        renderer.update_transforms();
    }
}

// ── geometry builders (local → world) ─────────────────────────────────────────

/// A translate arrow: shaft + a 2-segment chevron head. Segment-pair topology.
fn arrow_segments(origin: Vec3, dir: Vec3, perp: Vec3, scale: f32) -> Vec<Vec3> {
    let tip = origin + dir * (AXIS_LEN * scale);
    let back = tip - dir * (ARROW_HEAD_LEN * scale);
    let off = perp * (ARROW_HEAD_HALF * scale);
    vec![origin, tip, tip, back + off, tip, back - off]
}

/// A scale handle: shaft + a small square box outline at the tip (segments).
fn scale_segments(origin: Vec3, dir: Vec3, u: Vec3, v: Vec3, scale: f32) -> Vec<Vec3> {
    let tip = origin + dir * (SCALE_LEN * scale);
    let h = SCALE_BOX_HALF * scale;
    let c00 = tip + u * h + v * h;
    let c10 = tip - u * h + v * h;
    let c11 = tip - u * h - v * h;
    let c01 = tip + u * h - v * h;
    vec![
        origin, tip, // shaft
        c00, c10, c10, c11, c11, c01, c01, c00, // box outline
    ]
}

/// A plane-translate handle: a small square in the (a, b) plane (closed strip).
fn plane_strip(origin: Vec3, a: Vec3, b: Vec3, scale: f32) -> Vec<Vec3> {
    let o = PLANE_OFFSET * scale;
    let s = PLANE_SIZE * scale;
    let p00 = origin + a * o + b * o;
    let p10 = origin + a * (o + s) + b * o;
    let p11 = origin + a * (o + s) + b * (o + s);
    let p01 = origin + a * o + b * (o + s);
    vec![p00, p10, p11, p01, p00]
}

/// A rotation ring: a circle of radius `r` in the (u, v) plane (closed strip).
fn ring_strip(origin: Vec3, u: Vec3, v: Vec3, scale: f32) -> Vec<Vec3> {
    let r = RING_RADIUS * scale;
    let mut pts = Vec::with_capacity(RING_SEGMENTS + 1);
    for k in 0..=RING_SEGMENTS {
        let a = (k as f32 / RING_SEGMENTS as f32) * std::f32::consts::TAU;
        pts.push(origin + (u * a.cos() + v * a.sin()) * r);
    }
    pts
}

/// Allocate one gizmo line (strip or segments) with a tiny placeholder so a
/// `LineKey` exists; real geometry is written each frame by `redraw`.
fn seed_line(renderer: &mut AwsmRenderer, strip: bool) -> Result<LineKey> {
    let pos = [Vec3::ZERO, Vec3::X * 0.001];
    let col = [COLOR_X, COLOR_X];
    let res = if strip {
        renderer.add_line_strip(&pos, &col, LINE_WIDTH_PX, true)
    } else {
        renderer.add_line_segments(&pos, &col, LINE_WIDTH_PX, true)
    };
    res.map_err(|e| anyhow::anyhow!("gizmo line: {e}"))?
        .ok_or_else(|| anyhow::anyhow!("gizmo line: no key"))
}

fn update_segments(renderer: &mut AwsmRenderer, key: LineKey, pts: &[Vec3], color: Vec4) {
    let colors = vec![color; pts.len()];
    let _ = renderer.update_line_segments(key, pts, &colors);
}

fn update_strip(renderer: &mut AwsmRenderer, key: LineKey, pts: &[Vec3], color: Vec4) {
    let colors = vec![color; pts.len()];
    let _ = renderer.update_line_strip(key, pts, &colors);
}

fn clear_line(renderer: &mut AwsmRenderer, key: LineKey) {
    let _ = renderer.update_line_strip(key, &[], &[]);
}

// ── kind helpers ──────────────────────────────────────────────────────────────

fn translation_kind(i: usize) -> GizmoKind {
    [
        GizmoKind::TranslationX,
        GizmoKind::TranslationY,
        GizmoKind::TranslationZ,
    ][i]
}
fn plane_kind(i: usize) -> GizmoKind {
    [GizmoKind::PlaneX, GizmoKind::PlaneY, GizmoKind::PlaneZ][i]
}
fn rotation_kind(i: usize) -> GizmoKind {
    [
        GizmoKind::RotationX,
        GizmoKind::RotationY,
        GizmoKind::RotationZ,
    ][i]
}
fn scale_kind(i: usize) -> GizmoKind {
    [GizmoKind::ScaleX, GizmoKind::ScaleY, GizmoKind::ScaleZ][i]
}

fn axis_index(kind: GizmoKind) -> usize {
    match kind {
        GizmoKind::TranslationX | GizmoKind::PlaneX | GizmoKind::RotationX | GizmoKind::ScaleX => 0,
        GizmoKind::TranslationY | GizmoKind::PlaneY | GizmoKind::RotationY | GizmoKind::ScaleY => 1,
        _ => 2,
    }
}

fn axis_vec(kind: GizmoKind) -> Vec3 {
    AXES[axis_index(kind)]
}

fn is_plane(kind: GizmoKind) -> bool {
    matches!(
        kind,
        GizmoKind::PlaneX | GizmoKind::PlaneY | GizmoKind::PlaneZ
    )
}

fn is_rotation(kind: GizmoKind) -> bool {
    matches!(
        kind,
        GizmoKind::RotationX | GizmoKind::RotationY | GizmoKind::RotationZ
    )
}

// ── ray / intersection math ───────────────────────────────────────────────────

/// Build a world-space camera ray (origin, normalized direction) for a screen
/// point, given the camera's inverse view-projection matrix.
fn ray_from_screen(sx: f32, sy: f32, vw: f32, vh: f32, inv_view_proj: Mat4) -> (Vec3, Vec3) {
    let ndc_x = (2.0 * sx / vw) - 1.0;
    let ndc_y = 1.0 - (2.0 * sy / vh);
    let near = inv_view_proj * Vec4::new(ndc_x, ndc_y, 0.0, 1.0);
    let far = inv_view_proj * Vec4::new(ndc_x, ndc_y, 1.0, 1.0);
    let near = near.truncate() / near.w;
    let far = far.truncate() / far.w;
    (near, (far - near).normalize())
}

/// Closest approach between a ray and a segment. Returns the ray parameter `t`
/// (distance along the ray) if within `tol`, else `None`.
fn ray_segment_hit(ro: Vec3, rd: Vec3, a: Vec3, b: Vec3, tol: f32) -> Option<f32> {
    let d2 = b - a;
    let r = ro - a;
    let aa = rd.dot(rd);
    let e = d2.dot(d2);
    let f = d2.dot(r);
    if e < 1e-8 {
        return None;
    }
    let c = rd.dot(r);
    let bb = rd.dot(d2);
    let denom = aa * e - bb * bb;
    let mut t = if denom.abs() > 1e-8 {
        (bb * f - c * e) / denom
    } else {
        0.0
    };
    let mut s = (bb * t + f) / e;
    s = s.clamp(0.0, 1.0);
    t = ((d2.dot(rd)) * s - c) / aa;
    if t < 0.0 {
        t = 0.0;
    }
    let p_ray = ro + rd * t;
    let q_seg = a + d2 * s;
    if (p_ray - q_seg).length() <= tol {
        Some(t)
    } else {
        None
    }
}

/// Ray vs a ring (circle of `radius` centered at `center` in the plane with the
/// given `normal`). Returns ray `t` at the ring plane if the hit is within `tol`
/// of the circle, else `None`.
fn ray_ring_hit(
    ro: Vec3,
    rd: Vec3,
    center: Vec3,
    normal: Vec3,
    radius: f32,
    tol: f32,
) -> Option<f32> {
    let denom = rd.dot(normal);
    if denom.abs() < 1e-6 {
        return None;
    }
    let t = (center - ro).dot(normal) / denom;
    if t < 0.0 {
        return None;
    }
    let p = ro + rd * t;
    let dist = (p - center).length();
    if (dist - radius).abs() <= tol {
        Some(t)
    } else {
        None
    }
}

/// Ray vs the plane-translate quad (offset square in the `a`/`b` plane).
#[allow(clippy::too_many_arguments)]
fn ray_plane_quad_hit(
    ro: Vec3,
    rd: Vec3,
    origin: Vec3,
    normal: Vec3,
    a: Vec3,
    b: Vec3,
    scale: f32,
    tol: f32,
) -> Option<f32> {
    let denom = rd.dot(normal);
    if denom.abs() < 1e-6 {
        return None;
    }
    let t = (origin - ro).dot(normal) / denom;
    if t < 0.0 {
        return None;
    }
    let p = ro + rd * t;
    let local = p - origin;
    let u = local.dot(a) / scale;
    let v = local.dot(b) / scale;
    let tol_l = tol / scale;
    let lo = PLANE_OFFSET - tol_l;
    let hi = PLANE_OFFSET + PLANE_SIZE + tol_l;
    if u >= lo && u <= hi && v >= lo && v <= hi {
        Some(t)
    } else {
        None
    }
}

/// Ray vs a sphere (scale tip-box approximation). Returns the near `t` or `None`.
fn ray_sphere_hit(ro: Vec3, rd: Vec3, center: Vec3, radius: f32) -> Option<f32> {
    let oc = ro - center;
    let b = oc.dot(rd);
    let c = oc.dot(oc) - radius * radius;
    let disc = b * b - c;
    if disc < 0.0 {
        return None;
    }
    let t = -b - disc.sqrt();
    if t >= 0.0 {
        Some(t)
    } else {
        let t2 = -b + disc.sqrt();
        (t2 >= 0.0).then_some(t2)
    }
}

/// A stable orthonormal basis (u, v) for the plane perpendicular to `dir`.
fn perp_basis(dir: Vec3) -> (Vec3, Vec3) {
    let not_parallel = if dir.dot(Vec3::Y).abs() < 0.9 {
        Vec3::Y
    } else {
        Vec3::X
    };
    let u = dir.cross(not_parallel).normalize();
    let v = dir.cross(u).normalize();
    (u, v)
}

/// Two orthonormal basis vectors in the plane perpendicular to `axis` (rotation).
fn get_rotation_plane_basis(axis: Vec3) -> (Vec3, Vec3) {
    perp_basis(axis)
}

/// Cast a ray from the camera through a screen point and intersect a plane.
pub fn ray_plane_intersection(
    screen_x: f32,
    screen_y: f32,
    viewport_width: f32,
    viewport_height: f32,
    camera_matrices: &CameraMatrices,
    plane_point: Vec3,
    plane_normal: Vec3,
) -> Option<Vec3> {
    let (ray_origin, ray_direction) = ray_from_screen(
        screen_x,
        screen_y,
        viewport_width,
        viewport_height,
        camera_matrices.inv_view_projection(),
    );
    let denom = ray_direction.dot(plane_normal);
    if denom.abs() < 1e-6 {
        return None;
    }
    let t = (plane_point - ray_origin).dot(plane_normal) / denom;
    if t < 0.0 {
        return None;
    }
    Some(ray_origin + ray_direction * t)
}

// ── renderer accessors (unchanged) ────────────────────────────────────────────

fn get_local_transform(renderer: &AwsmRenderer, object: TransformObject) -> Option<Transform> {
    let local = renderer.transforms.get_local(object.key).ok()?.clone();
    match object.instance {
        Some(index) => Some(
            renderer
                .instances
                .get_transform(object.key, index)
                .unwrap_or(local),
        ),
        None => Some(local),
    }
}

fn get_world_matrix(renderer: &AwsmRenderer, object: TransformObject) -> Option<Mat4> {
    let world = *renderer.transforms.get_world(object.key).ok()?;
    match object.instance {
        Some(index) => {
            if let Some(t) = renderer.instances.get_transform(object.key, index) {
                Some(world.mul_mat4(&t.to_matrix()))
            } else {
                Some(world)
            }
        }
        None => {
            let mut world = world;
            if let Some(mesh_keys) = renderer.meshes.keys_by_transform_key(object.key) {
                let mut center_sum = Vec3::ZERO;
                let mut center_count = 0u32;
                for mesh_key in mesh_keys {
                    if let Ok(mesh) = renderer.meshes.get(*mesh_key) {
                        if let Some(aabb) = mesh.world_aabb.as_ref() {
                            center_sum += aabb.center();
                            center_count += 1;
                        }
                    }
                }
                if center_count > 0 {
                    let center = center_sum / center_count as f32;
                    world.w_axis = Vec4::new(center.x, center.y, center.z, 1.0);
                }
            }
            Some(world)
        }
    }
}

fn get_parent_key(renderer: &AwsmRenderer, object: TransformObject) -> Option<TransformKey> {
    renderer.transforms.get_parent(object.key).ok()
}

fn set_local_transform(
    renderer: &mut AwsmRenderer,
    object: TransformObject,
    transform: Transform,
) -> Result<()> {
    match object.instance {
        None => {
            renderer.transforms.set_local(object.key, transform)?;
        }
        Some(index) => {
            renderer
                .instances
                .transform_update(object.key, index, &transform);
        }
    }
    Ok(())
}
