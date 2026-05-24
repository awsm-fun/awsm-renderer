//! Viewport camera controls (Reset View, projection mode, etc.).

use crate::config::CONFIG;
use crate::context::{with_camera_mut, with_canvas, with_renderer};
use crate::state::app_state;
use awsm_renderer::bounds::Aabb;
use awsm_renderer::meshes::MeshKey;
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

/// Frame the viewport camera on the bounds of `mesh_keys`. Called by
/// [`crate::actions::insert::model`] right after `wait_for_models_ready`
/// so a freshly-inserted glTF — including small ones like
/// `DamagedHelmet` (~2 unit AABB) — lands centered in view instead of
/// floating as a speck under the editor's default 36-unit-away camera.
/// `model-tests` already does this via its `Camera::new_perspective`
/// constructor; this is the editor's equivalent.
///
/// Reads each mesh's `world_aabb` from the renderer and unions them.
/// Skips meshes with no `world_aabb` (skinned / vertex-animated meshes
/// before their first transform refresh). Bails silently if the union
/// is empty so a glTF that materialises with zero visible primitives
/// (extremely rare) doesn't snap the camera to the origin.
///
/// Preserves the user's current projection mode (Perspective /
/// Orthographic) and Reset-View aperture / focus settings — the only
/// changes are view position + look-at + frustum proportions.
pub async fn frame_on_meshes(mesh_keys: Vec<MeshKey>) {
    if mesh_keys.is_empty() {
        return;
    }
    let union = with_renderer(|r| {
        let mut acc: Option<Aabb> = None;
        for key in &mesh_keys {
            let Ok(mesh) = r.meshes.get(*key) else {
                continue;
            };
            let Some(aabb) = mesh.world_aabb.as_ref() else {
                continue;
            };
            match &mut acc {
                Some(u) => u.extend(aabb),
                None => acc = Some(aabb.clone()),
            }
        }
        acc
    })
    .await;
    let Some(aabb) = union else {
        tracing::debug!(
            "frame_on_meshes: no world_aabb on any of {} mesh(es); skipping",
            mesh_keys.len()
        );
        return;
    };

    let (w, h) = with_canvas(|c| (c.width().max(1), c.height().max(1)));
    let aspect = w as f32 / h as f32;
    let mode = app_state().projection_mode.get();

    with_camera_mut(|c| {
        // Margin 1.5 matches `model-tests`' framing — gives a small
        // border so the model isn't clipped against the viewport edge.
        let mut fresh = FreeCamera::new_aabb(aabb, aspect, 1.5);
        fresh.set_aperture(CONFIG.camera_aperture);
        fresh.set_focus_distance(CONFIG.camera_focus_distance);
        fresh.set_projection_mode(mode);
        *c = fresh;
    });
    tracing::info!(
        "action: camera::frame_on_meshes({} mesh(es))",
        mesh_keys.len()
    );
}
