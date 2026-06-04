//! Orbit + pan + zoom free camera.
//!
//! Lifted from the game-editor's viewport camera and shared across
//! every frontend app that wants a "look around the scene" mode.
//!
//! # Coordinate convention
//!
//! Right-handed, **Y-up**, **-Z-forward** (matches `glam::Mat4::look_at_rh`
//! / `Mat4::perspective_rh` and the gltf spec). The yaw/pitch convention
//! mirrors that:
//!
//! * `yaw == 0` looks down `-Z` (camera at `+Z`, target at origin)
//! * `yaw == π/2` looks down `-X`
//! * `pitch > 0` raises the camera above the horizon (looks down)
//!
//! WebGPU's NDC z range is `[0, 1]`, but world-space handedness is the
//! engine's call. We pick RH to match every other matrix call in the
//! renderer.
//!
//! # Aperture / focus distance
//!
//! Both fields appear in [`CameraMatrices`] for the renderer's DOF
//! pass; they're caller-tuned per app. Defaults are `aperture = 5.6`
//! and `focus_distance = 10.0`, matching the historical editor values.
//! Override via [`FreeCamera::set_aperture`] / [`FreeCamera::set_focus_distance`].

use awsm_renderer::{bounds::Aabb, camera::CameraMatrices};
use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

/// Which projection the viewport is currently using.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionMode {
    Perspective,
    Orthographic,
}

impl ProjectionMode {
    pub const ALL: [Self; 2] = [Self::Perspective, Self::Orthographic];

    pub fn label(self) -> &'static str {
        match self {
            Self::Perspective => "Perspective",
            Self::Orthographic => "Orthographic",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Perspective => "perspective",
            Self::Orthographic => "orthographic",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|m| m.id() == id)
    }
}

/// Combined orbit + dual-projection free camera. Both projection
/// branches are kept up-to-date as the orbit moves, so toggling
/// `mode` is a cheap enum flip.
#[derive(Debug, Clone)]
pub struct FreeCamera {
    perspective: CameraPerspectiveProjection,
    orthographic: CameraOrthographicProjection,
    mode: ProjectionMode,
    view: CameraView,
    aabb: Aabb,
    margin: f32,
    aperture: f32,
    focus_distance: f32,
}

impl FreeCamera {
    /// Construct a camera framed around an axis-aligned bounding box.
    /// `aspect` is `width / height`; pass `16.0 / 9.0` if you don't have
    /// a real canvas size yet (resize later via [`Self::set_aspect`]).
    pub fn new_aabb(aabb: Aabb, aspect: f32, margin: f32) -> Self {
        let view = CameraView::new_aabb(&aabb, margin);
        let perspective = CameraPerspectiveProjection::new_aabb(&view, &aabb, margin, aspect);
        let orthographic = CameraOrthographicProjection::new_aabb(&view, &aabb, margin, aspect);
        Self {
            view,
            perspective,
            orthographic,
            mode: ProjectionMode::Perspective,
            aabb,
            margin,
            aperture: 5.6,
            focus_distance: 10.0,
        }
    }

    /// Convenience: a default cube AABB sized for general
    /// "what's at origin" scenes. 80×80 cube → orbit radius ~76 m,
    /// which sits comfortably outside the largest current game arena
    /// (jetpack-duel's 18 m-radius dome) without being so far that
    /// smaller scenes (pole-balance) look like specks. Tunable per-
    /// game via `FreeCamera::set_aabb` once that wiring exists.
    pub fn new_default_cube(aspect: f32) -> Self {
        Self::new_aabb(Aabb::new_cube(40.0, 40.0), aspect, 1.1)
    }

    pub fn matrices(&self) -> CameraMatrices {
        let projection = match self.mode {
            ProjectionMode::Perspective => self.perspective.projection_matrix(),
            ProjectionMode::Orthographic => self.orthographic.projection_matrix(),
        };
        CameraMatrices {
            view: self.view.get_view_matrix(),
            projection,
            position_world: self.view.get_position(),
            aperture: self.aperture,
            focus_distance: self.focus_distance,
        }
    }

    pub fn set_aperture(&mut self, aperture: f32) {
        self.aperture = aperture;
    }

    pub fn set_focus_distance(&mut self, focus_distance: f32) {
        self.focus_distance = focus_distance;
    }

    pub fn projection_mode(&self) -> ProjectionMode {
        self.mode
    }

    /// Switch projection without disturbing the orbit pose. Both
    /// branches' near/far are refreshed against the current view so
    /// the active projection renders correctly on the very next frame.
    pub fn set_projection_mode(&mut self, mode: ProjectionMode) {
        self.mode = mode;
        self.perspective
            .refresh_clip_planes(&self.view, &self.aabb, self.margin);
        self.orthographic
            .refresh_clip_planes(&self.view, &self.aabb, self.margin);
    }

