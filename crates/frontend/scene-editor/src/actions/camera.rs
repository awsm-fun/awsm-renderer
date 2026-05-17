//! Viewport camera controls (Reset View, projection mode, etc.).

use crate::config::CONFIG;
use crate::context::with_camera_mut;
use crate::state::app_state;
use awsm_web_shared::util::free_camera::{FreeCamera, ProjectionMode};

pub fn reset_view() {
    let mode = app_state().projection_mode.get();
    with_camera_mut(|c| {
        let mut fresh = FreeCamera::new_default_cube(16.0 / 9.0);
        fresh.set_aperture(CONFIG.camera_aperture);
        fresh.set_focus_distance(CONFIG.camera_focus_distance);
        // Preserve the user's chosen projection across Reset View — the
        // dropdown shouldn't silently snap back to Perspective.
        fresh.set_projection_mode(mode);
        *c = fresh;
    });
    tracing::info!("action: camera::reset_view");
}

/// Switch the viewport projection. Called from the Camera tab dropdown.
pub fn set_projection_mode(mode: ProjectionMode) {
    let state = app_state();
    state.projection_mode.set_neq(mode);
    with_camera_mut(|c| c.set_projection_mode(mode));
    tracing::info!("action: camera::set_projection_mode({})", mode.id());
}
