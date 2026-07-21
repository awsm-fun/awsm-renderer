//! Pure `CameraConfig` → renderer `CameraParams` conversion — shared by the
//! editor bridge and `populate_awsm_scene`, the same single-source pattern as
//! materials/lights. Depth-of-field (`aperture`/`focus_distance`) isn't authored
//! on the node config yet, so it defaults to the values `scene_camera_matrices`
//! has always used.

use awsm_renderer::camera::{CameraParams, CameraProjectionParams};
use awsm_renderer_scene::{CameraConfig, CameraProjection};

/// Schema camera config → renderer camera params (projection kind + clip planes).
pub fn camera_params_from_config(cfg: &CameraConfig) -> CameraParams {
    let projection = match cfg.projection {
        CameraProjection::Perspective { fov_y_rad } => {
            CameraProjectionParams::Perspective { fov_y_rad }
        }
        CameraProjection::Orthographic { half_height } => {
            CameraProjectionParams::Orthographic { half_height }
        }
    };
    CameraParams {
        projection,
        near: cfg.near,
        far: cfg.far,
        aperture: 5.6,
        focus_distance: 10.0,
    }
}
