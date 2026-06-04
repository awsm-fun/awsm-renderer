//! On-canvas transform gizmo, backed by
//! `awsm_web_shared::viewport3d::TransformController`.
//!
//! Loads the crate-bundled `gizmo.glb`, populates it into the renderer as a
//! HUD/hidden model, and builds a `TransformController` bound to its named
//! handle meshes. The controller lives in a thread-local (wasm is
//! single-threaded) so both the render loop (per-frame zoom-to-screen-size +
//! re-anchor under the selection) and the canvas pointer handlers (pick + drag)
//! can reach it.

use std::cell::RefCell;

use awsm_renderer::meshes::MeshKey;
use awsm_renderer::transforms::TransformKey;
use awsm_renderer::AwsmRenderer;
use awsm_renderer_gltf::{data::GltfDataHints, loader::GltfLoader, AwsmRendererGltfExt};
use awsm_web_shared::viewport3d::transform_controller::{
    GizmoSpace, TransformController, TransformObject, TransformTarget,
};
use futures_signals::map_ref;

use super::context::renderer_handle;
use crate::controller::controller;
use crate::engine::bridge::bridge;
use crate::engine::config::CONFIG;
use crate::engine::scene::NodeId;
use crate::prelude::*;

thread_local! {
    static GIZMO: RefCell<Option<TransformController>> = const { RefCell::new(None) };
}

/// Initialise the gizmo. Call once after the renderer + bridge are ready.
pub fn init() {
    spawn_local(async {
        if let Err(err) = init_inner().await {
            tracing::error!("gizmo init failed: {err}");
        }
    });
}

async fn init_inner() -> Result<(), String> {
    let loader = GltfLoader::load(CONFIG.gizmo_url(), None)
        .await
        .map_err(|e| format!("gizmo.glb load: {e}"))?;

    let handle = renderer_handle();
    let mut renderer = handle.lock().await;

    let hints = GltfDataHints::default().with_hud(true).with_hidden(true);
    let gltf_data = loader
        .into_data(Some(hints))
        .map_err(|e| format!("gizmo decode: {e}"))?;

    let ctx = renderer
        .populate_gltf(gltf_data, None)
        .await
        .map_err(|e| format!("gizmo populate: {e}"))?;

    let controller = TransformController::new(ctx.key_lookups.clone(), GizmoSpace::Global)
        .map_err(|e| format!("TransformController::new: {e}"))?;

    // Hide every handle until a selection appears.
    let _ = controller.set_hidden(&mut renderer, true, true, true);
    drop(renderer);

    GIZMO.with(|g| *g.borrow_mut() = Some(controller));

    start_selection_observer();
    Ok(())
}

/// Anchor the gizmo on the single selected node (hide on multi/empty selection).
/// Combined with `scene.revision` so a selection that fires *before* the bridge
/// entry materializes re-syncs once the entry (and its transform key) appears.
fn start_selection_observer() {
    spawn_local(async move {
        let selected_id = controller().selected.signal_ref(|ids| {
            if ids.len() == 1 {
                Some(ids[0])
            } else {
                None
            }
        });
        map_ref! {
            let id = selected_id,
            let _rev = controller().scene.revision.signal() => *id
        }
        .dedupe()
        .for_each(|id| async move {
            sync_gizmo_selection(id).await;
        })
        .await;
    });
}

/// Look up a scene node's renderer-side transform key via the bridge.
fn transform_key_for_node(id: NodeId) -> Option<TransformKey> {
    bridge()
        .nodes
        .lock()
        .unwrap()
        .get(&id)
        .map(|n| n.transform_key)
}