    /// Snap the orbit to an explicit yaw/pitch (radians), preserving the current
    /// look-at point + radius. Used by the nav-cube axis-snap (and any external/
    /// MCP camera drive). Convention: `yaw == 0` looks down `-Z`, `yaw == π/2`
    /// looks down `-X`, `pitch > 0` raises the camera (looks down).
    pub fn snap_to(&mut self, yaw: f32, pitch: f32) {
        self.view = CameraView::new(yaw, pitch, self.view.look_at, self.view.radius);
    }

    /// Reset the orbit to the default framing (the `new_default_cube` pose) —
    /// look-at back at the origin, default yaw/pitch + radius — preserving the
    /// current projection mode and aspect. Backs the "Reset View" action.
    pub fn reset_default(&mut self) {
        self.aabb = Aabb::new_cube(40.0, 40.0);
        self.margin = 1.1;
        self.view = CameraView::new_aabb(&self.aabb, self.margin);
        self.perspective
            .refresh_clip_planes(&self.view, &self.aabb, self.margin);
        self.orthographic
            .refresh_clip_planes(&self.view, &self.aabb, self.margin);
    }

    pub fn set_aspect(&mut self, aspect: f32) {
        self.perspective.on_resize(aspect);
        self.orthographic
            .on_resize(&self.view, &self.aabb, self.margin, aspect);
    }

    pub fn on_pointer_down(&mut self) {
        self.view.on_pointer_down();
    }

    pub fn on_pointer_move(&mut self, x: i32, y: i32, is_panning: bool) {
        self.view.on_pointer_move(x as f32, y as f32, is_panning);
    }

    pub fn on_pointer_up(&mut self) {
        self.view.on_pointer_up();
    }

    pub fn on_wheel(&mut self, delta: f64) {
        self.view.on_wheel(delta as f32);
        // Keep both projections current — a mid-zoom mode-switch
        // shouldn't have to wait for the next wheel tick.
        self.perspective
            .on_wheel(&self.view, &self.aabb, self.margin);
        self.orthographic
            .on_wheel(&self.view, &self.aabb, self.margin, delta as f32);
    }
}

#[derive(Debug, Clone)]
pub struct CameraView {
    /// Point the camera orbits around.
    pub look_at: Vec3,
    /// Distance from look_at.
    pub radius: f32,
    pub sensitivity: f32,

    yaw: f32,
    pitch: f32,
    dragging: bool,
}

impl CameraView {
    pub fn new_aabb(aabb: &Aabb, margin: f32) -> Self {
        let center = aabb.center();
        let size = aabb.size();

        let bounding_radius = size.length() * 0.5;
        let radius = bounding_radius * margin;

        // Start head-on: looking from +Z axis, slightly above.
        // yaw: 0 = looking from +Z, π/2 = from +X, π = from -Z, 3π/2 = from -X
        let yaw = 0.0;
        // pitch: positive = camera above looking down
        let pitch = 0.3; // ~17° above horizon, looking down slightly

        Self::new(yaw, pitch, center, radius)
    }

    pub fn new_default(radius: f32) -> Self {
        // head-on view from -Z, X/Y at zero — useful for sanity-checking.
        let yaw: f32 = std::f32::consts::PI;
        let pitch: f32 = 0.0;
        let look_at = Vec3::ZERO;
        Self::new(yaw, pitch, look_at, radius)
    }

    pub fn new(yaw: f32, pitch: f32, look_at: Vec3, radius: f32) -> Self {
        Self {
            look_at,
            radius,
            yaw,
            pitch,
            dragging: false,
            sensitivity: 0.005,
        }
    }

    /// Right-handed look-at view matrix.
    pub fn get_view_matrix(&self) -> Mat4 {
        let cam_pos = self.get_position();
        Mat4::look_at_rh(cam_pos, self.look_at, Vec3::Y)
    }

    /// Current camera world position. Spherical → Cartesian.
    pub fn get_position(&self) -> Vec3 {
        let x = self.radius * self.pitch.cos() * self.yaw.sin();
        let y = self.radius * self.pitch.sin();
        let z = self.radius * self.pitch.cos() * self.yaw.cos();
        self.look_at + Vec3::new(x, y, z)
    }

    pub fn on_pointer_down(&mut self) {
        self.dragging = true;
    }

    pub fn on_pointer_move(&mut self, delta_x: f32, delta_y: f32, is_panning: bool) {
        if !self.dragging {
            return;
        }
        if is_panning {
            self.pan(delta_x, delta_y);
            return;
        }
        self.yaw -= delta_x * self.sensitivity;
        self.pitch -= delta_y * self.sensitivity;
        // Clamp pitch to just under ±90° to prevent flipping.
        let limit = std::f32::consts::FRAC_PI_2 - 0.0001;
        self.pitch = self.pitch.clamp(-limit, limit);
    }

