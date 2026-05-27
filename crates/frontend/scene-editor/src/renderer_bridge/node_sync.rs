//! Per-node bridge entries + signal observers that keep the renderer
//! in lockstep with the editor scene.
//!
//! Each editor `Node` maps to one `RendererNode`, which owns the renderer
//! transform key plus a bag of `AsyncLoader`s for the observers that
//! watch transform / kind / children changes. Dropping the `RendererNode`
//! cancels those observers and reclaims the renderer resources.

#![allow(clippy::arc_with_non_send_sync)]

use crate::context::{renderer_handle, with_renderer_mut};
use crate::prelude::*;
use crate::renderer_bridge::asset_cache::AssetCache;
use crate::scene::{AssetId, AssetStatus, Node, NodeId, NodeKind, Trs};
use crate::state::app_state;
use awsm_renderer::transforms::{Transform, TransformKey};
use futures::channel::oneshot;
use futures_signals::signal_vec::VecDiff;
use glam::{Quat, Vec3};
use std::collections::HashMap;
use wasm_bindgen_futures::spawn_local;

/// One bridge entry per live scene node.
pub struct RendererNode {
    pub node_id: NodeId,
    pub node: Arc<Node>,
    pub transform_key: TransformKey,
    /// Currently-active model asset id, if any. Used for guard-checks
    /// when an in-flight load completes (skip if the kind has changed
    /// out from under us).
    pub asset_id: Mutex<Option<AssetId>>,
    /// Transform keys we spawned *underneath* `transform_key` when
    /// instancing a glb. Removed + their meshes dropped on cleanup.
    pub model_transforms: Mutex<Vec<TransformKey>>,
    pub model_meshes: Mutex<Vec<awsm_renderer::meshes::MeshKey>>,
    /// Inline-material `MaterialKey`s this node owns. Shared (asset-cache)
    /// keys are *not* tracked here — they live for the cache's lifetime.
    /// `clear_model_instance` + `remove_node` free everything in this vec
    /// to keep the renderer's materials slotmap from growing on every
    /// kind change.
    pub material_keys: Mutex<Vec<awsm_renderer::materials::MaterialKey>>,
    /// Hash of the *geometry inputs* (curve control points, cross
    /// section, uv mode, samples, up hint) used to build this node's
    /// current mesh, when the node is a `SweepAlongCurve`. Used by
    /// `apply_kind`'s fast path: if the next `kind.set` doesn't change
    /// this hash, only the material binding is updated — saves a full
    /// `sweep_along_curve` evaluation + GPU upload on material-only
    /// edits of a heavy sweep (4096 samples × 32 radial = ~130k verts).
    pub sweep_geometry_hash: Mutex<Option<u64>>,
    /// The `NodeKind` value most recently materialized into the
    /// renderer for this node. F-A fast path: if the next `kind.set`
    /// emits a value `==` to this, skip the full clear-then-materialize
    /// — the bridge's per-frame state is already correct. Catches the
    /// common case where the inspector's `number_input` writes back a
    /// value identical to what's already there (Mutable::set always
    /// emits regardless of equality).
    pub last_applied_kind: Mutex<Option<NodeKind>>,
    /// Line strips registered against this node — Line / curve-visualization
    /// kinds register through `AwsmRenderer::add_line_strip` and store the
    /// returned `LineKey`(s) here. Cleared on kind change via `clear_lines`
    /// + on node removal.
    pub line_keys: Mutex<Vec<awsm_renderer::render_passes::lines::LineKey>>,
    /// `Some` if this node is currently a Light; `None` otherwise.
    pub light_key: Mutex<Option<awsm_renderer::lights::LightKey>>,
    /// `Some` if this node is currently a Decal; `None` otherwise.
    /// The per-frame `sync_decals_pre_render` reads this to push the
    /// node's current world transform via `AwsmRenderer::update_decal`.
    pub decal_key: Mutex<Option<awsm_renderer::decals::DecalKey>>,
    /// `node.visible` ANDed with every ancestor's visible. Updated by
    /// `apply_visibility_subtree` whenever any ancestor or this node
    /// flips its own `visible`. Read by the per-frame light + collision
    /// passes so they can skip hidden nodes; written before changing
    /// the renderer-side mesh hidden state for Model nodes.
    pub effective_visible: Mutex<bool>,
    /// Loader holding in-flight asset load. Dropping cancels.
    pub asset_loader: AsyncLoader,
    /// Observer tasks tied to this node. Dropping cancels.
    pub tasks: Mutex<Vec<AsyncLoader>>,
}

impl RendererNode {
    /// Clear the `last_applied_kind` identity cache for a node so the
    /// next `kind.set` is always treated as a fresh transition — even
    /// when the new value compares equal to the previous one. Needed
    /// when the outside world has changed in a way that requires
    /// re-materialize but doesn't show up in the kind value (e.g.
    /// mesh_cache bytes got overwritten under the same MeshRef).
    pub fn invalidate_apply_kind_cache(node_id: NodeId) {
        if let Some(entry) = bridge().nodes.lock().unwrap().get(&node_id).cloned() {
            *entry.last_applied_kind.lock().unwrap() = None;
        }
    }

    pub fn new(node: Arc<Node>, transform_key: TransformKey) -> Arc<Self> {
        Arc::new(Self {
            node_id: node.id,
            node,
            transform_key,
            asset_id: Mutex::new(None),
            model_transforms: Mutex::new(Vec::new()),
            model_meshes: Mutex::new(Vec::new()),
            material_keys: Mutex::new(Vec::new()),
            sweep_geometry_hash: Mutex::new(None),
            last_applied_kind: Mutex::new(None),
            line_keys: Mutex::new(Vec::new()),
            light_key: Mutex::new(None),
            decal_key: Mutex::new(None),
            effective_visible: Mutex::new(true),
            asset_loader: AsyncLoader::new(),
            tasks: Mutex::new(Vec::new()),
        })
    }
}

