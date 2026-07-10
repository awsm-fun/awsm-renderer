//! Screen-space selection box — the orange rectangle the reference draws around
//! the selected object (an axis-aligned 2D bound of the object's projected
//! world AABB, *not* a 3D outline render pass). The render loop calls
//! [`update`] each frame with the live camera; the viewport overlays a div
//! bound to [`rect_signal`].

use awsm_renderer::camera::CameraMatrices;
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::AwsmRenderer;
use glam::Vec3;

use crate::controller::controller;
use crate::engine::bridge::bridge;
use crate::engine::context::with_canvas;
use crate::prelude::*;

thread_local! {
    /// Selected object's screen-space rect in CSS px: `[x, y, w, h]`, or `None`
    /// when nothing is selected / the object is off-screen.
    static RECT: Mutable<Option<[f64; 4]>> = Mutable::new(None);
}

/// Signal of the current selection rect for the viewport overlay.
pub fn rect_signal() -> impl Signal<Item = Option<[f64; 4]>> {
    RECT.with(|m| m.signal())
}

/// Recompute the selection rect from the single selection + live camera. Call
/// once per frame after the renderables are collected (i.e. after `render`).
/// Rides the "Show gizmo" setting: selection chrome contaminates verification
/// screenshots exactly like the transform gizmo, so `gizmo = false` (UI or
/// `SetViewOptions { gizmos }`) hides both.
pub fn update(renderer: &AwsmRenderer, matrices: &CameraMatrices) {
    let rect = if controller().settings.gizmo.get() {
        compute(renderer, matrices)
    } else {
        None
    };
    RECT.with(|m| m.set_neq(rect));
}

fn selected_mesh_keys() -> Option<Vec<MeshKey>> {
    let id = {
        let ctrl = controller();
        let sel = ctrl.selected.lock_ref();
        if sel.len() != 1 {
            return None;
        }
        sel[0]
    };
    let bridge = bridge();
    let nodes = bridge.nodes.lock().unwrap();
    let keys = nodes.get(&id)?.model_meshes.lock().unwrap().clone();
    if keys.is_empty() {
        None
    } else {
        Some(keys)
    }
}

/// Expand the screen rect a few px past the object's tight bound, like the
/// reference's slight margin.
const PAD: f64 = 5.0;

fn compute(renderer: &AwsmRenderer, matrices: &CameraMatrices) -> Option<[f64; 4]> {
    let mesh_keys = selected_mesh_keys()?;

    // Union the world AABBs of the node's meshes (object passes only — never the
    // gizmo's HUD renderables).
    let rs = renderer.renderables();
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    let mut found = false;
    for r in rs.opaque.iter().chain(rs.transparent.iter()) {
        if mesh_keys.contains(&r.key) {
            if let Some(aabb) = r.world_aabb() {
                min = min.min(aabb.min);
                max = max.max(aabb.max);
                found = true;
            }
        }
    }
    if !found {
        return None;
    }

    let vp = matrices.view_projection();
    let (cw, ch) = with_canvas(|c| (c.client_width() as f64, c.client_height() as f64));
    if cw <= 0.0 || ch <= 0.0 {
        return None;
    }

    let corners = [
        Vec3::new(min.x, min.y, min.z),
        Vec3::new(max.x, min.y, min.z),
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(max.x, max.y, min.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(min.x, max.y, max.z),
        Vec3::new(max.x, max.y, max.z),
    ];

    let mut sx_min = f64::INFINITY;
    let mut sy_min = f64::INFINITY;
    let mut sx_max = f64::NEG_INFINITY;
    let mut sy_max = f64::NEG_INFINITY;
    for corner in corners {
        let clip = vp * corner.extend(1.0);
        // Any corner behind the camera → don't draw a (degenerate/huge) box.
        if clip.w <= 0.0001 {
            return None;
        }
        let ndc_x = (clip.x / clip.w) as f64;
        let ndc_y = (clip.y / clip.w) as f64;
        let sx = (ndc_x * 0.5 + 0.5) * cw;
        let sy = (1.0 - (ndc_y * 0.5 + 0.5)) * ch;
        sx_min = sx_min.min(sx);
        sx_max = sx_max.max(sx);
        sy_min = sy_min.min(sy);
        sy_max = sy_max.max(sy);
    }

    Some([
        sx_min - PAD,
        sy_min - PAD,
        (sx_max - sx_min) + 2.0 * PAD,
        (sy_max - sy_min) + 2.0 * PAD,
    ])
}
