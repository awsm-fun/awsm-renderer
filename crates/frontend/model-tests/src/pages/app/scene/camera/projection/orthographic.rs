use awsm_renderer::bounds::Aabb;
use glam::Mat4;

use crate::pages::app::scene::camera::CameraView;

use super::clip_planes::tight_clip_planes_from_aabb;

/// Orthographic projection camera for WebGPU (depth range [0,1])
#[derive(Debug, Clone)]
pub struct OrthographicCamera {
    pub left: f32,
    pub right: f32,
    pub bottom: f32,
    pub top: f32,
    pub near: f32,
    pub far: f32,
}

impl OrthographicCamera {
    pub fn new_aabb(view: &CameraView, aabb: &Aabb, margin: f32, aspect: f32) -> Self {
        // Use the bounding sphere radius instead of direct XY dimensions
        // This works correctly regardless of model rotation
        let bounding_radius = aabb.size().length() * 0.5;

        let mut half_h = bounding_radius;
        let mut half_w = half_h * aspect;

        half_w *= margin;
        half_h *= margin;

        let mut _self = Self {
            left: -half_w,
            right: half_w,
            bottom: -half_h,
            top: half_h,
            near: 0.01, // initial placeholder
            far: 100.0, // initial placeholder
        };

        _self.on_resize(view, aabb, margin, aspect);

        _self
    }

    pub fn on_wheel(&mut self, view: &CameraView, aabb: &Aabb, margin: f32, delta: f32) {
        self.zoom(1.0 + delta * 0.001);
        self.update_near_far(view, aabb, margin);
    }

    /// Recomputes near/far after camera view changes (orbit/pan/rotate).
    pub fn on_view_changed(&mut self, view: &CameraView, aabb: &Aabb, margin: f32) {
        self.update_near_far(view, aabb, margin);
    }

    // internal helper, whenever zoom changes to adjust clipping dynamically.
    fn update_near_far(&mut self, view: &CameraView, aabb: &Aabb, margin: f32) {
        let (near, far) = tight_clip_planes_from_aabb(view, aabb, margin);
        self.near = near;
        self.far = far;

        // eh, whatever
        // self.near = 0.1;
        // self.far = 100.0;
        // self.near = self.near.min(0.001);
        // self.far = self.far.max(1000000.0);
    }

    // Call this method whenever the window is resized.
    pub fn on_resize(&mut self, view: &CameraView, aabb: &Aabb, margin: f32, aspect: f32) {
        // current centre of the frustum
        let cx = (self.left + self.right) * 0.5;

        // keep vertical span, change horizontal to match aspect
        let half_h = (self.top - self.bottom) * 0.5;
        let half_w = half_h * aspect;

        self.left = cx - half_w;
        self.right = cx + half_w;

        // near/far might change too if the user resized immediately
        // after zooming the orbit camera
        self.update_near_far(view, aabb, margin);
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

    /// Zooms the ortho view in (factor<1) or out (factor>1), keeping the same center.
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

    pub fn setup_from_gltf(&mut self, _doc: &gltf::Document) {}
}