/// Shared bridge state. Mirrors the scene tree's structure one-to-one.
pub struct Bridge {
    pub nodes: Mutex<HashMap<NodeId, Arc<RendererNode>>>,
    pub assets: Arc<AssetCache>,
    /// Top-level of the scene → top-level of our tracking. Each entry
    /// maps a "children level" (via key = parent node id, or None for
    /// scene root) to the ordered list of NodeIds currently at that level.
    /// Needed because `VecDiff::RemoveAt` doesn't carry the removed id.
    pub child_order: Mutex<HashMap<Option<NodeId>, Vec<NodeId>>>,
    /// Bumps whenever a `RendererNode` is added or removed. Observers
    /// that need to react to bridge state (e.g. the gizmo rebinding
    /// when its target first appears) combine this with `selected`.
    pub nodes_revision: Mutable<u64>,
    /// Index of node ids whose kind has populated `light_key`. Kept
    /// in lockstep with `apply_kind_light` / `clear_light` /
    /// `remove_node` so the per-frame `sync_lights_pre_render` can
    /// iterate only the nodes that actually own lights instead of
    /// walking the entire bridge table every frame.
    pub light_node_ids: Mutex<std::collections::HashSet<NodeId>>,
    /// Mirror of [`Self::light_node_ids`] for Decal kinds.
    pub decal_node_ids: Mutex<std::collections::HashSet<NodeId>>,
    /// Mirror for Collider kinds. Iterated by
    /// `collider_wireframe::render::collect_shapes` each frame to
    /// rebuild the editor's collider wireframe overlay; an indexed
    /// set means a large art scene doesn't pay an O(N) walk +
    /// kind-clone per frame just to find the (typically few)
    /// collider authoring nodes.
    pub collider_node_ids: Mutex<std::collections::HashSet<NodeId>>,
    /// Mirror for Camera kinds. Iterated by
    /// `collider_wireframe::render::collect_cameras` each frame to
    /// draw the editor's camera-frustum wireframes.
    pub camera_node_ids: Mutex<std::collections::HashSet<NodeId>>,
    /// Pending coalesced `nodes_revision` bump, owned so its
    /// `AnimationFrame` callback stays alive until it fires.
    /// `bump_nodes_revision` consults this — if it's already
    /// `Some`, a bump is already scheduled and the call is a
    /// no-op. When the rAF fires it `take()`s itself out of the
    /// slot, drops the frame handle (already-fired so cancel is
    /// a no-op), and bumps the revision once. Result: N
    /// per-frame `bump_nodes_revision` calls collapse into a
    /// single signal-graph cascade per animation frame.
    pub pending_bump_raf: Mutex<Option<gloo_render::AnimationFrame>>,
}

impl Bridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            nodes: Mutex::new(HashMap::new()),
            assets: Arc::new(AssetCache::new()),
            child_order: Mutex::new(HashMap::new()),
            nodes_revision: Mutable::new(0),
            light_node_ids: Mutex::new(std::collections::HashSet::new()),
            decal_node_ids: Mutex::new(std::collections::HashSet::new()),
            collider_node_ids: Mutex::new(std::collections::HashSet::new()),
            camera_node_ids: Mutex::new(std::collections::HashSet::new()),
            pending_bump_raf: Mutex::new(None),
        })
    }

    /// Schedules a `nodes_revision` bump for the next animation
    /// frame, coalescing every call that lands inside the same
    /// frame into a single signal-graph cascade. Consumers
    /// (selection observer, gizmo, point-handle, inspector) all
    /// re-derive on every revision bump, so for a multi-mesh
    /// model insert (which used to fire `bump_nodes_revision`
    /// per node during `insert_node` and again per node during
    /// `remove_node`) this collapses dozens of cascades into one.
    /// Reactive observers don't expect synchronous response —
    /// they're all `signal()` subscribers — so a one-frame delay
    /// is invisible.
    pub fn bump_nodes_revision(&self) {
        // Hold the slot lock across the whole check → schedule → store
        // sequence. `request_animation_frame` only *registers* the
        // callback (it never runs it inline), so the rAF closure — which
        // locks `pending_bump_raf` itself — can't fire until this guard
        // drops, well after this function returns. Taking the lock once
        // and keeping it also closes the check-then-act window: two
        // callers can't both observe `None` and each queue a frame.
        let mut slot = self.pending_bump_raf.lock().unwrap();
        if slot.is_some() {
            return;
        }
        let frame = gloo_render::request_animation_frame(|_| {
            let b = bridge();
            // Drop the held frame handle first — already-fired,
            // so this is just slot cleanup; lets the next bump
            // schedule another rAF.
            b.pending_bump_raf.lock().unwrap().take();
            let prev = b.nodes_revision.get();
            b.nodes_revision.set(prev.wrapping_add(1));
        });
        *slot = Some(frame);
    }
}

pub fn bridge() -> Arc<Bridge> {
    app_state().renderer_bridge.clone()
}

// ==================== observer wiring ====================

/// Start watching the scene's top-level children vector.
pub fn start_top_level_observer() {
    let scene = app_state().scene.clone();
    let top_signal = scene.nodes.signal_vec_cloned();
    spawn_local(async move {
        use futures_signals::signal_vec::SignalVecExt;
        top_signal
            .for_each(|diff| async move {
                handle_children_diff(None, diff).await;
            })
            .await;
    });
}

/// Apply a `VecDiff` coming from either the scene root's children
/// (parent_id = None) or an interior node's children.
async fn handle_children_diff(parent_id: Option<NodeId>, diff: VecDiff<Arc<Node>>) {
    let parent_tk = resolve_parent_transform_key(parent_id).await;
    let Some(parent_tk) = parent_tk else {
        tracing::warn!("children diff for missing parent {parent_id:?}; dropping");
        return;
    };

    match diff {
        VecDiff::Replace { values } => {
            remove_all_children(parent_id).await;
            for (i, node) in values.iter().enumerate() {
                add_child_at(parent_id, parent_tk, i, node.clone()).await;
            }
        }
        VecDiff::Push { value } => {
            let len = current_children_len(parent_id);
            add_child_at(parent_id, parent_tk, len, value).await;
        }
        VecDiff::InsertAt { index, value } => {
            add_child_at(parent_id, parent_tk, index, value).await;
        }
        VecDiff::UpdateAt { index, value } => {
            // A clean "replace this child" path. Tear the old one down,
            // stand a fresh one up at the same index.
            remove_child_at(parent_id, index).await;
            add_child_at(parent_id, parent_tk, index, value).await;
        }
        VecDiff::RemoveAt { index } => {
            remove_child_at(parent_id, index).await;
        }
        VecDiff::Move {
            old_index,
            new_index,
        } => {
            let bridge_handle = bridge();
            let mut order = bridge_handle.child_order.lock().unwrap();
            if let Some(level) = order.get_mut(&parent_id) {
                if old_index < level.len() && new_index <= level.len() {
                    let id = level.remove(old_index);
                    level.insert(new_index.min(level.len()), id);
                }
            }
        }
        VecDiff::Pop {} => {
            let idx = current_children_len(parent_id).saturating_sub(1);
            if current_children_len(parent_id) > 0 {
                remove_child_at(parent_id, idx).await;
            }
        }
        VecDiff::Clear {} => {
            remove_all_children(parent_id).await;
        }
    }
}

