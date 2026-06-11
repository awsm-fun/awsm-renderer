//! Viewport **light icons** — a small pickable HUD marker at every light node.
//!
//! Lights carry no renderable geometry, so without a proxy they can't be
//! clicked in the viewport at all (you'd have to find them in the outliner).
//! This renders one HUD sphere per light (always on top, like the transform
//! gizmo), tracks each icon ↔ its light node, and resolves a pick on an icon to
//! a selection of that light. Once selected, the transform gizmo appears and the
//! light can be moved / rotated normally.
//!
//! Mirrors `gizmo.rs` / `curve_handles.rs`: the icon set lives in a thread-local
//! so the render loop (per-frame re-anchor + screen-constant zoom) and the
//! canvas pick handler can both reach it. Reuses the shared
//! [`PointHandleSet`](awsm_web_shared::viewport3d::point_handle::PointHandleSet)
//! for the HUD mesh + pick plumbing; we only add the icon→node mapping.

use std::cell::RefCell;

use awsm_renderer::camera::CameraMatrices;
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::AwsmRenderer;
use awsm_web_shared::viewport3d::point_handle::PointHandleSet;
use glam::Vec3;

use crate::engine::bridge::bridge;
use crate::engine::scene::NodeId;

thread_local! {
    static ICONS: RefCell<Option<LightIcons>> = const { RefCell::new(None) };
}

struct LightIcons {
    handles: PointHandleSet,
    /// Parallel to `handles`: icon index → the light node it represents.
    node_ids: Vec<NodeId>,
}

/// Initialise the icon set. Call once after the renderer + bridge are ready.
pub fn init() {
    ICONS.with(|c| {
        *c.borrow_mut() = Some(LightIcons {
            handles: PointHandleSet::new(),
            node_ids: Vec::new(),
        });
    });
}

/// Per-frame: re-anchor one HUD icon on every light node + keep them a fixed
/// pixel size. Called from the render loop after world transforms are derived.
pub fn per_frame_update(renderer: &mut AwsmRenderer, camera_matrices: &CameraMatrices) {
    // Snapshot the live light nodes + their world positions. Light nodes are
    // tracked by the node-sync bridge as they materialize/teardown.
    let mut positions: Vec<Vec3> = Vec::new();
    let mut ids: Vec<NodeId> = Vec::new();
    {
        let b = bridge();
        let light_ids: Vec<NodeId> = b.light_node_ids.lock().unwrap().iter().copied().collect();
        let nodes = b.nodes.lock().unwrap();
        for id in light_ids {
            if let Some(entry) = nodes.get(&id) {
                if let Ok(world) = renderer.transforms.get_world(entry.transform_key) {
                    positions.push(world.w_axis.truncate());
                    ids.push(id);
                }
            }
        }
    }

    ICONS.with(|c| {
        let mut guard = c.borrow_mut();
        let Some(icons) = guard.as_mut() else {
            return;
        };
        if positions.is_empty() {
            if icons.handles.handle_count() > 0 {
                icons.handles.clear(renderer);
                icons.node_ids.clear();
            }
            return;
        }
        let _ = icons.handles.set_points(renderer, &positions);
        icons.node_ids = ids;
        if !icons.handles.is_visible() {
            icons.handles.show(renderer, true);
        }
        let _ = icons.handles.zoom_handles(renderer, camera_matrices);
    });
}

/// If `mesh_key` (or a near-miss at the cursor) is a light icon, return the
/// light node it represents. Checked by the canvas pick handler before the
/// regular scene-mesh → node lookup, so clicking a light's icon selects it.
pub fn try_pick(renderer: &AwsmRenderer, mesh_key: MeshKey, x: i32, y: i32) -> Option<NodeId> {
    ICONS.with(|c| {
        let guard = c.borrow();
        let icons = guard.as_ref()?;
        let idx = icons
            .handles
            .is_handle_mesh(mesh_key)
            .or_else(|| icons.handles.pick_with_tolerance(renderer, x, y, 10.0))?;
        icons.node_ids.get(idx).copied()
    })
}
