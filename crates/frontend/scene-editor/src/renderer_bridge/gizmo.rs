//! On-canvas transform gizmo, backed by `awsm_renderer_editor::TransformController`.
//!
//! Loads `gizmo.glb` from the editor-common CDN and builds a
//! `TransformController` bound to its named meshes. The controller is
//! stored on `AppState` so both the render loop (for per-frame zoom-to-
//! screen-size) and the canvas pointer handlers (for pick + drag) can
//! reach it.

use crate::config::CONFIG;
use crate::context::renderer_handle;
use crate::scene::NodeId;
use crate::state::app_state;
use awsm_renderer::{transforms::TransformKey, AwsmRenderer};
use awsm_renderer_editor::transform_controller::{GizmoSpace, TransformController};
use awsm_renderer_gltf::{data::GltfDataHints, loader::GltfLoader, AwsmRendererGltfExt};
use futures_signals::map_ref;
use futures_signals::signal::SignalExt;
use wasm_bindgen_futures::spawn_local;

/// Which kind of pointer drag is currently in flight. Cleared on pointerup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveAction {
    CameraMoving,
    GizmoTransforming,
    PointHandleDragging,
}

/// Initialise the gizmo. Safe to call once; no-op on re-entry.
pub fn init() {
    spawn_local(async {
        if let Err(err) = init_inner().await {
            tracing::error!("gizmo init failed: {err}");
        }
    });
}

async fn init_inner() -> anyhow::Result<()> {
    let url = CONFIG.gizmo_url();
    let loader = GltfLoader::load(&url, None)
        .await
        .map_err(|e| anyhow::anyhow!("gizmo.glb load: {e}"))?;

    let handle = renderer_handle();
    let mut renderer = handle.lock().await;

    let hints = GltfDataHints::default()
        .with_hud(true)
        .with_hidden(true)
        .with_render_timings(renderer.logging.render_timings);
    let gltf_data = loader
        .into_data(Some(hints))
        .map_err(|e| anyhow::anyhow!("gizmo decode: {e}"))?;

    let ctx = renderer
        .populate_gltf(gltf_data, None)
        .await
        .map_err(|e| anyhow::anyhow!("gizmo populate: {e}"))?;

    let controller = TransformController::new(ctx.key_lookups.clone(), GizmoSpace::Global)
        .map_err(|e| anyhow::anyhow!("TransformController::new: {e}"))?;

    // Hide all gizmo meshes until a selection appears.
    let _ = controller.set_hidden(&mut renderer, true, true, true);
    drop(renderer);

    let state = app_state();
    *state.transform_controller.lock().unwrap() = Some(controller);

    // Wire the selection signal — when exactly one node is selected, the
    // gizmo centers on it; otherwise the gizmo hides.
    start_selection_observer();

    tracing::info!("gizmo: loaded + controller ready");
    Ok(())
}

fn start_selection_observer() {
    let state = app_state();
    let selected = state.selected.clone();
    let bridge = state.renderer_bridge.clone();

    spawn_local(async move {
        let selected_id = selected.signal_ref(|set| {
            if set.len() == 1 {
                set.iter().next().copied()
            } else {
                None
            }
        });
        // Combine with `nodes_revision` so that a selection which fires
        // before the bridge entry is inserted gets re-synced once the
        // entry appears. Without this the gizmo stays hidden on fresh
        // insert (the selection observer fires, but the transform key
        // lookup returns None).
        let key_lookup = {
            let bridge = bridge.clone();
            move |id: &Option<NodeId>| -> Option<TransformKey> {
                id.and_then(|id| {
                    bridge
                        .nodes
                        .lock()
                        .unwrap()
                        .get(&id)
                        .map(|n| n.transform_key)
                })
            }
        };
        map_ref! {
            let id = selected_id,
            let _rev = bridge.nodes_revision.signal() => {
                (*id, key_lookup(id))
            }
        }
        .dedupe()
        .for_each(move |(id, _key)| async move {
            sync_gizmo_selection(id).await;
        })
        .await;
    });
}