async fn resolve_parent_transform_key(parent_id: Option<NodeId>) -> Option<TransformKey> {
    match parent_id {
        None => Some(with_renderer_mut(|r| r.transforms.root_node).await),
        Some(id) => bridge()
            .nodes
            .lock()
            .unwrap()
            .get(&id)
            .map(|n| n.transform_key),
    }
}

fn current_children_len(parent_id: Option<NodeId>) -> usize {
    bridge()
        .child_order
        .lock()
        .unwrap()
        .get(&parent_id)
        .map(|v| v.len())
        .unwrap_or(0)
}

async fn remove_all_children(parent_id: Option<NodeId>) {
    let ids: Vec<NodeId> = bridge()
        .child_order
        .lock()
        .unwrap()
        .remove(&parent_id)
        .unwrap_or_default();
    for id in ids {
        remove_node(id).await;
    }
}

async fn remove_child_at(parent_id: Option<NodeId>, index: usize) {
    let id_opt = {
        let bridge_handle = bridge();
        let mut order = bridge_handle.child_order.lock().unwrap();
        order.get_mut(&parent_id).and_then(|v| {
            if index < v.len() {
                Some(v.remove(index))
            } else {
                None
            }
        })
    };
    if let Some(id) = id_opt {
        remove_node(id).await;
    }
}

async fn add_child_at(
    parent_id: Option<NodeId>,
    parent_tk: TransformKey,
    index: usize,
    node: Arc<Node>,
) {
    // Insert the transform into the renderer + record in bridge state.
    let node_id = node.id;
    let transform_key = with_renderer_mut(|r| {
        r.transforms
            .insert(trs_to_transform(&node.transform.get()), Some(parent_tk))
    })
    .await;

    let entry = RendererNode::new(node.clone(), transform_key);

    bridge()
        .nodes
        .lock()
        .unwrap()
        .insert(node_id, entry.clone());

    {
        let bridge_handle = bridge();
        let mut order = bridge_handle.child_order.lock().unwrap();
        let level = order.entry(parent_id).or_default();
        let clamped = index.min(level.len());
        level.insert(clamped, node_id);
    }

    bridge().bump_nodes_revision();

    // Spawn per-node observers.
    spawn_transform_observer(node.clone(), entry.clone());
    spawn_kind_observer(node.clone(), entry.clone());
    spawn_children_observer(node.clone(), entry.clone());
    spawn_visibility_observer(node.clone(), entry.clone());

    // The newly-mounted node may have been hydrated from a snapshot
    // that already had `visible = false`, or may sit under an ancestor
    // that's already hidden. Either way, settle the renderer-side
    // state once now so we don't ship the first frame with a "leaked"
    // visible mesh / live light.
    apply_visibility_subtree(node_id);
}

async fn remove_node(node_id: NodeId) {
    let entry_opt = bridge().nodes.lock().unwrap().remove(&node_id);
    let Some(entry) = entry_opt else {
        return;
    };

    // Drop any failure record this node owned — gone from the scene means
    // gone from the missing-assets surface.
    app_state().clear_asset_failure(node_id);

    // Recursively tear down children first, so their sub-entries leave
    // the map before we drop the renderer transform.
    let child_ids: Vec<NodeId> = bridge()
        .child_order
        .lock()
        .unwrap()
        .remove(&Some(node_id))
        .unwrap_or_default();
    for child in child_ids {
        Box::pin(remove_node(child)).await;
    }

    // Drop observer tasks + in-flight asset load.
    entry.tasks.lock().unwrap().clear();
    entry.asset_loader.cancel();
    // Tear down any "playing" emitter runtime owned by this node.
    super::particles_sync::forget(node_id).await;

    // Clean up any glb sub-transforms + duplicated meshes we instanced
    // under this node.
    let sub_transforms: Vec<TransformKey> =
        std::mem::take(&mut *entry.model_transforms.lock().unwrap());
    let sub_meshes: Vec<awsm_renderer::meshes::MeshKey> =
        std::mem::take(&mut *entry.model_meshes.lock().unwrap());
    let material_keys: Vec<awsm_renderer::materials::MaterialKey> =
        std::mem::take(&mut *entry.material_keys.lock().unwrap());
    let line_keys: Vec<awsm_renderer::render_passes::lines::LineKey> =
        std::mem::take(&mut *entry.line_keys.lock().unwrap());
    let light_key = entry.light_key.lock().unwrap().take();
    let decal_key = entry.decal_key.lock().unwrap().take();

    // Remove from the per-frame sync indices regardless of whether
    // the key was set — set ops are O(1) and the "absent" case is
    // fine. Done before the renderer-mut hop so the next frame's
    // sync never sees a stale node id.
    {
        let bridge = bridge();
        bridge.light_node_ids.lock().unwrap().remove(&node_id);
        bridge.decal_node_ids.lock().unwrap().remove(&node_id);
        bridge.collider_node_ids.lock().unwrap().remove(&node_id);
        bridge.camera_node_ids.lock().unwrap().remove(&node_id);
    }

    with_renderer_mut(|r| {
        if let Some(key) = light_key {
            r.remove_light(key);
        }
        if let Some(key) = decal_key {
            r.remove_decal(key);
        }
        for mesh in sub_meshes {
            r.remove_mesh(mesh);
        }
        // Free owned inline materials *after* the meshes that referenced
        // them are gone, so no live draw still points at the slot.
        for mat in material_keys {
            r.remove_material(mat);
        }
        for line in line_keys {
            r.remove_line(line);
        }
        for tk in sub_transforms {
            r.transforms.remove(tk);
        }
        r.transforms.remove(entry.transform_key);
    })
    .await;

    bridge().bump_nodes_revision();
}