async fn sync_gizmo_selection(id: Option<NodeId>) {
    let transform_key = id.and_then(transform_key_for_node);

    let handle = renderer_handle();
    let mut renderer = handle.lock().await;
    GIZMO.with(|g| {
        let mut guard = g.borrow_mut();
        let Some(controller) = guard.as_mut() else {
            return;
        };
        let gizmo_enabled = controller_gizmo_enabled();
        match transform_key {
            Some(key) => {
                controller.selected_object = Some(TransformObject {
                    key,
                    instance: None,
                });
                let hidden = !gizmo_enabled;
                let _ = controller.set_hidden(&mut renderer, hidden, hidden, hidden);
            }
            None => {
                controller.selected_object = None;
                let _ = controller.set_hidden(&mut renderer, true, true, true);
            }
        }
    });
}

fn controller_gizmo_enabled() -> bool {
    controller().settings.gizmo.get()
}

/// Per-frame update: enforce visibility against the selection + the toggle, keep
/// the gizmo a fixed screen size, and re-anchor it under the selected transform.
/// Called from the render loop after `update_camera`.
pub fn per_frame_update(renderer: &mut AwsmRenderer) {
    GIZMO.with(|g| {
        let mut guard = g.borrow_mut();
        let Some(controller) = guard.as_mut() else {
            return;
        };
        let has_selection = controller.selected_object.is_some();
        let force_hidden = !has_selection || !controller_gizmo_enabled();
        let _ = controller.set_hidden(renderer, force_hidden, force_hidden, force_hidden);
        if force_hidden {
            return;
        }
        let Some(matrices) = renderer.camera.last_matrices.as_ref().cloned() else {
            return;
        };
        let _ = controller.zoom_gizmo_transforms(renderer, &matrices);
    });
}

/// Begin a gizmo drag if `mesh_key` is one of the gizmo handles. Returns `true`
/// when a handle was grabbed (the caller then routes pointer-move to the gizmo
/// instead of the camera). The renderer lock is held by the caller.
pub fn try_start_pick(renderer: &mut AwsmRenderer, mesh_key: MeshKey, x: i32, y: i32) -> bool {
    GIZMO.with(|g| {
        let mut guard = g.borrow_mut();
        let Some(controller) = guard.as_mut() else {
            return false;
        };
        matches!(
            controller.start_pick(renderer, mesh_key, x, y),
            Some(TransformTarget::GizmoHit(_))
        )
    })
}

/// Apply a pointer-move delta to the in-flight gizmo drag.
pub fn drag(renderer: &mut AwsmRenderer, dx: i32, dy: i32) {
    GIZMO.with(|g| {
        if let Some(controller) = g.borrow_mut().as_mut() {
            controller.update_transform(renderer, dx, dy);
        }
    });
}

/// Map a transform key back to a scene node id (checks the node's primary
/// transform and any populated sub-transforms).
fn node_for_transform_key(transform_key: TransformKey) -> Option<NodeId> {
    let bridge = bridge();
    let nodes = bridge.nodes.lock().unwrap();
    for (id, entry) in nodes.iter() {
        if entry.transform_key == transform_key {
            return Some(*id);
        }
        if entry
            .model_transforms
            .lock()
            .unwrap()
            .contains(&transform_key)
        {
            return Some(*id);
        }
    }
    None
}

/// Write the dragged node's renderer-side local transform back into its scene
/// `Trs` so the Inspector updates live. Spawns its own renderer read.
pub fn sync_scene_transform_from_renderer() {
    let selected = GIZMO.with(|g| g.borrow().as_ref().and_then(|c| c.selected_object));
    let Some(selected) = selected else {
        return;
    };
    spawn_local(async move {
        let handle = renderer_handle();
        let local = {
            let renderer = handle.lock().await;
            renderer.transforms.get_local(selected.key).cloned().ok()
        };
        let Some(local) = local else {
            return;
        };
        let Some(node_id) = node_for_transform_key(selected.key) else {
            return;
        };
        let node = bridge()
            .nodes
            .lock()
            .unwrap()
            .get(&node_id)
            .map(|n| n.node.clone());
        if let Some(node) = node {
            node.transform.set(crate::engine::scene::types::Trs {
                translation: local.translation.to_array(),
                rotation: local.rotation.to_array(),
                scale: local.scale.to_array(),
            });
        }
    });
}
