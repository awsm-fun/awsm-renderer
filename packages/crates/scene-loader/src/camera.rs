//! Pure `CameraConfig` → renderer `CameraParams` conversion — shared by the
//! editor bridge and `populate_awsm_scene`, the same single-source pattern as
//! materials/lights. Depth-of-field (`aperture`/`focus_distance`) comes from the
//! node config when authored there, else the renderer defaults.

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
        aperture: cfg
            .aperture
            .filter(|a| a.is_finite() && *a > 0.0)
            .unwrap_or(CameraParams::DEFAULT_APERTURE),
        focus_distance: cfg
            .focus_distance
            .filter(|d| d.is_finite() && *d > 0.0)
            .unwrap_or(CameraParams::DEFAULT_FOCUS_DISTANCE),
    }
}