fn spawn_transform_observer(node: Arc<Node>, entry: Arc<RendererNode>) {
    let loader = AsyncLoader::new();
    let task_node = node.clone();
    let tk = entry.transform_key;
    loader.load(async move {
        use futures_signals::signal::SignalExt;
        task_node
            .transform
            .signal()
            .for_each(move |trs| {
                let t = trs_to_transform(&trs);
                async move {
                    with_renderer_mut(move |r| {
                        let _ = r.transforms.set_local(tk, t);
                    })
                    .await;
                }
            })
            .await;
    });
    entry.tasks.lock().unwrap().push(loader);
}

fn spawn_kind_observer(node: Arc<Node>, entry: Arc<RendererNode>) {
    let loader = AsyncLoader::new();
    let task_node = node.clone();
    let entry_for_task = entry.clone();
    loader.load(async move {
        use futures_signals::signal::SignalExt;
        task_node
            .kind
            .signal_cloned()
            .for_each(move |kind| {
                let entry = entry_for_task.clone();
                async move {
                    apply_kind(entry, kind).await;
                }
            })
            .await;
    });
    entry.tasks.lock().unwrap().push(loader);
}

fn spawn_children_observer(node: Arc<Node>, entry: Arc<RendererNode>) {
    let loader = AsyncLoader::new();
    let task_node = node.clone();
    let node_id = entry.node_id;
    loader.load(async move {
        use futures_signals::signal_vec::SignalVecExt;
        task_node
            .children
            .signal_vec_cloned()
            .for_each(move |diff| async move {
                handle_children_diff(Some(node_id), diff).await;
            })
            .await;
    });
    entry.tasks.lock().unwrap().push(loader);
}

/// Watch `node.visible`. When it flips, recompute effective visibility
/// for this node's whole subtree (descendants inherit the change) and
/// push the new state into the renderer per-kind.
fn spawn_visibility_observer(node: Arc<Node>, entry: Arc<RendererNode>) {
    let loader = AsyncLoader::new();
    let task_node = node.clone();
    let node_id = entry.node_id;
    loader.load(async move {
        use futures_signals::signal::SignalExt;
        task_node
            .visible
            .signal()
            .for_each(move |_| {
                // Apply on the same task so consecutive flips serialize.
                apply_visibility_subtree(node_id);
                async {}
            })
            .await;
    });
    entry.tasks.lock().unwrap().push(loader);
}

/// Walk the scene subtree rooted at `node_id` (inclusive) and reapply
/// effective visibility to each descendant. Called whenever any node's
/// own `visible` flag changes — descendants inherit ancestors so the
/// only correct response is to recompute the subtree.
///
/// One pre-order DFS pass: visit each node once, AND its own
/// `node.visible` into the inherited "ancestors-so-far visibility"
/// (computed once via [`ancestors_all_visible_to`] for the root). The
/// prior implementation walked all descendants, then re-walked each
/// descendant's ancestor chain to the root — O(N×depth) per subtree
/// flip. This is O(N + root-depth) — the descendant walk plus the
/// single ancestor walk for the root.
pub fn apply_visibility_subtree(node_id: NodeId) {
    let scene = app_state().scene.clone();
    let Some(root) = crate::scene::mutate::find_by_id(&scene, node_id) else {
        return;
    };
    // Inherited visibility for the root: AND of every ancestor's
    // `visible`. `find_parent` returns `None` at the scene root, which
    // is the implicit "always visible" sentinel.
    let inherited = ancestors_all_visible_to(&scene, node_id);

    // Batched mesh-visibility ops: collect every descendant whose
    // `effective_visible` actually flipped, then issue ONE
    // `with_renderer_mut` for the whole subtree. The prior
    // implementation spawned one `spawn_local` per descendant, paying
    // N renderer-lock acquires on every Group hide/show. With the
    // identity guard inside `update_visibility_entry_returning_meshes`,
    // most descendants don't contribute any ops at all.
    let mut pending_meshes: Vec<(awsm_renderer::meshes::MeshKey, bool)> = Vec::new();

    fn walk(
        node: &Arc<Node>,
        inherited: bool,
        pending_meshes: &mut Vec<(awsm_renderer::meshes::MeshKey, bool)>,
    ) {
        let effective = inherited && node.visible.get();
        if let Some(meshes) = update_visibility_entry_returning_meshes(node.id, effective) {
            for mesh in meshes {
                pending_meshes.push((mesh, effective));
            }
        }
        for child in node.children.lock_ref().iter() {
            walk(child, effective, pending_meshes);
        }
    }
    walk(&root, inherited, &mut pending_meshes);

    if pending_meshes.is_empty() {
        return;
    }
    spawn_local(async move {
        with_renderer_mut(move |r| {
            for (mesh, visible) in pending_meshes {
                let _ = r.set_mesh_hidden(mesh, !visible);
            }
        })
        .await;
    });
}

/// Updates the bridge entry's `effective_visible` to `visible`. Returns
/// the entry's mesh-key list iff visibility actually flipped AND the
/// entry has meshes — the caller (currently
/// [`apply_visibility_subtree`]) is responsible for pushing the
/// mesh-hide/show through the renderer.
fn update_visibility_entry_returning_meshes(
    node_id: NodeId,
    visible: bool,
) -> Option<Vec<awsm_renderer::meshes::MeshKey>> {
    let entry = bridge().nodes.lock().unwrap().get(&node_id).cloned()?;
    let prev = {
        let mut slot = entry.effective_visible.lock().unwrap();
        let prev = *slot;
        *slot = visible;
        prev
    };
    if prev == visible {
        return None;
    }
    let meshes: Vec<awsm_renderer::meshes::MeshKey> = entry.model_meshes.lock().unwrap().clone();
    if meshes.is_empty() {
        return None;
    }
    Some(meshes)
}

/// Walks strictly *above* `node_id` — returns true iff every ancestor
/// of `node_id` has `visible == true`. The node itself isn't checked.
/// Missing nodes short-circuit to `true` (matches the prior helper's
/// "nothing to AND, treat as visible" semantics for the root).
fn ancestors_all_visible_to(scene: &crate::scene::Scene, node_id: NodeId) -> bool {
    use crate::scene::mutate;
    let mut current = mutate::find_parent(scene, node_id);
    while let Some(node) = current {
        if !node.visible.get() {
            return false;
        }
        current = mutate::find_parent(scene, node.id);
    }
    true
}

