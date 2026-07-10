mod projection;
mod view;

use awsm_renderer::bounds::Aabb;
use awsm_renderer::camera::CameraMatrices;
use glam::{Mat4, Vec3};
use projection::orthographic::OrthographicCamera;
use projection::perspective::PerspectiveCamera;
use view::orbit::OrbitCamera;

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq)]
pub enum CameraId {
    #[default]
    Orthographic,
    Perspective,
}

#[derive(Debug, Clone)]
pub struct Camera {
    projection: CameraProjection,
    view: CameraView,
    aabb: Aabb,
    margin: f32,
    pub focus_distance: f32,
    pub aperture: f32,
}

// This is what needs to be implemented to make the camera work with the renderer
impl Camera {
    pub fn is_orthographic(&self) -> bool {
        matches!(self.projection, CameraProjection::Orthographic(_))
    }

    pub fn is_perspective(&self) -> bool {
        matches!(self.projection, CameraProjection::Perspective(_))
    }
    pub fn projection_matrix(&self) -> Mat4 {
        match &self.projection {
            CameraProjection::Orthographic(camera) => camera.projection_matrix(),
            CameraProjection::Perspective(camera) => camera.projection_matrix(),
        }
    }

    pub fn view_matrix(&self) -> Mat4 {
        match &self.view {
            CameraView::Orbit(camera) => camera.get_view_matrix(),
        }
    }

    pub fn position_world(&self) -> Vec3 {
        match &self.view {
            CameraView::Orbit(camera) => camera.get_position(),
        }
    }

    pub fn matrices(&self) -> CameraMatrices {
        CameraMatrices {
            view: self.view_matrix(),
            projection: self.projection_matrix(),
            position_world: self.position_world(),
            focus_distance: self.focus_distance,
            aperture: self.aperture,
            // Model-tests stay forward-Z (features default; 003)
            reverse_z: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum CameraView {
    Orbit(OrbitCamera),
}

impl CameraView {
    pub fn position(&self) -> Vec3 {
        match self {
            CameraView::Orbit(camera) => camera.get_position(),
        }
    }

    pub fn look_at(&self) -> Vec3 {
        match self {
            CameraView::Orbit(camera) => camera.look_at,
        }
    }

    pub fn view_matrix(&self) -> Mat4 {
        match self {
            CameraView::Orbit(camera) => camera.get_view_matrix(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum CameraProjection {
    Orthographic(OrthographicCamera),
    Perspective(PerspectiveCamera),
}

impl Camera {
    pub fn new_orthographic(
        aabb: Option<Aabb>,
        gltf_doc: Option<gltf::Document>,
        aspect: f32,
        aperture: f32,
        focus_distance: f32,
    ) -> Self {
        let margin = 1.1;

        let aabb = aabb.unwrap_or_else(|| {
            if let Some(doc) = &gltf_doc {
                awsm_renderer_gltf::aabb_from_gltf_doc(doc)
            } else {
                Aabb::new_unit_cube()
            }
        });

        let mut view = OrbitCamera::new_aabb(&aabb, margin);
        if let Some(doc) = &gltf_doc {
            view.setup_from_gltf(doc);
        }
        let view = CameraView::Orbit(view);

        let mut projection = OrthographicCamera::new_aabb(&view, &aabb, margin, aspect);
        if let Some(doc) = &gltf_doc {
            projection.setup_from_gltf(doc);
        }
        let projection = CameraProjection::Orthographic(projection);

        Self {
            projection,
            view,
            aabb,
            margin,
            aperture,
            focus_distance,
        }
    }

    pub fn new_perspective(
        aabb: Option<Aabb>,
        gltf_doc: Option<gltf::Document>,
        aspect: f32,
        aperture: f32,
        focus_distance: f32,
    ) -> Self {
        let margin = 1.1;
        let aabb = aabb.unwrap_or_else(|| {
            if let Some(doc) = &gltf_doc {
                awsm_renderer_gltf::aabb_from_gltf_doc(doc)
            } else {
                Aabb::new_unit_cube()
            }
        });

        let mut view = OrbitCamera::new_aabb(&aabb, margin);
        if let Some(doc) = &gltf_doc {
            view.setup_from_gltf(doc);
        }
        let view = CameraView::Orbit(view);

        let mut projection = PerspectiveCamera::new_aabb(&view, &aabb, margin, aspect);
        if let Some(doc) = &gltf_doc {
            projection.setup_from_gltf(doc);
        }
        let projection = CameraProjection::Perspective(projection);

        Self {
            projection,
            view,
            aabb,
            margin,
            aperture,
            focus_distance,
        }
    }

    pub fn on_pointer_down(&mut self) {
        match &mut self.view {
            CameraView::Orbit(orbit_view) => orbit_view.on_pointer_down(),
        }
    }

    pub fn on_pointer_move(&mut self, x: i32, y: i32) {
        match &mut self.view {
            CameraView::Orbit(orbit_view) => orbit_view.on_pointer_move(x as f32, y as f32),
        }

        match &mut self.projection {
            CameraProjection::Orthographic(ortho) => {
                ortho.on_view_changed(&self.view, &self.aabb, self.margin);
            }
            CameraProjection::Perspective(persp) => {
                persp.on_view_changed(&self.view, &self.aabb, self.margin);
            }
        }
    }

    pub fn on_pointer_up(&mut self) {
        match &mut self.view {
            CameraView::Orbit(orbit_view) => orbit_view.on_pointer_up(),
        }
    }

    pub fn on_wheel(&mut self, delta: f64) {
        match &mut self.view {
            CameraView::Orbit(orbit_view) => orbit_view.on_wheel(delta as f32),
        }

        match &mut self.projection {
            CameraProjection::Orthographic(ortho) => {
                ortho.on_wheel(&self.view, &self.aabb, self.margin, delta as f32);
            }
            CameraProjection::Perspective(persp) => {
                persp.on_wheel(&self.view, &self.aabb, self.margin);
            }
        }
    }

    pub fn on_resize(&mut self, aspect: f32) {
        match &mut self.projection {
            CameraProjection::Orthographic(ortho) => {
                ortho.on_resize(&self.view, &self.aabb, self.margin, aspect);
            }
            CameraProjection::Perspective(persp) => {
                persp.on_resize(aspect);
            }
        }
    }
}