async fn sync_gizmo_selection(id: Option<NodeId>) {
    use awsm_renderer_editor::transform_controller::TransformObject;
    let state = app_state();

    // Look up the bridge entry for this scene node to get its TransformKey.
    let transform_key = id.and_then(|id| {
        state
            .renderer_bridge
            .nodes
            .lock()
            .unwrap()
            .get(&id)
            .map(|n| n.transform_key)
    });

    let handle = renderer_handle();
    let mut renderer = handle.lock().await;
    let mut controller_lock = state.transform_controller.lock().unwrap();
    let Some(controller) = controller_lock.as_mut() else {
        return;
    };

    let gizmo_enabled = state.gizmo_enabled.get();
    match transform_key {
        Some(key) => {
            controller.selected_object = Some(TransformObject {
                key,
                instance: None,
            });
            // Show all three gizmo types when the user has the feature
            // enabled; otherwise stay hidden even on selection. The
            // per-frame update enforces this every frame too, so this
            // call mostly avoids a one-frame flash.
            let force_hidden = !gizmo_enabled;
            let _ = controller.set_hidden(&mut renderer, force_hidden, force_hidden, force_hidden);
        }
        None => {
            controller.selected_object = None;
            let _ = controller.set_hidden(&mut renderer, true, true, true);
        }
    }
}

/// Per-frame gizmo update: keep the gizmo a fixed screen size and
/// re-anchored under the current selection.
///
/// The first thing we do every frame is *enforce* the gizmo's
/// visibility against `selected_object`. The selection observer
/// (`sync_gizmo_selection`) is the primary path that toggles
/// `set_hidden`, but if anything else mutates mesh visibility
/// (e.g. a future render-pass tweak, or a dropped signal during a
/// busy frame), the gizmo could otherwise linger after a deselect.
/// This guarantees "no `selected_object` → no visible gizmo," every
/// frame, no matter what.
pub fn per_frame_update(renderer: &mut AwsmRenderer) {
    let state = app_state();
    let mut controller_lock = state.transform_controller.lock().unwrap();
    let Some(controller) = controller_lock.as_mut() else {
        return;
    };
    let has_selection = controller.selected_object.is_some();
    // Gizmo visibility is "selection AND gizmo-enabled toggle on";
    // either condition false → all three handle types hidden. This
    // also ensures the GPU pick won't see the handles, so clicks pass
    // through as scene-object picks (or empty-space deselects).
    let gizmo_enabled = state.gizmo_enabled.get();
    let force_hidden = !has_selection || !gizmo_enabled;
    let _ = controller.set_hidden(renderer, force_hidden, force_hidden, force_hidden);
    if force_hidden {
        return;
    }
    let Some(matrices) = renderer.camera.last_matrices.as_ref().cloned() else {
        return;
    };
    // `zoom_gizmo_transforms` also re-centers under the currently-selected
    // transform; see awsm-renderer-editor for the exact math.
    let _ = controller.zoom_gizmo_transforms(renderer, &matrices);
}

/// Map a transform key back to a scene node id.
pub fn node_for_transform_key(transform_key: TransformKey) -> Option<NodeId> {
    let state = app_state();
    let nodes = state.renderer_bridge.nodes.lock().unwrap();
    for (id, entry) in nodes.iter() {
        if entry.transform_key == transform_key {
            return Some(*id);
        }
        // The picked mesh may sit on a populated sub-transform, not the
        // node's primary transform. Walk those too.
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

/// Write the selected node's renderer-side local transform back into
/// its `scene.Trs`. Called while dragging a gizmo so the properties
/// panel updates live.
pub fn sync_scene_transform_from_renderer() {
    let state = app_state();
    let controller_lock = state.transform_controller.lock().unwrap();
    let Some(controller) = controller_lock.as_ref() else {
        return;
    };
    let Some(selected) = controller.selected_object else {
        return;
    };
    drop(controller_lock);

    // Fetch the renderer's current local transform for the node.
    let state_for_async = state.clone();
    spawn_local(async move {
        let handle = renderer_handle();
        let renderer = handle.lock().await;
        let Ok(local) = renderer.transforms.get_local(selected.key).cloned() else {
            return;
        };
        drop(renderer);

        let Some(node_id) = node_for_transform_key(selected.key) else {
            return;
        };
        let nodes = state_for_async.renderer_bridge.nodes.lock().unwrap();
        let Some(entry) = nodes.get(&node_id) else {
            return;
        };
        // Push the renderer-side local back into the scene Trs. This fires
        // the bridge's transform observer, which pushes back to renderer —
        // but the value is identical so the round-trip is a no-op.
        let trs = crate::scene::Trs {
            translation: local.translation.to_array(),
            rotation: local.rotation.to_array(),
            scale: local.scale.to_array(),
        };
        entry.node.transform.set(trs);
    });
}