async fn apply_kind(entry: Arc<RendererNode>, kind: NodeKind) {
    // F-A identity check: if this kind equals the last one we
    // materialized for the node, the bridge state is already correct
    // — bail without touching the renderer. `Mutable::set` always
    // emits regardless of equality, so the inspector's `number_input`
    // routinely writes back a value identical to what's already there
    // (e.g. focus+blur with no edit, scrub-wheel hitting the existing
    // value, undo restoring the same kind).
    {
        let last = entry.last_applied_kind.lock().unwrap();
        if last.as_ref() == Some(&kind) {
            return;
        }
    }
    // F9 fast path (SweepAlongCurve only): if the new kind is a Sweep
    // whose geometry inputs match the previous Sweep this node built,
    // skip the full rebuild and just swap the material binding. Avoids
    // re-evaluating `sweep_along_curve` (potentially tens of thousands
    // of verts) on every material-only edit during a drag.
    if try_sweep_material_only_update(&entry, &kind).await {
        *entry.last_applied_kind.lock().unwrap() = Some(kind);
        return;
    }
    // L5 fast path (ParticleEmitter only): if the new kind is a
    // ParticleEmitter and the previous one had matching structural
    // fields (`blend`, `max_alive`, `texture`), hot-swap the live
    // simulator's emitter snapshot in place. The simulator's
    // per-particle state (positions, velocities, remaining lifetimes)
    // survives, so a user can drag a slider in the inspector and watch
    // the running emitter react smoothly instead of restarting.
    if try_particle_param_only_update(&entry, &kind) {
        *entry.last_applied_kind.lock().unwrap() = Some(kind);
        return;
    }
    // L5+ structural fast path: same kind variant but a structural
    // field differs (e.g. blend toggle, max_alive resize, texture
    // swap). Renderer-side mesh + material need rebuilding, but the
    // simulator state itself is salvageable — lift it across the
    // rebuild so the particle stream doesn't restart, only the
    // render-pass / buffer-size / material identity changes. Also
    // covers the not-currently-playing case: nothing to rebuild but
    // we still want to skip the generic `forget` path, which would
    // drop the per-node `playing` Mutable the inspector is bound to.
    if try_particle_structural_smooth_rebuild(&entry, &kind).await {
        *entry.last_applied_kind.lock().unwrap() = Some(kind);
        return;
    }
    // Clear any previous kind's renderer-side state first so changing
    // a node's kind at runtime doesn't leak sub-transforms/meshes / lights.
    clear_model_instance(&entry).await;
    clear_light(&entry).await;
    clear_decal(&entry).await;
    clear_lines(&entry).await;
    super::particles_sync::forget(entry.node_id).await;

    // Any kind transition wipes a previous Model's failure record — if
    // the node is no longer a Model, it can't be missing an asset.
    app_state().clear_asset_failure(entry.node_id);

    // Remember this for the next identity check. Done before dispatch
    // so even early-returning paths (Group / Collider / Camera) get
    // their last-applied-kind state updated.
    *entry.last_applied_kind.lock().unwrap() = Some(kind.clone());

    // Maintain the per-frame index sets keyed by NodeKind variant.
    // The clear_* helpers already drop the entry from light_node_ids
    // / decal_node_ids; here we drop it from the collider / camera
    // indices on every transition (set removal is O(1) and the
    // "absent" case is a no-op) and re-add it in the new kind's arm
    // below. This way the bridge's per-frame walks
    // (`collider_wireframe::render::collect_*`) iterate only the
    // entries that own the matching kind, instead of the full
    // bridge table.
    {
        let bridge = bridge();
        bridge
            .collider_node_ids
            .lock()
            .unwrap()
            .remove(&entry.node_id);
        bridge
            .camera_node_ids
            .lock()
            .unwrap()
            .remove(&entry.node_id);
    }

    match kind {
        NodeKind::Group => {
            apply_kind_passive(&entry);
        }
        NodeKind::Collider(_) => {
            bridge()
                .collider_node_ids
                .lock()
                .unwrap()
                .insert(entry.node_id);
            apply_kind_passive(&entry);
        }
        NodeKind::Camera(_) => {
            bridge()
                .camera_node_ids
                .lock()
                .unwrap()
                .insert(entry.node_id);
            apply_kind_passive(&entry);
        }
        NodeKind::Primitive { .. }
        | NodeKind::Mesh { .. }
        | NodeKind::Curve(_)
        | NodeKind::SweepAlongCurve { .. }
        | NodeKind::InstancesAlongCurve(_)
        | NodeKind::Line(_)
        | NodeKind::Sprite(_)
        | NodeKind::ParticleEmitter(_) => {
            apply_kind_procedural(entry, kind).await;
        }
        NodeKind::Light(cfg) => apply_kind_light(entry, cfg).await,
        NodeKind::Model(model_ref) => apply_kind_model(entry, model_ref),
        NodeKind::Decal(cfg) => apply_kind_decal(entry, cfg).await,
    }
}

/// Pure transform / wireframe-only kinds. No renderer-side asset or
/// light handle allocated; the existing transform key already covers
/// what these need.
fn apply_kind_passive(entry: &Arc<RendererNode>) {
    *entry.asset_id.lock().unwrap() = None;
    entry.node.asset_status.set(AssetStatus::Idle);
}

/// Procedural variants — materializer in `procedural_sync` does the
/// real work. Mesh keys + sub-transforms land in `model_meshes` /
/// `model_transforms` so the standard `clear_model_instance` cleanup
/// path tears them down on the next kind change.
async fn apply_kind_procedural(entry: Arc<RendererNode>, kind: NodeKind) {
    *entry.asset_id.lock().unwrap() = None;
    entry.node.asset_status.set(AssetStatus::Idle);
    let parent_tk = entry.transform_key;
    super::procedural_sync::materialize_procedural(entry, kind, parent_tk).await;
}

