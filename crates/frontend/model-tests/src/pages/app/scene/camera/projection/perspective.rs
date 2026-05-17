use awsm_renderer::bounds::Aabb;
use glam::Mat4;

use crate::pages::app::scene::camera::CameraView;

use super::clip_planes::tight_clip_planes_from_aabb;

/// Perspective projection camera for WebGPU (depth range [0, 1])
#[derive(Debug, Clone)]
pub struct PerspectiveCamera {
    /// Vertical field‑of‑view in radians
    pub fov_y: f32,
    /// Viewport aspect ratio (width / height)
    pub aspect: f32,
    pub near: f32,
    pub far: f32,
}

impl PerspectiveCamera {
    /// Build a perspective camera that encloses `aabb` with the same
    /// margin logic used for the orthographic version.
    ///
    /// `aspect` should come from the current swap‑chain size.
    pub fn new_aabb(view: &CameraView, aabb: &Aabb, margin: f32, aspect: f32) -> Self {
        // 45° vertical FOV is a comfortable default
        let fov_y = 45.0_f32.to_radians();

        let mut this = Self {
            fov_y,
            aspect,
            near: 0.01, // placeholder – fixed just below
            far: 100.0, // placeholder – fixed just below
        };

        this.update_near_far(view, aabb, margin);
        this
    }

    /// Call whenever the window is resized.
    pub fn on_resize(&mut self, new_aspect: f32) {
        self.aspect = new_aspect;
    }

    /// Call on every mouse‑wheel event *after* `OrbitCamera` has
    /// updated its radius, so the clip planes stay snug.
    pub fn on_wheel(&mut self, view: &CameraView, aabb: &Aabb, margin: f32) {
        self.update_near_far(view, aabb, margin);
    }

    /// Recomputes near/far after camera view changes (orbit/pan/rotate).
    pub fn on_view_changed(&mut self, view: &CameraView, aabb: &Aabb, margin: f32) {
        self.update_near_far(view, aabb, margin);
    }

    /// Keeps the scene fully inside the frustum while minimising
    /// depth‑buffer precision loss.
    fn update_near_far(&mut self, view: &CameraView, aabb: &Aabb, margin: f32) {
        let (near, far) = tight_clip_planes_from_aabb(view, aabb, margin);
        self.near = near;
        self.far = far;
    }

    /// Standard right‑handed perspective projection
    pub fn projection_matrix(&self) -> Mat4 {
        Mat4::perspective_rh(self.fov_y, self.aspect, self.near, self.far)
    }

    pub fn setup_from_gltf(&mut self, _doc: &gltf::Document) {}
}