    pub fn on_pointer_up(&mut self) {
        self.dragging = false;
    }

    pub fn on_wheel(&mut self, delta_y: f32) {
        let zoom_factor = 1.0 + delta_y * 0.001;
        self.radius = (self.radius * zoom_factor).max(0.1);
    }

    fn pan(&mut self, delta_x: f32, delta_y: f32) {
        let cam_pos = self.get_position();
        let forward = (self.look_at - cam_pos).normalize();
        let right = forward.cross(Vec3::Y).normalize();
        let up = right.cross(forward).normalize();

        let pan_scale = self.radius * self.sensitivity;
        let translation = right * (delta_x * pan_scale) - up * (delta_y * pan_scale);

        self.look_at += translation;
    }
}

/// Orthographic projection (WebGPU depth range `[0, 1]`).
#[derive(Debug, Clone)]
pub struct CameraOrthographicProjection {
    pub left: f32,
    pub right: f32,
    pub bottom: f32,
    pub top: f32,
    pub near: f32,
    pub far: f32,
}

impl CameraOrthographicProjection {
    pub fn new_aabb(view: &CameraView, aabb: &Aabb, margin: f32, aspect: f32) -> Self {
        let bounding_radius = aabb.size().length() * 0.5;

        let mut half_h = bounding_radius;
        let mut half_w = half_h * aspect;
        half_w *= margin;
        half_h *= margin;

        let mut this = Self {
            left: -half_w,
            right: half_w,
            bottom: -half_h,
            top: half_h,
            near: 0.01,
            far: 100.0,
        };
        this.on_resize(view, aabb, margin, aspect);
        this
    }

    pub fn on_wheel(&mut self, view: &CameraView, aabb: &Aabb, margin: f32, delta: f32) {
        self.zoom(1.0 + delta * 0.001);
        self.refresh_clip_planes(view, aabb, margin);
    }

    pub fn refresh_clip_planes(&mut self, view: &CameraView, aabb: &Aabb, margin: f32) {
        let bounding_radius = aabb.size().length() * 0.5;
        let distance = view.get_position().distance(view.look_at);
        self.near = (distance - bounding_radius * margin * 2.0).max(0.01);
        self.far = distance + bounding_radius * margin * 2.0;
    }

    pub fn on_resize(&mut self, view: &CameraView, aabb: &Aabb, margin: f32, aspect: f32) {
        let cx = (self.left + self.right) * 0.5;
        let half_h = (self.top - self.bottom) * 0.5;
        let half_w = half_h * aspect;
        self.left = cx - half_w;
        self.right = cx + half_w;
        self.refresh_clip_planes(view, aabb, margin);
    }

    pub fn projection_matrix(&self) -> Mat4 {
        Mat4::orthographic_rh(
            self.left,
            self.right,
            self.bottom,
            self.top,
            self.near,
            self.far,
        )
    }

    pub fn zoom(&mut self, factor: f32) {
        let cx = (self.left + self.right) * 0.5;
        let cy = (self.bottom + self.top) * 0.5;
        let half_w = (self.right - self.left) * 0.5 * factor;
        let half_h = (self.top - self.bottom) * 0.5 * factor;
        self.left = cx - half_w;
        self.right = cx + half_w;
        self.bottom = cy - half_h;
        self.top = cy + half_h;
    }
}

/// Perspective projection (WebGPU depth range `[0, 1]`).
#[derive(Debug, Clone)]
pub struct CameraPerspectiveProjection {
    pub fov_y: f32,
    pub aspect: f32,
    pub near: f32,
    pub far: f32,
}

impl CameraPerspectiveProjection {
    pub fn new_aabb(view: &CameraView, aabb: &Aabb, margin: f32, aspect: f32) -> Self {
        let fov_y = 45.0_f32.to_radians();
        let mut this = Self {
            fov_y,
            aspect,
            near: 0.01,
            far: 100.0,
        };
        this.refresh_clip_planes(view, aabb, margin);
        this
    }

    pub fn on_resize(&mut self, new_aspect: f32) {
        self.aspect = new_aspect;
    }

    pub fn on_wheel(&mut self, view: &CameraView, aabb: &Aabb, margin: f32) {
        self.refresh_clip_planes(view, aabb, margin);
    }

    pub fn refresh_clip_planes(&mut self, view: &CameraView, aabb: &Aabb, margin: f32) {
        let bounding_radius = aabb.size().length() * 0.5;
        let distance = view.get_position().distance(view.look_at);
        self.near = (distance - bounding_radius * margin * 2.0).max(0.01);
        self.far = distance + bounding_radius * margin * 2.0;
    }

    pub fn projection_matrix(&self) -> Mat4 {
        Mat4::perspective_rh(self.fov_y, self.aspect, self.near, self.far)
    }
}