/// Light kinds — insert a `Light` at the origin; the per-frame light
/// sync in `renderer_bridge.rs` pushes the right world pos/dir next
/// tick.
async fn apply_kind_light(entry: Arc<RendererNode>, cfg: crate::scene::LightConfig) {
    *entry.asset_id.lock().unwrap() = None;
    entry.node.asset_status.set(AssetStatus::Idle);
    let light = light_from_config(&cfg, Vec3::ZERO, Vec3::NEG_Z);
    let shadow_params = light_shadow_params_from_config(cfg.shadow());
    let casts_shadow = shadow_params.cast;
    let key = with_renderer_mut(move |r| {
        // Insert the light + register shadow params atomically — the
        // coordinated API ensures no frame can render between the
        // two inserts. `lights.mark_punctual_dirty()` (called via
        // `set_light_shadow_params`'s internal flag) isn't needed
        // here because the fresh insert already marks the buffer
        // dirty.
        r.insert_light(light, Some(shadow_params))
    })
    .await;
    // Block B.1 + B.2 lazy-compile trigger: when a casting light lands
    // we kick the shadow pipelines compile so the next render frame
    // can draw shadows. `with_renderer_mut`'s closure is sync — go
    // through the renderer handle's async lock directly so we can
    // `.await` `ensure_shadow_pipelines_compiled` while still holding
    // the lock. No-op if pipelines are already compiled or if no
    // casters are active.
    if casts_shadow {
        let handle = crate::context::renderer_handle();
        let mut renderer = handle.lock().await;
        if let Err(err) = renderer.ensure_shadow_pipelines_compiled().await {
            tracing::warn!(
                "scene-editor: ensure_shadow_pipelines_compiled failed: {:?}",
                err
            );
        }
    }
    if let Ok(key) = key {
        *entry.light_key.lock().unwrap() = Some(key);
        // Add to the per-frame sync index so the render-loop's
        // `sync_lights_pre_render` only touches actual light entries
        // instead of scanning the entire bridge node table.
        bridge()
            .light_node_ids
            .lock()
            .unwrap()
            .insert(entry.node_id);
    }
}

/// Model kinds — kick off the gltf load, then instance the matched
/// template node under the editor node's transform when the load
/// completes. Loading is parked on `entry.asset_loader` so a kind
/// change mid-load cancels the in-flight work via the standard
/// AsyncLoader drop semantics.
fn apply_kind_model(entry: Arc<RendererNode>, model_ref: crate::scene::ModelRef) {
    let asset_id = model_ref.asset_id;
    *entry.asset_id.lock().unwrap() = Some(asset_id);

    entry.node.asset_status.set(AssetStatus::Loading);

    // Resolve which gltf node + (optional) primitive index this editor
    // node represents up front — the kind is already known here, and
    // the materializer needs both values.
    let (node_index, primitive_index) = match &*entry.node.kind.lock_ref() {
        NodeKind::Model(r) => (r.node_index, r.primitive_index),
        _ => (0, None),
    };

    let entry_for_load = entry.clone();
    entry.asset_loader.load(async move {
        // Enqueue into the per-asset batcher. A single coordinator
        // task per `asset_id` will await `cache.get_or_load`, then
        // process every queued entry in one renderer-lock acquisition
        // — this is how the editor avoids 38 separate `lock().await`
        // calls competing with the render loop for a model with 38
        // primitives.
        let (tx, rx) = oneshot::channel();
        super::instance_batcher::enqueue(super::instance_batcher::PendingInstance {
            entry: entry_for_load,
            asset_id,
            node_index,
            primitive_index,
            done: tx,
        });
        // Keep the asset_loader task alive until the batch finishes
        // materializing us, so kind-change cancellation (which drops
        // the loader) reflects the right "in-flight" state. Drop of
        // `rx` is a no-op for the coordinator; it'll silently
        // discard the resulting `send(())`.
        let _ = rx.await;
    });
}

/// Public so the batched materializer can route failures through the
/// same editor-side bookkeeping the per-node path used: sets the
/// node's `asset_status = Failed` and records the missing asset in
/// the `failed_assets_by_node` map that drives the editor's missing-
/// assets indicator. Called from `instance_batcher::coordinator` when
/// `cache.get_or_load` resolves to an `Err`.
pub fn report_model_load_failure(entry: Arc<RendererNode>, asset_id: AssetId, err: String) {
    entry.node.asset_status.set(AssetStatus::Failed(err));
    let label = app_state()
        .scene
        .assets
        .lock()
        .unwrap()
        .display_name(asset_id)
        .map(|s| s.to_string())
        .unwrap_or_else(|| asset_id.to_string());
    app_state().report_asset_failed(entry.node_id, label);
}

/// F9 fast path: returns `true` if the incoming kind is a Sweep whose
/// geometry inputs match the node's stored hash AND we still have a
/// live mesh from the previous Sweep. In that case the only thing
/// that needs to happen is rebinding the material — no expensive
/// `sweep_along_curve` re-evaluation, no GPU vertex upload, no
/// transform churn.
///
/// Returns `false` and leaves the node untouched in every other case
/// (kind isn't Sweep, hash differs, no existing mesh, curve lookup
/// failed). The caller then takes the standard clear-then-materialize
/// path.
/// L5: ParticleEmitter param-only fast path. Hot-swaps the live
/// simulator's `Emitter` snapshot when only "param" fields changed —
/// preserving per-particle state (positions, velocities, lifetimes)
/// so an inspector drag plays out smoothly instead of restarting.
///
/// Structural fields (`blend`, `max_alive`, `texture`) fall through
/// to [`try_particle_structural_smooth_rebuild`], which preserves
/// simulator state across a true rebuild. The not-currently-playing
/// case also falls through there — that path no-ops cleanly while
/// keeping the per-node Play Mutable alive (`forget` in the generic
/// path would orphan it).
fn try_particle_param_only_update(entry: &Arc<RendererNode>, kind: &NodeKind) -> bool {
    let new_def = match kind {
        NodeKind::ParticleEmitter(def) => def,
        _ => return false,
    };
    let last_def = {
        let last = entry.last_applied_kind.lock().unwrap();
        match last.as_ref() {
            Some(NodeKind::ParticleEmitter(def)) => def.clone(),
            _ => return false,
        }
    };
    // `space` is a structural change: Local vs World picks different
    // parents for the instanced mesh's transform, which the hot-swap
    // path doesn't re-apply (it only mutates the simulator/Emitter
    // snapshot). Falling through to the structural rebuild path
    // re-runs `build_runtime_*` which picks the right parent.
    if new_def.blend != last_def.blend
        || new_def.max_alive != last_def.max_alive
        || new_def.texture != last_def.texture
        || new_def.space != last_def.space
    {
        return false;
    }
    super::particles_sync::hot_swap_emitter(entry.node_id, new_def)
}

