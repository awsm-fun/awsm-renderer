//! Per-asset batched instantiator for glb Model nodes.
//!
//! When a glb is inserted, the editor synchronously stamps a tree of
//! Model nodes into the scene (one per gltf node that owns mesh
//! primitives). Each reactive Model node's `apply_kind_model`
//! observer would, prior to batching, spawn its own async task that
//! awaited `cache.get_or_load(asset_id)` then took the renderer lock
//! for its `instance_template` work. For a 38-primitive robot that's
//! 38 separate `Mutex::lock().await` acquisitions on
//! `renderer_handle()` — and the per-frame `render_one_frame`
//! `try_lock()` interleaving between them — which serialised through
//! the renderer mutex with the render loop fighting in the gaps.
//!
//! This module folds those 38 acquisitions into a single one per
//! glb. Each Model node calls [`enqueue`] (sync, microsecond cost),
//! which adds it to a per-`AssetId` queue and — if not already
//! running — spawns a single coordinator task for that asset.
//!
//! The coordinator:
//! 1. Awaits `cache.get_or_load(asset_id)` (shared with all other
//!    coordinators / consumers — actually loads the glb exactly once).
//! 2. Drains the pending queue.
//! 3. Takes the renderer lock **once**, performs every queued entry's
//!    mesh-duplicate + material-override work, runs **one**
//!    `finalize_gpu_textures`, drops the lock.
//! 4. Loops: any entries that arrived during materialization (e.g. the
//!    user double-Inserts the same glb) get picked up.
//!
//! Cancellation: each Model node still owns its `AsyncLoader`; the
//! task spawned by `apply_kind_model` is what drives a `oneshot::Receiver`
//! that the coordinator fires post-materialize. If the kind flips back
//! to Group mid-load the loader is dropped, the receiver is dropped,
//! and the coordinator's `send(())` on a closed channel is silently
//! ignored. The per-entry guard inside the materialize loop checks
//! `entry.asset_id` and skips entries whose target asset has changed,
//! so a stale enqueue never wires up the wrong meshes.

use crate::context::renderer_handle;
use crate::prelude::*;
use crate::renderer_bridge::asset_cache::AssetTemplate;
use crate::renderer_bridge::node_sync::{
    bridge, mesh_shadow_flags_from_config, report_model_load_failure, RendererNode,
};
use crate::scene::{AssetId, AssetStatus, NodeKind};
use crate::state::app_state;
use futures::channel::oneshot;
use std::collections::HashMap;
use std::sync::LazyLock;
use wasm_bindgen_futures::spawn_local;

/// Inputs the batcher needs to materialize a single Model node.
pub struct PendingInstance {
    pub entry: Arc<RendererNode>,
    pub asset_id: AssetId,
    pub node_index: u32,
    pub primitive_index: Option<u32>,
    /// Fires after the batch that processed this entry finishes
    /// (success or failure). The signal is "done" — the actual status
    /// is read from `entry.node.asset_status`. Dropping the receiver
    /// is fine: `send` on a closed channel is a no-op.
    pub done: oneshot::Sender<()>,
}

#[derive(Default)]
struct BatcherState {
    pending: HashMap<AssetId, Vec<PendingInstance>>,
    /// `true` for an asset id whose coordinator is alive. Prevents
    /// us from spawning a second coordinator while one is mid-flight;
    /// the running coordinator drains the queue in a loop and only
    /// clears this flag right before returning.
    coordinator_active: HashMap<AssetId, ()>,
}

static BATCHER: LazyLock<Mutex<BatcherState>> =
    LazyLock::new(|| Mutex::new(BatcherState::default()));

/// Enqueue an entry for batched materialization. Returns immediately
/// — the actual work runs on the per-asset coordinator task. The
/// caller is expected to await the `oneshot::Receiver` paired with
/// the `done` sender on the pending instance.
pub fn enqueue(pending: PendingInstance) {
    let asset_id = pending.asset_id;
    let spawn_coordinator;
    {
        let mut state = BATCHER.lock().unwrap();
        state.pending.entry(asset_id).or_default().push(pending);
        spawn_coordinator = !state.coordinator_active.contains_key(&asset_id);
        if spawn_coordinator {
            state.coordinator_active.insert(asset_id, ());
        }
    }
    if spawn_coordinator {
        spawn_local(async move {
            coordinator(asset_id).await;
        });
    }
}

async fn coordinator(asset_id: AssetId) {
    let cache = bridge().assets.clone();
    let asset_entry = cache.get_or_load(asset_id);
    let template_result = asset_entry.wait().await;

    loop {
        // Drain the queue. If nothing's pending, retire the
        // coordinator. The flag is cleared *inside* the lock to
        // avoid a race where a fresh `enqueue` lands between
        // "queue is empty" and "I'm done."
        let pendings = {
            let mut state = BATCHER.lock().unwrap();
            let drained = match state.pending.get_mut(&asset_id) {
                Some(v) => std::mem::take(v),
                None => Vec::new(),
            };
            if drained.is_empty() {
                state.coordinator_active.remove(&asset_id);
                state.pending.remove(&asset_id);
                return;
            }
            drained
        };

        match &template_result {
            Ok(template) => {
                materialize_batch(asset_id, template, pendings).await;
            }
            Err(err) => {
                for pending in pendings {
                    report_model_load_failure(pending.entry.clone(), asset_id, err.clone());
                    let _ = pending.done.send(());
                }
            }
        }
    }
}

async fn materialize_batch(
    asset_id: AssetId,
    template: &AssetTemplate,
    pendings: Vec<PendingInstance>,
) {
    // Read scene-side state *before* taking the renderer lock — the
    // scene mutex is independent of the renderer mutex, and reading
    // it under the renderer lock would unnecessarily prolong the
    // critical section.
    let gltf_material_asset_ids: Vec<AssetId> = {
        let scene = app_state().scene.clone();
        let assets = scene.assets.lock().unwrap();
        assets
            .get(asset_id)
            .map(|e| e.gltf_material_asset_ids.clone())
            .unwrap_or_default()
    };
    let scene_for_materials = app_state().scene.clone();

    // Snapshot per-entry inputs that depend on scene state at queue
    // time. This way the renderer-locked loop only touches renderer
    // and template data — no signal evaluations, no scene mutex.
    struct PreparedEntry {
        pending: PendingInstance,
        visible: bool,
        shadow_cfg: Option<awsm_scene_schema::MeshShadowConfig>,
    }
    let mut prepared: Vec<PreparedEntry> = pendings
        .into_iter()
        .map(|pending| {
            // The kind-change guard: if the entry's asset_id no
            // longer matches, the materialize loop will skip it. We
            // still snapshot visibility / shadow because reading
            // those is cheap and lets the loop be lock-free against
            // the scene.
            let visible = *pending.entry.effective_visible.lock().unwrap();
            let shadow_cfg = match &*pending.entry.node.kind.lock_ref() {
                NodeKind::Model(r) => Some(r.shadow),
                _ => None,
            };
            PreparedEntry {
                pending,
                visible,
                shadow_cfg,
            }
        })
        .collect();

    // The single renderer-lock acquisition for the whole batch.
    let handle = renderer_handle();
    let mut renderer = handle.lock().await;

    // Per-entry results we'll publish after dropping the lock.
    let mut per_entry_meshes: Vec<Vec<awsm_renderer::meshes::MeshKey>> =
        Vec::with_capacity(prepared.len());

    for entry_state in &prepared {
        // Guard: did this node get removed or change kind while we
        // were waiting for the batch? If so, skip — the materialize
        // call is benign-but-wasteful otherwise.
        let cur_asset = *entry_state.pending.entry.asset_id.lock().unwrap();
        if cur_asset != Some(asset_id) {
            per_entry_meshes.push(Vec::new());
            continue;
        }

        let Some(template_node) = template.find_by_node_index(entry_state.pending.node_index)
        else {
            tracing::warn!(
                "instance_batcher: gltf node_index={} not found in template; \
                 nothing to render for this node",
                entry_state.pending.node_index
            );
            per_entry_meshes.push(Vec::new());
            continue;
        };
        if template_node.mesh_keys.is_empty() {
            per_entry_meshes.push(Vec::new());
            continue;
        }

        // `primitive_index = Some(i)` is the Split-peeled case. Mirrors
        // the original `instance_template` selection logic.
        let (mesh_keys, gltf_material_indices): (Vec<_>, Vec<Option<usize>>) =
            match entry_state.pending.primitive_index {
                None => (
                    template_node.mesh_keys.clone(),
                    template_node.mesh_gltf_material_indices.clone(),
                ),
                Some(idx) => match template_node.mesh_keys.get(idx as usize).copied() {
                    Some(k) => {
                        let mat_idx = template_node
                            .mesh_gltf_material_indices
                            .get(idx as usize)
                            .copied()
                            .unwrap_or(None);
                        (vec![k], vec![mat_idx])
                    }
                    None => {
                        tracing::warn!(
                            "instance_batcher: primitive_index={idx} out of range for gltf \
                             node_index={} (has {} primitives)",
                            entry_state.pending.node_index,
                            template_node.mesh_keys.len()
                        );
                        per_entry_meshes.push(Vec::new());
                        continue;
                    }
                },
            };

        let parent_tk = entry_state.pending.entry.transform_key;
        let visible = entry_state.visible;
        let mut created = Vec::with_capacity(mesh_keys.len());

        for (i, mesh_key) in mesh_keys.iter().enumerate() {
            match renderer.duplicate_mesh_with_transform(*mesh_key, parent_tk) {
                Ok(new_mesh) => {
                    let _ = renderer.set_mesh_hidden(new_mesh, !visible);

                    if let Some(gltf_mat_idx) = gltf_material_indices.get(i).copied().flatten() {
                        if let Some(&override_asset_id) = gltf_material_asset_ids.get(gltf_mat_idx)
                        {
                            let override_ref = awsm_scene_schema::MaterialRef(override_asset_id);
                            match super::material_cache::get_or_create(
                                &mut renderer,
                                &scene_for_materials,
                                override_ref,
                            ) {
                                Some(key) => {
                                    if let Err(err) = renderer.set_mesh_material(new_mesh, key) {
                                        tracing::warn!(
                                            "instance_batcher: set_mesh_material for editable \
                                             gltf material {gltf_mat_idx} (asset \
                                             {override_asset_id}) failed: {err}"
                                        );
                                    }
                                }
                                None => {
                                    tracing::warn!(
                                        "instance_batcher: get_or_create for gltf material \
                                         {gltf_mat_idx} (asset {override_asset_id}) returned \
                                         None — falling back to renderer-baked material"
                                    );
                                }
                            }
                        }
                    }

                    created.push(new_mesh);
                }
                Err(err) => {
                    tracing::warn!("instance_batcher: duplicate_mesh_with_transform failed: {err}");
                }
            }
        }

        // Apply shadow flags while we still hold the renderer lock —
        // the original single-entry path used a separate
        // `with_renderer_mut` round-trip after dropping the lock,
        // which re-acquired the mutex (and re-contended with the
        // render loop) for no good reason. Folding it in here makes
        // the whole materialize path a single lock-hold.
        if let Some(cfg) = entry_state.shadow_cfg {
            let flags = mesh_shadow_flags_from_config(&cfg);
            for mk in &created {
                let _ = renderer.set_mesh_shadow_flags(*mk, flags);
            }
        }

        per_entry_meshes.push(created);
    }

    // Single finalize for the whole batch. The texture-pool dirty
    // gate (see renderer/src/textures.rs::finalize_gpu_textures)
    // short-circuits to ~0 ms after the first call within a batch —
    // material_cache hits are cache-only on the second+ entry, so
    // only one entry's worth of texture uploads can actually dirty
    // the pool.
    if let Err(err) = renderer.finalize_gpu_textures().await {
        tracing::warn!("instance_batcher: finalize_gpu_textures failed: {err}");
    }

    drop(renderer);

    // Publish results and signal completion. After dropping the lock
    // so signal observers don't reacquire it inside our critical
    // section.
    for (entry_state, created) in prepared.drain(..).zip(per_entry_meshes) {
        if !created.is_empty() {
            entry_state
                .pending
                .entry
                .model_meshes
                .lock()
                .unwrap()
                .extend(created);
        }
        entry_state
            .pending
            .entry
            .node
            .asset_status
            .set(AssetStatus::Ready);
        app_state().clear_asset_failure(entry_state.pending.entry.node_id);
        let _ = entry_state.pending.done.send(());
    }

    // Single revision bump for the whole batch instead of N. Anything
    // that derives from "is this Model splittable?" only needs to
    // re-evaluate once the materialization is visible.
    bridge().bump_nodes_revision();
}