/// L5+ ParticleEmitter structural-edit fast path. The L5 param-only
/// path catches cases where `blend` / `max_alive` / `texture` are
/// unchanged; this path handles the remaining ParticleEmitter →
/// ParticleEmitter transitions by lifting the live simulator state
/// across a renderer-side rebuild (mesh + material reconstructed on
/// the new def's render pass / buffer size / texture).
///
/// Returns `true` for every `ParticleEmitter → ParticleEmitter`
/// transition, INCLUDING the not-currently-playing case — that
/// branch no-ops but the `true` return prevents the generic clear
/// path from running `forget`, which would drop the per-node
/// `playing` Mutable the inspector is bound to. Returns `false`
/// only when the variant transitions in or out of `ParticleEmitter`
/// — those changes need the generic clear path so observer
/// registration / cleanup runs.
async fn try_particle_structural_smooth_rebuild(
    entry: &Arc<RendererNode>,
    kind: &NodeKind,
) -> bool {
    let new_def = match kind {
        NodeKind::ParticleEmitter(def) => def.clone(),
        _ => return false,
    };
    let old_was_particle = matches!(
        entry.last_applied_kind.lock().unwrap().as_ref(),
        Some(NodeKind::ParticleEmitter(_))
    );
    if !old_was_particle {
        return false;
    }
    let _ = super::particles_sync::try_rebuild_preserving_simulator(
        entry.node_id,
        entry.transform_key,
        &new_def,
    )
    .await;
    true
}

async fn try_sweep_material_only_update(entry: &Arc<RendererNode>, kind: &NodeKind) -> bool {
    let (def, material_ref, inline, shadow_cfg) = match kind {
        NodeKind::SweepAlongCurve {
            def,
            material,
            inline_material,
            shadow,
            ..
        } => (def.clone(), *material, inline_material.clone(), *shadow),
        _ => return false,
    };

    // Need an existing mesh + stored hash to take the fast path.
    let existing_mesh = entry.model_meshes.lock().unwrap().first().copied();
    let Some(existing_mesh) = existing_mesh else {
        return false;
    };
    let stored_hash = *entry.sweep_geometry_hash.lock().unwrap();
    let Some(stored_hash) = stored_hash else {
        return false;
    };

    let Some(curve_def) = super::procedural_sync::lookup_curve_def(def.curve_node) else {
        return false;
    };
    let new_hash = super::procedural_sync::sweep_geometry_hash(&def, &curve_def);
    if new_hash != stored_hash {
        return false;
    }

    // Geometry unchanged — only the material side and/or the shadow
    // flags may have changed. Free the previously-owned inline
    // material (if any), resolve the new material, rebind the
    // existing mesh to it, and re-apply the per-mesh shadow flags so
    // a toggle of Cast/Receive on a sweep node actually reaches the
    // renderer (without this, the fast path skipped the
    // `set_mesh_shadow_flags` that `procedural_sync` would otherwise
    // run after a full re-materialize).
    let old_owned: Vec<awsm_renderer::materials::MaterialKey> =
        std::mem::take(&mut *entry.material_keys.lock().unwrap());
    let entry_for_apply = entry.clone();
    let shadow_flags = mesh_shadow_flags_from_config(&shadow_cfg);
    with_renderer_mut(move |r| {
        let scene = app_state().scene.clone();
        let resolved = super::material_cache::resolve(r, &scene, material_ref, &inline);
        let new_material = resolved.key();
        if let Err(err) = r.set_mesh_material(existing_mesh, new_material) {
            tracing::warn!("node_sync (Sweep fast path): set_mesh_material failed: {err}");
        }
        if let Err(err) = r.set_mesh_shadow_flags(existing_mesh, shadow_flags) {
            tracing::warn!("node_sync (Sweep fast path): set_mesh_shadow_flags failed: {err}");
        }
        // Free the previously-owned inline material *after* the rebind
        // so the slot isn't briefly orphaned mid-frame.
        for k in old_owned {
            r.remove_material(k);
        }
        if let super::material_cache::ResolvedMaterial::Owned(k) = resolved {
            entry_for_apply.material_keys.lock().unwrap().push(k);
        }
    })
    .await;

    true
}

async fn clear_light(entry: &Arc<RendererNode>) {
    let key = entry.light_key.lock().unwrap().take();
    bridge()
        .light_node_ids
        .lock()
        .unwrap()
        .remove(&entry.node_id);
    if let Some(key) = key {
        with_renderer_mut(move |r| r.remove_light(key)).await;
    }
}

async fn clear_decal(entry: &Arc<RendererNode>) {
    let key = entry.decal_key.lock().unwrap().take();
    bridge()
        .decal_node_ids
        .lock()
        .unwrap()
        .remove(&entry.node_id);
    if let Some(key) = key {
        with_renderer_mut(move |r| {
            r.remove_decal(key);
        })
        .await;
    }
}

/// Decal kind — inserts a runtime decal at identity transform; the
/// per-frame `sync_decals_pre_render` pushes the actual world transform
/// (so identity here is just a placeholder until the next tick).
async fn apply_kind_decal(entry: Arc<RendererNode>, cfg: awsm_scene_schema::DecalConfig) {
    *entry.asset_id.lock().unwrap() = None;
    entry.node.asset_status.set(AssetStatus::Idle);
    let texture_index = decal_texture_index(&cfg);
    let alpha = cfg.alpha;
    let key =
        with_renderer_mut(move |r| r.insert_decal(glam::Mat4::IDENTITY, texture_index, alpha))
            .await;
    match key {
        Ok(key) => {
            *entry.decal_key.lock().unwrap() = Some(key);
            // Add to the per-frame sync index — sync_decals_pre_render
            // iterates this set instead of the whole bridge node table.
            bridge()
                .decal_node_ids
                .lock()
                .unwrap()
                .insert(entry.node_id);
        }
        Err(err) => {
            tracing::warn!("insert_decal failed: {err:?}");
        }
    }
}

/// Resolve a `DecalConfig`'s texture ref through the asset table /
/// texture cache to the packed `texture_index` the decal compute pass
/// expects (`array_index * 64 + layer_index`). Returns `0` if the
/// texture isn't uploaded yet — the decal stays inert with the
/// fallback magenta until the texture lands.
pub(crate) fn decal_texture_index(cfg: &awsm_scene_schema::DecalConfig) -> u32 {
    let Some(tex_ref) = cfg.texture else {
        return 0;
    };
    let Some(texture_key) = crate::renderer_bridge::texture_cache::lookup(tex_ref.0) else {
        return 0;
    };
    let handle = renderer_handle();
    let Some(renderer) = handle.try_lock() else {
        return 0;
    };
    match renderer.textures.get_entry(texture_key) {
        Ok(entry) => (entry.array_index as u32) * 64 + (entry.layer_index as u32),
        Err(_) => 0,
    }
}

async fn clear_lines(entry: &Arc<RendererNode>) {
    let keys: Vec<_> = std::mem::take(&mut *entry.line_keys.lock().unwrap());
    if keys.is_empty() {
        return;
    }
    with_renderer_mut(move |r| {
        for k in keys {
            r.remove_line(k);
        }
    })
    .await;
}

/// Schema → runtime conversion for a light's shadow configuration.
/// This is the only place in the codebase that performs this
/// translation; non-editor consumers construct `LightShadowParams`
/// directly.
pub fn light_shadow_params_from_config(
    cfg: &awsm_scene_schema::LightShadowConfig,
) -> awsm_renderer::shadows::LightShadowParams {
    use awsm_renderer::shadows as r;
    use awsm_scene_schema as s;
    r::LightShadowParams {
        cast: cfg.cast,
        depth_bias: cfg.depth_bias,
        normal_bias: cfg.normal_bias,
        resolution: cfg.resolution,
        hardness: match cfg.hardness {
            s::LightShadowHardness::Hard => r::LightShadowHardness::Hard,
            s::LightShadowHardness::Soft => r::LightShadowHardness::Soft,
            s::LightShadowHardness::Pcss => r::LightShadowHardness::Pcss,
        },
        pcss_penumbra_scale: cfg.pcss_penumbra_scale,
        max_distance: cfg.max_distance,
        cascade_count: cfg.cascade_count,
        cascade_split_lambda: cfg.cascade_split_lambda,
        evsm_cutoff: match cfg.evsm_cutoff {
            s::EvsmCutoff::Off => r::EvsmCutoff::Off,
            s::EvsmCutoff::LastCascade => r::EvsmCutoff::LastCascade,
            s::EvsmCutoff::LastTwoCascades => r::EvsmCutoff::LastTwoCascades,
        },
        far_cascade_update_rate: match cfg.far_cascade_update_rate {
            s::FarCascadeUpdateRate::EveryFrame => r::FarCascadeUpdateRate::EveryFrame,
            s::FarCascadeUpdateRate::Every2Frames => r::FarCascadeUpdateRate::Every2Frames,
            s::FarCascadeUpdateRate::Every4Frames => r::FarCascadeUpdateRate::Every4Frames,
            s::FarCascadeUpdateRate::Every8Frames => r::FarCascadeUpdateRate::Every8Frames,
        },
        cube_face_update_rate: match cfg.cube_face_update_rate {
            s::CubeFaceUpdateRate::EveryFrame => r::CubeFaceUpdateRate::EveryFrame,
            s::CubeFaceUpdateRate::Every2Frames => r::CubeFaceUpdateRate::Every2Frames,
            s::CubeFaceUpdateRate::Every4Frames => r::CubeFaceUpdateRate::Every4Frames,
            s::CubeFaceUpdateRate::Every8Frames => r::CubeFaceUpdateRate::Every8Frames,
        },
    }
}

/// Schema → runtime conversion for a mesh's shadow flags.
///
/// Wired into per-mesh creation sites in phase 2 once the renderer
/// actually consumes the flags.
#[allow(dead_code)]
pub fn mesh_shadow_flags_from_config(
    cfg: &awsm_scene_schema::MeshShadowConfig,
) -> awsm_renderer::shadows::MeshShadowFlags {
    awsm_renderer::shadows::MeshShadowFlags {
        cast: cfg.cast,
        receive: cfg.receive,
    }
}

fn light_from_config(
    cfg: &crate::scene::LightConfig,
    position: Vec3,
    direction: Vec3,
) -> awsm_renderer::lights::Light {
    use crate::scene::LightConfig;
    use awsm_renderer::lights::Light;
    match cfg {
        LightConfig::Directional {
            color, intensity, ..
        } => Light::Directional {
            color: *color,
            intensity: *intensity,
            direction: direction.to_array(),
        },
        LightConfig::Point {
            color,
            intensity,
            range,
            ..
        } => Light::Point {
            color: *color,
            intensity: *intensity,
            position: position.to_array(),
            range: *range,
        },
        LightConfig::Spot {
            color,
            intensity,
            range,
            inner_angle,
            outer_angle,
            ..
        } => Light::Spot {
            color: *color,
            intensity: *intensity,
            position: position.to_array(),
            direction: direction.to_array(),
            range: *range,
            inner_angle: *inner_angle,
            outer_angle: *outer_angle,
        },
    }
}

async fn clear_model_instance(entry: &Arc<RendererNode>) {
    let sub_transforms: Vec<TransformKey> =
        std::mem::take(&mut *entry.model_transforms.lock().unwrap());
    let sub_meshes: Vec<awsm_renderer::meshes::MeshKey> =
        std::mem::take(&mut *entry.model_meshes.lock().unwrap());
    let material_keys: Vec<awsm_renderer::materials::MaterialKey> =
        std::mem::take(&mut *entry.material_keys.lock().unwrap());
    // Tearing down the mesh invalidates the sweep fast-path hash —
    // the next Sweep build will repopulate it. Forgetting to clear
    // it here would let an unrelated transition (Sweep → Primitive
    // → Sweep with same hash) take the fast path on a Primitive mesh.
    *entry.sweep_geometry_hash.lock().unwrap() = None;
    if sub_transforms.is_empty() && sub_meshes.is_empty() && material_keys.is_empty() {
        return;
    }
    with_renderer_mut(move |r| {
        for mesh in sub_meshes {
            r.remove_mesh(mesh);
        }
        // Free owned inline materials *after* the meshes that referenced
        // them — keeps the materials slotmap flat across repeated kind
        // changes (was leaking one entry per re-materialize).
        for mat in material_keys {
            r.remove_material(mat);
        }
        for tk in sub_transforms {
            r.transforms.remove(tk);
        }
    })
    .await;
}

// `instance_template` was here. Model materialization is now batched
// per-glb in `super::instance_batcher` so the editor pays one
// renderer-lock acquisition per glb insert instead of N per-primitive.

pub fn trs_to_transform(trs: &Trs) -> Transform {
    Transform {
        translation: Vec3::from_array(trs.translation),
        rotation: Quat::from_array(trs.rotation),
        scale: Vec3::from_array(trs.scale),
    }
}
