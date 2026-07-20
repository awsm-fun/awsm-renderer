//! Scene→GPU sync: observe the reactive scene tree and materialize/teardown each
//! node's renderer resources (primitives, captured + skinned + morph meshes, sprites,
//! particles, instances, lights, cameras).
//!
//! ## Loading is ONE transaction
//!
//! A bulk scene load — a project reload (`apply_project` → `scene.nodes.replace_cloned`)
//! or an imported subtree, i.e. a `VecDiff::Replace` — is ONE transaction: declare the
//! WHOLE forest declare-only (the recursive `add_node(bulk_load=true)` join, with the
//! kind/children observers skipping their initial fire), THEN `commit_bulk_load` commits
//! ONCE. The commit dedups, runs concurrently, finalizes the texture pool, and recompiles
//! pipelines a single time — matching the player loader `populate_awsm_scene`. There is no
//! debounce, so no declared-but-unresolved window (which previously broke the decal
//! texture-pool bind group).
//!
//! A live add/edit (`InsertAt`/`Push`/`UpdateAt`, a kind/transform/material change) keeps
//! `bulk_load=false`: it declares AND commits per node, since the existing scene must stay
//! rendering while the one new node compiles.

use std::sync::Arc;

use awsm_renderer::meshes::buffer_info::MeshBufferGeometryMorphInfo;
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::raw_mesh::{RawMeshData, RawMorph, RawSkin};
use awsm_renderer::transforms::{Transform, TransformKey};
use awsm_renderer_meshgen::MeshData;
// Shared with the runtime-bundle loader (`populate_awsm_scene`) so a light lowers
// identically on the editor's live render and the player — the round-trip premise.
use awsm_renderer_scene_loader::camera::camera_params_from_config;
use awsm_renderer_scene_loader::light::{light_from_config, light_shadow_params_from_config};
use futures_signals::signal::SignalExt;
use futures_signals::signal_vec::{SignalVecExt, VecDiff};
use glam::{Mat4, Quat, Vec3, Vec4};

use super::{bridge, material, RendererNode};
use crate::engine::context::{renderer_handle, with_renderer_mut};
use crate::engine::scene::{AssetId, LightConfig, Node, NodeId, NodeKind, Trs};
use crate::prelude::*;

/// Begin mirroring the controller's scene root onto the renderer.
pub fn start() {
    let scene = controller().scene.clone();
    spawn_local(async move {
        scene
            .nodes
            .signal_vec_cloned()
            .for_each(|diff| async move {
                handle_diff(None, None, diff).await;
            })
            .await;
    });
}

/// Handle one diff on a children list. `parent_id`/`parent_tk` are `None` for the
/// scene root, `Some` for a node's children.
async fn handle_diff(
    parent_id: Option<NodeId>,
    parent_tk: Option<TransformKey>,
    diff: VecDiff<Arc<Node>>,
) {
    match diff {
        VecDiff::Replace { values } => {
            // Hold the `WaitRenderSettled` barrier open across the WHOLE bulk
            // materialization (teardown → transforms-first → declare-all →
            // commit). Like the kind observer's guard, this is raised on the
            // microtask queue — before a settled-wait's first (timer) poll —
            // so a driver can't observe "settled" in the gap between the
            // triggering command and this async processing. For a project
            // load the barrier was additionally armed SYNCHRONOUSLY inside
            // the command itself (`apply_project` →
            // [`arm_load_settle_barrier`]); this guard extends the same
            // coverage to every other bulk Replace (an imported subtree's
            // children, New Project).
            let _guard = crate::controller::CompileGuard::new();
            // Tear down whatever was there, then add all.
            for id in order_snapshot(parent_id) {
                remove_node(id).await;
            }
            order_reset(parent_id);
            // ⭐ TRANSACTION PRINCIPLE (§0): a bulk scene load (project reload) is one
            // transaction — establish the ENTIRE transform hierarchy of the new forest
            // BEFORE materialising any geometry, so a SkinnedMesh node's joint bones
            // (often in a SIBLING subtree) exist when its geometry is declared. Without
            // this, a skinned node could materialise before its bones → its skin can't
            // resolve the bone TransformKeys → renders empty. `add_node` below reuses
            // these pre-established transforms (idempotent). This is the ORDERING fix —
            // NOT a post-hoc re-materialise.
            establish_forest_transforms(parent_tk, values.clone()).await;
            // BULK LOAD: materialize the whole forest declare-only (add_node recurses
            // children directly + awaits — the JOIN), then commit ONCE. One transaction
            // for the project reload / import — no per-node commits (⭐ §0 / §5b).
            for (i, node) in values.into_iter().enumerate() {
                add_node(parent_id, parent_tk, i, node, true).await;
            }
            commit_bulk_load().await;
            // The scene is now FULLY populated + committed: drop the barrier
            // the load command armed (`apply_project`). Root list only — a
            // nested children Replace mid-flight must not release the load's
            // barrier early. No-op when nothing is armed (New Project).
            if parent_id.is_none() {
                release_load_settle_barrier();
            }
        }
        VecDiff::InsertAt { index, value } => {
            add_node(parent_id, parent_tk, index, value, false).await
        }
        VecDiff::Push { value } => {
            let index = order_len(parent_id);
            add_node(parent_id, parent_tk, index, value, false).await;
        }
        VecDiff::UpdateAt { index, value } => {
            if let Some(id) = order_get(parent_id, index) {
                remove_node(id).await;
            }
            // Replace the slot.
            remove_order_at(parent_id, index);
            add_node(parent_id, parent_tk, index, value, false).await;
        }
        VecDiff::RemoveAt { index } => {
            if let Some(id) = order_get(parent_id, index) {
                remove_node(id).await;
            }
            remove_order_at(parent_id, index);
        }
        VecDiff::Pop {} => {
            let len = order_len(parent_id);
            if len > 0 {
                if let Some(id) = order_get(parent_id, len - 1) {
                    remove_node(id).await;
                }
                remove_order_at(parent_id, len - 1);
            }
        }
        VecDiff::Move {
            old_index,
            new_index,
        } => {
            // Reorder tracking only (the renderer doesn't care about sibling
            // order); GPU resources are unaffected.
            let b = bridge();
            let mut co = b.child_order.lock().unwrap();
            if let Some(v) = co.get_mut(&parent_id) {
                if old_index < v.len() {
                    let id = v.remove(old_index);
                    let ni = new_index.min(v.len());
                    v.insert(ni, id);
                }
            }
        }
        VecDiff::Clear {} => {
            for id in order_snapshot(parent_id) {
                remove_node(id).await;
            }
            order_reset(parent_id);
        }
    }
}

fn order_len(parent_id: Option<NodeId>) -> usize {
    let b = bridge();
    let co = b.child_order.lock().unwrap();
    co.get(&parent_id).map(|v| v.len()).unwrap_or(0)
}
fn order_get(parent_id: Option<NodeId>, index: usize) -> Option<NodeId> {
    let b = bridge();
    let co = b.child_order.lock().unwrap();
    co.get(&parent_id).and_then(|v| v.get(index).copied())
}
fn order_insert(parent_id: Option<NodeId>, index: usize, id: NodeId) {
    let b = bridge();
    let mut co = b.child_order.lock().unwrap();
    let v = co.entry(parent_id).or_default();
    let i = index.min(v.len());
    v.insert(i, id);
}
fn remove_order_at(parent_id: Option<NodeId>, index: usize) {
    let b = bridge();
    let mut co = b.child_order.lock().unwrap();
    if let Some(v) = co.get_mut(&parent_id) {
        if index < v.len() {
            v.remove(index);
        }
    }
}
fn order_snapshot(parent_id: Option<NodeId>) -> Vec<NodeId> {
    let b = bridge();
    let co = b.child_order.lock().unwrap();
    co.get(&parent_id).cloned().unwrap_or_default()
}
fn order_reset(parent_id: Option<NodeId>) {
    let b = bridge();
    b.child_order.lock().unwrap().insert(parent_id, Vec::new());
}

/// The bridge-side parent of `id`, derived from `child_order` (the bridge keeps
/// no parent pointers). Returns `None` both for scene-root nodes and for nodes
/// not yet ordered (mid-load) — callers treat either as "top of the chain".
/// Toggle/materialize-path only, never per-frame.
fn parent_of(id: NodeId) -> Option<NodeId> {
    let b = bridge();
    let co = b.child_order.lock().unwrap();
    co.iter()
        .find(|(_, kids)| kids.contains(&id))
        .and_then(|(parent, _)| *parent)
}

/// EFFECTIVE visibility of a node: its own eye AND every ancestor's. The
/// renderer's mesh-hidden flag is FLAT (no scene-graph inheritance), so every
/// materialize path applies this AND at creation and the visibility observer
/// fans a toggle out to the whole subtree — without it, hiding a GROUP left
/// every descendant mesh/light rendering (the eye only worked on the node
/// carrying the geometry itself).
fn effective_visible(id: NodeId) -> bool {
    let mut cur = Some(id);
    while let Some(c) = cur {
        let own = {
            let b = bridge();
            let nodes = b.nodes.lock().unwrap();
            nodes.get(&c).map(|e| e.node.visible.get())
        };
        match own {
            Some(false) => return false,
            Some(true) => cur = parent_of(c),
            // Not bridged (shouldn't happen for a live chain): stop the walk.
            None => break,
        }
    }
    true
}

/// Push effective visibility for `root`'s whole subtree to the renderer:
/// meshes get `set_mesh_hidden(!effective)`, lights are removed/re-inserted
/// (there is no per-light hide flag). Each descendant's state is the AND of
/// its own eye with its ancestors' — a child hidden directly stays hidden
/// when its group re-shows.
async fn apply_subtree_visibility(root: NodeId) {
    let parent_eff = match parent_of(root) {
        Some(p) => effective_visible(p),
        None => true,
    };
    let mut mesh_ops: Vec<(MeshKey, bool)> = Vec::new();
    let mut light_on: Vec<Arc<RendererNode>> = Vec::new();
    let mut light_off: Vec<Arc<RendererNode>> = Vec::new();
    let mut stack = vec![(root, parent_eff)];
    while let Some((id, above)) = stack.pop() {
        let entry = {
            let b = bridge();
            let nodes = b.nodes.lock().unwrap();
            nodes.get(&id).cloned()
        };
        let Some(entry) = entry else { continue };
        let eff = above && entry.node.visible.get();
        for mk in entry.model_meshes.lock().unwrap().iter() {
            mesh_ops.push((*mk, !eff));
        }
        if matches!(entry.node.kind.get_cloned(), NodeKind::Light(_)) {
            if eff {
                light_on.push(entry.clone());
            } else {
                light_off.push(entry.clone());
            }
        }
        for kid in order_snapshot(Some(id)) {
            stack.push((kid, eff));
        }
    }
    if !mesh_ops.is_empty() {
        with_renderer_mut(move |r| {
            for (mk, hidden) in mesh_ops {
                let _ = r.set_mesh_hidden(mk, hidden);
            }
        })
        .await;
    }
    let mut light_churned = false;
    for e in light_off {
        let taken = e.light_key.lock().unwrap().take();
        if let Some(lk) = taken {
            with_renderer_mut(move |r| r.remove_light(lk)).await;
            light_churned = true;
        }
    }
    for e in light_on {
        // Re-insert only if the hide arm removed it (an already-lit light
        // keeps its key untouched).
        if e.light_key.lock().unwrap().is_none() {
            if let NodeKind::Light(cfg) = e.node.kind.get_cloned() {
                apply_light(e.clone(), cfg).await;
                light_churned = true;
            }
        }
    }
    if light_churned {
        // LightKeys churned — re-lower so animation channels rebind.
        super::animation_sync::schedule_relower();
    }
}

/// Establish a node's renderer transform + bridge entry — the transforms-first half
/// of the load transaction (⭐ TRANSACTION PRINCIPLE, §0). NO geometry, NO observers.
/// Idempotent: returns the existing transform key if the node was already established
/// (so `add_node` reuses it). Lets [`establish_forest_transforms`] declare the whole
/// transform hierarchy before any geometry, so a skinned mesh's joint bones exist when
/// its geometry is declared (no re-materialise).
async fn establish_transform_only(
    node: &Arc<Node>,
    parent_tk: Option<TransformKey>,
) -> TransformKey {
    let node_id = node.id;
    if let Some(tk) = bridge()
        .nodes
        .lock()
        .unwrap()
        .get(&node_id)
        .map(|e| e.transform_key)
    {
        return tk;
    }
    let trs = node.transform.get();
    let tk =
        with_renderer_mut(move |r| r.transforms.insert(trs_to_transform(&trs), parent_tk)).await;
    let entry = RendererNode::new(node.clone(), tk);
    bridge().nodes.lock().unwrap().insert(node_id, entry);
    tk
}

/// Recursively establish the transform hierarchy of a node forest BEFORE any geometry
/// materialises (⭐ TRANSACTION PRINCIPLE, §0): transforms declared in dependency order
/// (parent before child, ALL of them before geometry). Used on a bulk scene load (the
/// `Replace` diff at the scene root — project reload), so a `SkinnedMesh` node's joint
/// bones (which may be in a SIBLING subtree) exist when its geometry is declared. This
/// is the ordering fix for skinned save→reload — NOT a post-hoc re-materialise pass.
fn establish_forest_transforms(
    parent_tk: Option<TransformKey>,
    nodes: Vec<Arc<Node>>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> {
    Box::pin(async move {
        for node in nodes {
            let tk = establish_transform_only(&node, parent_tk).await;
            let children: Vec<Arc<Node>> = node.children.lock_ref().iter().cloned().collect();
            establish_forest_transforms(Some(tk), children).await;
        }
    })
}

/// The single `commit_load` that closes a bulk load (the `Replace` join): after the
/// whole forest has been DECLARED (declare-only `add_node`s), commit once — the commit
/// dedups, runs concurrently, and recompiles the texture pool / pipelines ONCE. No
/// debounce, so no declared-but-unresolved window (which broke the decal texture-pool
/// bind group — §5b).
async fn commit_bulk_load() {
    let handle = renderer_handle();
    let mut r = handle.lock().await;
    if let Err(e) = r
        .commit_load(crate::engine::activity::commit_phase_handler())
        .await
    {
        tracing::warn!("bulk-load commit_load: {e}");
    }
}

thread_local! {
    /// Count of ARMED load-settle barriers — one per in-flight `apply_project`
    /// (project load / in-memory reload). See [`arm_load_settle_barrier`].
    static LOAD_SETTLE_ARMED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Raise the `WaitRenderSettled` barrier for a whole bulk scene load,
/// SYNCHRONOUSLY inside the load command (`apply_project` calls this right
/// before swapping the scene forest via `replace_cloned`). The matching release
/// happens in `handle_diff`'s ROOT `Replace` arm after the single bulk-load
/// `commit_load` completes — so a driver that dispatches `LoadProjectFromUrl`
/// (or `ReloadProjectInMemory`) and then queries `wait_render_settled` observes
/// the FULLY populated scene, instead of settling in the gap before the async
/// Replace materialization starts (loading is ONE transaction — the §5b
/// observability half; drivers previously had to poll node counts).
///
/// Deadlock-safe by construction: arming happens only immediately before
/// `replace_cloned`, whose `Replace` diff is always delivered to the
/// boot-started `node_sync` observer (which releases regardless of the commit's
/// Ok/Err), and load failures BEFORE `apply_project` never arm.
pub(crate) fn arm_load_settle_barrier() {
    crate::controller::compile_begin();
    LOAD_SETTLE_ARMED.with(|c| c.set(c.get() + 1));
}

/// Release one armed load-settle barrier. No-op when none is armed (a root
/// `Replace` that wasn't a project load — e.g. New Project), so it can never
/// underflow `compile_pending`.
fn release_load_settle_barrier() {
    LOAD_SETTLE_ARMED.with(|c| {
        let n = c.get();
        if n > 0 {
            c.set(n - 1);
            crate::controller::compile_end();
        }
    });
}

async fn add_node(
    parent_id: Option<NodeId>,
    parent_tk: Option<TransformKey>,
    index: usize,
    node: Arc<Node>,
    bulk_load: bool,
) {
    let node_id = node.id;
    // Reuse a transform established by a transforms-first pre-pass
    // ([`establish_forest_transforms`]) if present — so a node's transform exists
    // before its (or a sibling's) geometry materialises (⭐ §0). Otherwise establish
    // it now (the incremental single-node add path — unchanged).
    let entry = {
        let existing = bridge().nodes.lock().unwrap().get(&node_id).cloned();
        match existing {
            Some(e) => e,
            None => {
                let trs = node.transform.get();
                let tk = with_renderer_mut(move |r| {
                    r.transforms.insert(trs_to_transform(&trs), parent_tk)
                })
                .await;
                let e = RendererNode::new(node.clone(), tk);
                bridge().nodes.lock().unwrap().insert(node_id, e.clone());
                e
            }
        }
    };
    let tk = entry.transform_key;
    order_insert(parent_id, index, node_id);

    // A freshly materialized node may be the missing dependency of a PENDING
    // animation channel (lowering skips channels whose target node isn't in the
    // bridge yet — e.g. clips registering before their import's bone mirrors
    // finish landing). Nudge the debounced re-lower so those channels resolve;
    // bursts (a whole rig materializing) coalesce into one rebuild, and the
    // relower is a cheap no-op when no clips exist.
    super::animation_sync::schedule_relower();

    // BULK LOAD (project reload / import = a `Replace` diff): materialize this node's
    // geometry NOW, declare-only (the JOIN — awaited, no commit), then recurse its
    // children directly. The observers below SKIP their initial fire (already done
    // here). The caller (`handle_diff`'s `Replace` arm) commits ONCE after the whole
    // forest declares — ONE transaction, no per-node commits, no texture-pool-grow
    // window (⭐ §0 / §5b). Live add (`InsertAt`/`Push`/`UpdateAt`) passes
    // `bulk_load=false`: the kind/children observers fire their initial as before.
    if bulk_load {
        let kind = node.kind.get_cloned();
        apply_kind(entry.clone(), kind, true).await;
        let children: Vec<Arc<Node>> = node.children.lock_ref().iter().cloned().collect();
        for (ci, child) in children.into_iter().enumerate() {
            Box::pin(add_node(Some(node_id), Some(tk), ci, child, true)).await;
        }
    }

    // Kind observer — re-materializes on any kind change (declare+commit, a live
    // edit). On a bulk load the initial value was already materialized above, so
    // skip the first emission.
    {
        let loader = AsyncLoader::new();
        loader.load(clone!(entry => async move {
            let first = std::cell::Cell::new(bulk_load);
            entry.node.kind.signal_cloned().for_each(clone!(entry => move |kind| {
                let skip = first.replace(false);
                clone!(entry => async move {
                    if !skip {
                        // Hold the `WaitRenderSettled` barrier open for the whole
                        // re-materialization. A kind edit (patch_kind /
                        // assign_material / update_builtin_material's same-value
                        // resync) lands here ASYNC after its command returns; the
                        // signal wakes on the microtask queue, so this guard is
                        // visible before a settled-wait's first (timer) poll —
                        // closing the "settled in 32 ms while the variant was
                        // still recompiling" race for MCP screenshot-after-edit.
                        let _guard = crate::controller::CompileGuard::new();
                        apply_kind(entry, kind, false).await;
                    }
                })
            })).await;
        }));
        entry.loaders.lock().unwrap().push(loader);
    }
    // Transform observer — push local transform changes to the renderer.
    {
        let loader = AsyncLoader::new();
        loader.load(clone!(entry => async move {
            entry.node.transform.signal().for_each(move |trs| {
                clone!(entry => async move {
                    let tk = entry.transform_key;
                    with_renderer_mut(move |r| {
                        let _ = r.transforms.set_local(tk, trs_to_transform(&trs));
                    }).await;
                    // A collider's wireframe is world-baked line geometry (not
                    // parented to the node transform), so a move/rotate leaves it
                    // stale — re-bake it from the fresh transform so the on-screen
                    // affordance keeps matching where the collider actually is.
                    // A decal is the same shape twice over: the renderer decal
                    // carries a world-baked inverse_transform + AABB and its
                    // volume wireframe is world-baked lines — without this
                    // re-bake a moved decal keeps projecting at its
                    // materialize-time placement (while the gizmo moves on).
                    match entry.node.kind.get_cloned() {
                        NodeKind::Collider(shape) => {
                            materialize_collider(entry.clone(), shape).await;
                        }
                        NodeKind::Decal(cfg) => {
                            // Live re-bake (never a bulk load): the texture is
                            // already resolved, so the cache-hit probe inside
                            // skips the commit.
                            materialize_decal(entry.clone(), cfg, false).await;
                        }
                        _ => {}
                    }
                })
            }).await;
        }));
        entry.loaders.lock().unwrap().push(loader);
    }
    // Visibility observer — a toggle fans out to the node's WHOLE SUBTREE with
    // EFFECTIVE visibility (own eye AND ancestors' — the renderer's hidden flag
    // is flat, so group-hide must reach every descendant); for a Light node the
    // eye actually turns the light OFF/ON (remove/re-insert the renderer
    // light — there is no per-light hide flag), so hiding a light darkens the
    // scene instead of silently doing nothing.
    {
        let loader = AsyncLoader::new();
        loader.load(clone!(entry => async move {
            // Skip the initial (current-value) emission entirely: every
            // materialize path applies effective visibility at creation
            // (meshes via `set_mesh_hidden`, `apply_light` early-returns when
            // hidden), and reacting to the initial fire here could walk a
            // subtree that is still mid-load (children not yet ordered) or
            // double-insert a light against the in-flight kind observer.
            let first = std::cell::Cell::new(true);
            entry.node.visible.signal().for_each(move |_visible| {
                let initial = first.replace(false);
                clone!(entry => async move {
                    if initial {
                        return;
                    }
                    apply_subtree_visibility(entry.node_id).await;
                })
            }).await;
        }));
        entry.loaders.lock().unwrap().push(loader);
    }
    // Children observer — recurse for nested nodes. On a bulk load the children were
    // already materialized by the direct recursion above, so skip the INITIAL
    // `Replace` (subsequent live child diffs still process normally).
    {
        let loader = AsyncLoader::new();
        loader.load(clone!(node => async move {
            let first_replace = std::cell::Cell::new(bulk_load);
            node.children.signal_vec_cloned().for_each(move |diff| {
                let skip = matches!(diff, VecDiff::Replace { .. }) && first_replace.replace(false);
                clone!(node_id => async move {
                    if !skip { handle_diff(Some(node_id), Some(tk), diff).await; }
                })
            }).await;
        }));
        entry.loaders.lock().unwrap().push(loader);
    }
}

async fn remove_node(node_id: NodeId) {
    // Remove any descendants first.
    for child in order_snapshot(Some(node_id)) {
        Box::pin(remove_node(child)).await;
    }
    {
        let b = bridge();
        b.child_order.lock().unwrap().remove(&Some(node_id));
    }

    let entry = {
        let b = bridge();
        let e = b.nodes.lock().unwrap().remove(&node_id);
        e
    };
    if let Some(entry) = entry {
        // Real deletion: reclaim the node's textures too (glTF leak fix).
        teardown(&entry, true).await;
        // Free the node's OWN transform. `teardown` only frees sub-transforms
        // (`model_transforms`); the node's `transform_key` is a SlotMap key into
        // `r.transforms`, and dropping the `Arc<RendererNode>` below does NOT free
        // that renderer slot — so without this the transform leaked (+1 per
        // inserted-then-deleted node, verified via memory_stats). Children were
        // already removed by the recursion above, so nothing still parents off it.
        let tk = entry.transform_key;
        with_renderer_mut(move |r| {
            r.transforms.remove(tk);
        })
        .await;
        // Skin-bridge cleanup: drop any baked-joint mapping this node had (no-op
        // for non-bone nodes) so a deleted skinned-model bone doesn't linger.
        bridge().unregister_skin_joint(node_id);
        // Template reclamation (mid-session leak fix): node teardown deliberately
        // leaves an import's populate-baked resources alone (skinned meshes are
        // template-owned + deform live; static hidden copies survive their
        // captured siblings). Reclaim them — meshes, their materials → pooled
        // textures, baked transforms — once the LAST instance of an import is
        // gone. Candidate templates: this node's tracked import id, and (for a
        // skinned node) the template it renders from.
        reclaim_templates_for_removed(&entry, node_id).await;
        // Free the view-only cluster DAG cache for a deleted ClusterMesh node (last
        // reference only) — closes the editor-side session leak that otherwise grows
        // the wasm heap toward an OOM abort on re-import-heavy sessions.
        reclaim_cluster_cache_for_removed(&entry);
        // Dropping the entry (and its loaders) cancels the observers.
    }
}

/// Free the populate-baked renderer resources of any import whose **last**
/// instance just got deleted. Dangle-free: a template is freed only when no
/// tracked instance remains AND no live `SkinnedMesh` (e.g. a duplicate) still
/// renders from it (`Bridge::any_live_skinned_from`).
/// Whether the AUTHORED scene (controller, updated synchronously by mutations +
/// `apply_project`) still has a `SkinnedMesh` referencing `aid`. Used by template
/// reclamation as a reload-safe guard (see `reclaim_templates_for_removed`):
/// during a project reload the bridge nodes lag (async re-materialize) but the
/// scene already holds the new SkinnedMesh nodes.
fn scene_has_skinned_from(aid: AssetId) -> bool {
    fn walk(node: &Arc<crate::engine::scene::node::Node>, aid: AssetId) -> bool {
        if let NodeKind::SkinnedMesh { skin, .. } = node.kind.get_cloned() {
            if skin.source == aid {
                return true;
            }
        }
        node.children.lock_ref().iter().any(|c| walk(c, aid))
    }
    controller()
        .scene
        .nodes
        .lock_ref()
        .iter()
        .any(|n| walk(n, aid))
}

/// Whether the AUTHORED scene still has a `ClusterMesh` referencing `source`.
/// Mirror of [`scene_has_skinned_from`] for view-only cluster meshes: keeps the
/// `cluster_cache` free reload-safe (on `apply_project` the new nodes with the same
/// source are already in `controller().scene`) and duplicate-safe (a duplicated
/// `ClusterMesh` shares the source, so deleting one must not free the DAG the other
/// still renders from).
fn scene_has_cluster_from(source: AssetId) -> bool {
    fn walk(node: &Arc<crate::engine::scene::node::Node>, source: AssetId) -> bool {
        if let NodeKind::ClusterMesh { cluster, .. } = node.kind.get_cloned() {
            if cluster.source == source {
                return true;
            }
        }
        node.children.lock_ref().iter().any(|c| walk(c, source))
    }
    controller()
        .scene
        .nodes
        .lock_ref()
        .iter()
        .any(|n| walk(n, source))
}

/// Free the parsed cluster-LOD DAG ([`super::cluster_cache`]) of a deleted
/// `ClusterMesh` node once **nothing** still references its source. Without this the
/// ~tens-of-MB parsed DAG leaks for the whole session — and since each import mints
/// a fresh `AssetId`, re-importing the same `.clusters.bin` accumulates a new entry
/// every time, growing the wasm heap until `memory.grow` fails the V8 backing-store
/// allocation and the renderer aborts (FatalProcessOutOfMemory). The renderer-side
/// GPU state is already freed by `remove_mesh` → `drop_cluster_lod_for_mesh`; this
/// closes the editor-side half. Reload-safe + duplicate-safe via the authored-scene
/// check (see [`scene_has_cluster_from`]).
fn reclaim_cluster_cache_for_removed(entry: &Arc<RendererNode>) {
    if let NodeKind::ClusterMesh { cluster, .. } = entry.node.kind.get_cloned() {
        if !scene_has_cluster_from(cluster.source) {
            super::cluster_cache::remove(cluster.source);
            tracing::debug!(
                "freed cluster cache for {:?} (last ClusterMesh node deleted)",
                cluster.source
            );
        }
    }
}

async fn reclaim_templates_for_removed(entry: &Arc<RendererNode>, node_id: NodeId) {
    let mut candidates: Vec<AssetId> = Vec::new();
    if let Some(aid) = bridge().untrack_template_node(node_id) {
        candidates.push(aid);
    }
    if let NodeKind::SkinnedMesh { skin, .. } = entry.node.kind.get_cloned() {
        if !candidates.contains(&skin.source) {
            candidates.push(skin.source);
        }
    }
    for aid in candidates {
        // Don't reclaim a template anything still references. Three checks:
        //  - tracked instances (import-time registration),
        //  - a live materialized SkinnedMesh in the bridge,
        //  - a SkinnedMesh in the AUTHORED SCENE referencing it. The scene check
        //    is what keeps a project RELOAD safe: `apply_project` swaps the scene
        //    nodes SYNCHRONOUSLY (old removed → this async teardown's reclaim runs,
        //    but the new same-id SkinnedMesh is already in `controller().scene`),
        //    so the freshly re-populated template (slice-3 persistence) survives.
        //    On a genuine DELETE the scene node is already gone → reclaim proceeds.
        if bridge().template_instance_count(aid) > 0
            || bridge().any_live_skinned_from(aid)
            || scene_has_skinned_from(aid)
        {
            continue;
        }
        let Some(template) = bridge().get_template(aid) else {
            continue;
        };
        with_renderer_mut(move |r| {
            super::asset_template::remove_template_meshes(r, &template);
        })
        .await;
        bridge().remove_template(aid);
        super::skinned_bake_cache::remove(aid);
        tracing::debug!("reclaimed import template {aid:?} (last instance deleted)");
    }
}

/// Tear down a node's GPU resources (meshes / sub-transforms / owned materials /
/// light). Deliberately leaves the node's own `transform_key` alone so a kind
/// change (re-materialize) keeps a stable transform; when the node is actually
/// deleted, `remove_node` frees that `transform_key` explicitly after this.
///
/// `reclaim_textures`: a RE-MATERIALIZE (`false`) must KEEP the material's
/// textures — the immediate rebuild re-references them by key from the session
/// texture cache, and freeing them here would leave the rebuilt material reading
/// a dead `TextureKey` as absent → the mesh renders untextured (§1: a UV
/// transform / any edit on a textured built-in material made the texture vanish).
/// An actual node DELETE (`true`) reclaims them (the glTF leak fix).
async fn teardown(entry: &Arc<RendererNode>, reclaim_textures: bool) {
    let meshes: Vec<_> = entry.model_meshes.lock().unwrap().drain(..).collect();
    let transforms: Vec<_> = entry.model_transforms.lock().unwrap().drain(..).collect();
    let materials: Vec<_> = entry.material_keys.lock().unwrap().drain(..).collect();
    let lines: Vec<_> = entry.line_keys.lock().unwrap().drain(..).collect();
    let decals: Vec<_> = entry.decal_keys.lock().unwrap().drain(..).collect();
    let light = entry.light_key.lock().unwrap().take();
    let camera = entry.camera_key.lock().unwrap().take();
    let node_id = entry.node_id;
    for mk in &meshes {
        bridge().unregister_mesh(*mk);
    }
    with_renderer_mut(move |r| {
        for mk in meshes {
            r.remove_mesh(mk);
        }
        for tk in transforms {
            r.transforms.remove(tk);
        }
        for mat in materials {
            if reclaim_textures {
                r.remove_material(mat);
            } else {
                r.remove_material_keep_textures(mat);
            }
        }
        for lk in lines {
            r.remove_line(lk);
        }
        for dk in decals {
            r.remove_decal(dk);
        }
        // Free any particle-emitter runtime this node owns (no-op otherwise).
        super::particles::teardown(r, node_id);
        if let Some(lk) = light {
            r.remove_light(lk);
        }
        if let Some(ck) = camera {
            r.cameras.remove(ck);
        }
    })
    .await;
    bridge().light_node_ids.lock().unwrap().remove(&node_id);
}

/// Materialize (or re-materialize) a node for its current kind.
/// Materialize a node's `kind` onto the renderer. `declare_only`: when true, the
/// geometry/material is DECLARED into the open load transaction but NOT committed —
/// the bulk-load (`Replace`) path materializes the whole forest declare-only, then
/// commits ONCE (the ⭐ TRANSACTION PRINCIPLE; see `add_node` `bulk_load`). When false
/// (live add / live edit), it declares AND commits, as before.
async fn apply_kind(entry: Arc<RendererNode>, kind: NodeKind, declare_only: bool) {
    // Camera → Camera: update the params IN PLACE so the `CameraKey` stays
    // stable. Editing a camera param re-emits `node.kind`, but a numeric
    // `SetKind` doesn't bump `anim_revision`, so a lowered
    // `AnimationTarget::Camera { camera }` channel never re-lowers — a
    // teardown + re-insert here would churn the key and strand that target on a
    // freed slot. The camera store is purpose-built for this (it holds the
    // params the animation channel drives). The key is only freed when the node
    // is deleted or changes away from `Camera` (handled by `teardown` below /
    // `remove_node`).
    if let NodeKind::Camera(cfg) = &kind {
        let existing = *entry.camera_key.lock().unwrap();
        if let Some(ck) = existing {
            // The in-place path assumes a camera node owns nothing else that
            // `teardown` would normally free (only the camera key). If a future
            // kind gives camera nodes extra GPU resources, this early return
            // would leak them — trip it in tests.
            debug_assert!(
                entry.model_meshes.lock().unwrap().is_empty()
                    && entry.material_keys.lock().unwrap().is_empty()
                    && entry.light_key.lock().unwrap().is_none(),
                "camera node unexpectedly owns non-camera GPU resources"
            );
            let params = camera_params_from_config(cfg);
            with_renderer_mut(move |r| {
                r.cameras.update(ck, |p| *p = params);
            })
            .await;
            // Keep `last_kind` in step with the applied kind, exactly as the
            // normal path does after its match arm.
            *entry.last_kind.lock().unwrap() = Some(entry.node.kind.get_cloned());
            return;
        }
    }

    // Tear down the previous materialization (no-op on first apply). KEEP the old
    // material's textures — the rebuild below re-references them by key from the
    // session cache; reclaiming here would make the rebuilt mesh render untextured.
    teardown(&entry, false).await;

    // The mesh kinds render their SELECTED variant's instance (magenta when
    // none selected / empty palette).
    let selected_material = kind.selected_material().cloned();
    match kind {
        NodeKind::Light(cfg) => apply_light(entry.clone(), cfg).await,
        NodeKind::Line(def) => materialize_line(entry.clone(), def).await,
        NodeKind::Curve(def) => materialize_curve_viz(entry.clone(), def).await,
        NodeKind::Sprite(def) => materialize_sprite(entry.clone(), def, declare_only).await,
        NodeKind::Collider(shape) => materialize_collider(entry.clone(), shape).await,
        NodeKind::Decal(cfg) => materialize_decal(entry.clone(), cfg, declare_only).await,
        NodeKind::InstancesAlongCurve(def) => {
            materialize_instances(entry.clone(), def, declare_only).await
        }
        NodeKind::Instancer(def) => materialize_instancer(entry.clone(), def, declare_only).await,
        NodeKind::ParticleEmitter(def) => {
            materialize_particle(entry.clone(), def, declare_only).await
        }
        // The sole procedural-geometry path: read the baked stack from the mesh
        // cache + upload with the node's assigned material (magenta when None).
        // Primitives + sweeps are now `MeshDef` stacks behind this same arm.
        // GEOMETRY SHARING (axis 4, static): when another live node already
        // materialized this exact mesh ASSET, duplicate its mesh over the
        // shared resource instead of re-uploading — N duplicates (or a
        // reloaded project full of them) keep ONE geometry upload. Vertex
        // edits stay consistent: they re-bake the asset's cache and
        // re-materialize every node using it, so shared-asset semantics are
        // unchanged.
        NodeKind::Mesh { mesh, .. } => {
            if materialize_static_duplicate(
                &entry,
                mesh.0,
                selected_material.as_ref(),
                declare_only,
            )
            .await
            {
                // shared a donor's geometry — nothing to upload
            } else {
                match super::mesh_cache::get_raw(mesh.0) {
                    Some(raw) => {
                        upload_simple_mesh(
                            entry.clone(),
                            raw,
                            MeshMaterial::Assigned(selected_material),
                            declare_only,
                        )
                        .await;
                    }
                    None => {
                        tracing::warn!(
                            "NodeKind::Mesh {mesh:?}: not in the capture cache; renders empty"
                        )
                    }
                }
            }
        }
        // A skinned glTF import: the renderer's `populate_gltf` already built the
        // skinned mesh + skeleton; we keep that copy rendering (it deforms via the
        // joints) and just (re)assign this node's material/shadow to it. NOT the
        // captured-mesh pipeline — skinned geometry isn't editable.
        NodeKind::SkinnedMesh { skin, .. } => {
            materialize_skinned_mesh(entry.clone(), skin, selected_material, declare_only).await
        }
        // A view-only pre-baked cluster mesh: materialize through the SAME cluster
        // path the player uses (no in-editor re-bake, no dense explode). Cluster
        // data comes from the import-time `cluster_cache`.
        NodeKind::ClusterMesh { cluster, .. } => {
            materialize_cluster_mesh_node(entry.clone(), cluster, selected_material, declare_only)
                .await
        }
        NodeKind::Camera(cfg) => materialize_camera(entry.clone(), cfg).await,
        // Group: no procedural geometry, no renderer resource.
        _ => {}
    }

    *entry.last_kind.lock().unwrap() = Some(entry.node.kind.get_cloned());
}

/// Resolve a built-in material slot's texture binding (§11 priority). The
/// per-mesh **inline** texture (what `set_node_texture` /
/// `set_node_texture_transform` write) wins and ENABLES the slot even when the
/// shared variant lacks it; otherwise a custom `texture_overrides` swap; otherwise
/// the shared variant's default image. Pure (no controller state) so it is
/// unit-tested directly — the regression guard for the §11 "bound texture renders
/// flat" silent failure.
fn merge_slot_texture(
    inline_tex: Option<awsm_renderer_editor_protocol::TextureRef>,
    override_tex: Option<awsm_renderer_editor_protocol::TextureRef>,
    variant_default: Option<awsm_renderer_editor_protocol::TextureRef>,
) -> Option<awsm_renderer_editor_protocol::TextureRef> {
    inline_tex.or(override_tex).or(variant_default)
}

/// If `id` names a **built-in** library material, merge its shared variant
/// settings (shading / alpha / double-sided / vertex-colors / texture-enables)
/// with this mesh's per-mesh uniform values (`inline`: base color / metallic /
/// roughness / emissive) into a final `MaterialDef`. Returns `None` for a dynamic
/// material or an unknown id (callers then try the dynamic path / inline).
pub(crate) fn builtin_merged(
    inst: &awsm_renderer_editor_protocol::dynamic_material::MaterialInstance,
) -> Option<awsm_renderer_editor_protocol::MaterialDef> {
    let ctrl = crate::controller::controller();
    let mat =
        crate::controller::custom_material::find_material(&ctrl.custom_materials, inst.asset)?;
    let variant = mat.builtin.get_cloned()?;
    Some(merged_builtin_def(inst, &variant))
}

/// The pure variant ∪ inline merge behind [`builtin_merged`] (which only adds
/// the library lookup). Pure so the merge rule is unit-tested directly — the
/// regression guard for "inline extensions stored but never rendered", and the
/// parity anchor for export: `flatten_builtin_materials` writes this exact def
/// into each bundle node's `inline`, so editor rendering == player rendering
/// holds iff this merge is a fixed point over its own output (tested).
pub(crate) fn merged_builtin_def(
    inst: &awsm_renderer_editor_protocol::dynamic_material::MaterialInstance,
    variant: &awsm_renderer_editor_protocol::MaterialDef,
) -> awsm_renderer_editor_protocol::MaterialDef {
    use awsm_renderer_editor_protocol::material::{
        MaterialAlphaMode, MaterialShading, PbrExtensions,
    };
    use awsm_renderer_editor_protocol::TextureRef;
    let inline = &inst.inline;

    // ── The override rule: the MATERIAL owns the pipeline, the NODE owns data ─
    // Everything pipeline-shaped comes ONLY from the shared `variant`: shading
    // model, alpha MODE, double-sided cull, vertex-colors, extension ENABLES,
    // and texture-slot CAPABILITIES. The per-mesh `inline` surface is pure
    // data: factors, extension PARAMETERS (where the variant enables the
    // extension), Toon/FlipBook knobs, the Mask cutoff value, and texture
    // IMAGES bound into CAPABLE slots. A capable slot's sampling code is
    // compiled into the shared bucket behind a runtime `exists` flag, so
    // binding/unbinding an image is a data write — never a recompile, and
    // never a per-mesh pipeline.

    // Texture binding is pure DATA — every slot's sampling code is always
    // compiled (unbound slots pack the shared 1×1 neutral), so a per-mesh
    // `inline` image wins, then a custom `texture_overrides` swap, then the
    // variant's default image. Binds never re-key the pipeline.
    let tex = |slot: &str,
               inline_tex: Option<TextureRef>,
               default: Option<TextureRef>|
     -> Option<TextureRef> {
        merge_slot_texture(
            inline_tex,
            inst.texture_overrides.get(slot).cloned(),
            default,
        )
    };

    // Extension ENABLES are variant-only (strict capabilities); an enabled
    // extension's parameters come from `inline` when seeded. The rule lives
    // on `PbrExtensions::merged_over` — shared with the inspector so the UI
    // shows exactly what renders.
    let extensions = PbrExtensions::merged_over(&inline.extensions, &variant.extensions);

    // Alpha MODE is variant-only (pipeline routing). The Mask *cutoff* is a
    // per-mesh uniform compare, so inline's value wins when both layers are
    // Mask.
    let alpha_mode = match (&variant.alpha_mode, &inline.alpha_mode) {
        (MaterialAlphaMode::Mask { .. }, MaterialAlphaMode::Mask { cutoff }) => {
            MaterialAlphaMode::Mask { cutoff: *cutoff }
        }
        _ => variant.alpha_mode.clone(),
    };

    // Shading MODEL is variant (selects the renderer Material flavour); the
    // Toon / FlipBook knobs are uniform (one canonical shader_id each), so
    // carry them from inline.
    let shading = match (variant.shading, inline.shading) {
        (MaterialShading::Toon { .. }, t @ MaterialShading::Toon { .. }) => t,
        (MaterialShading::FlipBook { .. }, f @ MaterialShading::FlipBook { .. }) => f,
        (v, _) => v,
    };

    awsm_renderer_editor_protocol::MaterialDef {
        base_color: inline.base_color,
        metallic: inline.metallic,
        roughness: inline.roughness,
        emissive: inline.emissive,
        normal_scale: inline.normal_scale,
        occlusion_strength: inline.occlusion_strength,
        ssr_mask: inline.ssr_mask,
        base_color_texture: tex(
            "base_color_texture",
            inline.base_color_texture,
            variant.base_color_texture,
        ),
        metallic_roughness_texture: tex(
            "metallic_roughness_texture",
            inline.metallic_roughness_texture,
            variant.metallic_roughness_texture,
        ),
        normal_texture: tex(
            "normal_texture",
            inline.normal_texture,
            variant.normal_texture,
        ),
        occlusion_texture: tex(
            "occlusion_texture",
            inline.occlusion_texture,
            variant.occlusion_texture,
        ),
        emissive_texture: tex(
            "emissive_texture",
            inline.emissive_texture,
            variant.emissive_texture,
        ),
        alpha_mode,
        shading,
        extensions,
        // variant-only: double_sided, vertex_colors_enabled, label.
        ..variant.clone()
    }
}

/// Resolve the renderer material key for a geometry node from its optional
/// library assignment + per-mesh inline uniform store. **The single source of
/// truth** for the material model — shared by primitives, captured meshes, and
/// sweeps so all three render identically:
///   • assigned built-in → merge its shared *variant* with this mesh's per-mesh
///     `inline` uniforms → one `Material::Pbr/Unlit/Toon`;
///   • assigned custom WGSL → its registered bucket;
///   • unassigned (or an assignment that can't resolve yet) → flat **magenta**,
///     the missing-material sentinel.
///
/// The instance's `inline` field is purely the per-mesh *uniform* store for a
/// built-in assignment (base colour / metallic / … — see the material model note
/// in `inspector.rs::material_editor`); it never stands in as a material on its
/// own.
///
/// `vertex_color_set` is the geometry COLOR set the mesh carries (`Some(0)` for
/// painted `COLOR_0`), or `None` when it has none. Vertex-colour *usage* is
/// geometry-derived, so — exactly as the skinned path does — we flip
/// `vertex_colors_enabled` on the merged built-in def + bind the set, so painted
/// colours actually multiply the base colour instead of being uploaded and
/// silently ignored.
fn resolve_assigned_material(
    r: &mut awsm_renderer::AwsmRenderer,
    material: Option<&awsm_renderer_editor_protocol::dynamic_material::MaterialInstance>,
    vertex_color_set: Option<u32>,
) -> awsm_renderer::materials::MaterialKey {
    match material {
        Some(inst) => {
            if let Some(mut merged) = builtin_merged(inst) {
                merged.vertex_colors_enabled = vertex_color_set.is_some();
                material::insert_material_vc(r, &merged, vertex_color_set)
            } else if let Some(k) = super::dynamic::insert_custom(r, inst) {
                k
            } else {
                material::insert_magenta(r)
            }
        }
        None => material::insert_magenta(r),
    }
}

/// How [`upload_simple_mesh`] resolves its material.
enum MeshMaterial {
    /// A user-assignable geometry node (captured mesh, sweep): resolve the
    /// optional assignment via [`resolve_assigned_material`] — magenta when
    /// unassigned, exactly like a primitive.
    Assigned(Option<awsm_renderer_editor_protocol::dynamic_material::MaterialInstance>),
    /// Render this material def directly — no assignment concept. Used by
    /// instanced geometry, whose appearance is the flat default + per-instance
    /// colours, not a per-node material assignment.
    Flat(awsm_renderer_editor_protocol::MaterialDef),
}

/// The geometry COLOR set index a renderer mesh carries (glTF `COLOR_n`), or
/// `None` if it has no vertex-colour attribute. Vertex-colour *usage* is
/// geometry-derived, so the bridge sets `vertex_colors_enabled` + the set index
/// from this (mirroring how `populate_gltf` decides it per primitive).
fn mesh_vertex_color_set(
    r: &awsm_renderer::AwsmRenderer,
    mk: awsm_renderer::meshes::MeshKey,
) -> Option<u32> {
    use awsm_renderer::meshes::buffer_info::{
        MeshBufferCustomVertexAttributeInfo, MeshBufferVertexAttributeInfo,
    };
    r.meshes.buffer_info(mk).ok().and_then(|info| {
        info.triangles.vertex_attributes.iter().find_map(|attr| {
            if let MeshBufferVertexAttributeInfo::Custom(
                MeshBufferCustomVertexAttributeInfo::Colors { index, .. },
            ) = attr
            {
                Some(*index)
            } else {
                None
            }
        })
    })
}

/// Schema → runtime per-mesh shadow cast/receive flags.
fn mesh_shadow_flags_from_config(
    cfg: &awsm_renderer_editor_protocol::MeshShadowConfig,
) -> awsm_renderer::shadows::MeshShadowFlags {
    awsm_renderer::shadows::MeshShadowFlags {
        cast: cfg.cast,
        receive: cfg.receive,
    }
}

/// Build a node-owned skinned [`RawMeshData`] for a `SkinnedMesh` node by decoding
/// the clean rig glb (our-format) at the node's `rig_node_index` — the MATERIALISER's
/// geometry+skin read. The per-vertex `JOINTS_0` index into the skin's joint list;
/// each joint's rig-glb node-index is mapped → its editor bone `TransformKey` via the
/// `SkinnedMeshRef::joints` table (bone `NodeId` ↔ rig-glb flat index) + the bridge,
/// so the skin deforms DIRECTLY from the animated editor bones (no `skin_bridge` hop).
/// IBMs come from the same rig-glb decode (bit-identical to the original — proven by
/// the glb-export round-trip proptest), so the bind pose is exact.
///
/// Returns `None` when no rig glb is cached for the source (e.g. a legacy project's
/// skinned node, or a morph-only node with no skin), the node carries no skin, or a
/// joint can't be resolved to its bone yet — callers then fall back to the
/// template-reuse path.
fn raw_mesh_from_rig(skin: &awsm_renderer_editor_protocol::SkinnedMeshRef) -> Option<RawMeshData> {
    let Some(decode) = super::skinned_bake_cache::get_rig_node_decode(
        skin.source,
        skin.rig_node_index,
        skin.primitive_index,
    ) else {
        tracing::debug!(
            "raw_mesh_from_rig: no rig decode for {:?} rig_node_index={}",
            skin.source,
            skin.rig_node_index
        );
        return None;
    };
    // SKIN (optional): build it when the decode carries one, mapping each rig-glb
    // joint node-index → its editor bone `TransformKey`. A bone not yet in the bridge
    // means the transforms-first pass hasn't landed it → return `None` so the caller
    // retries (same ordering guard as before). Morph-only nodes have no skin → `None`.
    let raw_skin = match decode.skin.as_ref() {
        Some(ext_skin) => {
            let joints = resolve_skin_joint_transforms(skin, ext_skin)?;
            let inverse_bind_matrices: Vec<Mat4> = ext_skin
                .inverse_bind_matrices
                .iter()
                .map(Mat4::from_cols_array)
                .collect();
            Some(RawSkin {
                joints,
                inverse_bind_matrices,
                set_count: 1,
                index_weights: ext_skin.packed_index_weights(),
            })
        }
        None => None,
    };

    // MORPH (optional): pack the decoded morph targets into the renderer's
    // geometry-morph layout — `add_raw_mesh` inserts it + the relower auto-rebinds
    // the morph-weight channel (node → mesh → geometry_morph_key).
    let vertex_count = decode.mesh.positions.len();
    let raw_morph = decode.morph.as_ref().map(|m| {
        let values = m.packed_values(vertex_count);
        RawMorph {
            info: MeshBufferGeometryMorphInfo {
                targets_len: m.targets_len(),
                vertex_stride_size: m.vertex_stride_size(),
                values_size: values.len(),
            },
            weights: m.weights_bytes(),
            values,
        }
    });

    // A node-owned drawable needs at least one of skin/morph (else it's a degenerate
    // SkinnedMesh — fall back so behaviour is unchanged for that case).
    if raw_skin.is_none() && raw_morph.is_none() {
        tracing::debug!("raw_mesh_from_rig: rig decode has neither skin nor morph — falling back");
        return None;
    }

    let tangents = decode.tangents;
    let m = decode.mesh;
    Some(RawMeshData {
        positions: m.positions,
        normals: m.normals,
        // All UV sets ride `mesh.uvs` now (incl. TEXCOORD_1).
        uv_sets: m.uvs,
        colors: m.colors,
        indices: m.indices,
        // Authored tangents from the rig glb decode → used verbatim (else regenerated).
        tangents,
        skin: raw_skin,
        morph: raw_morph,
    })
}

/// Resolve a `SkinnedMesh` node's skin joints to their editor-bone renderer
/// `TransformKey`s, in the RIG-GLB joint order (`ext_skin.joint_node_indices`)
/// — the order every renderer skin built from this source uses, so the result
/// pairs 1:1 with that skin's inverse-bind matrices. `None` when a joint isn't
/// in the node's `skin.joints` table or its bone node hasn't materialized in
/// the bridge yet (callers fall back / retry — the same ordering guard the
/// raw-mesh path always had).
fn resolve_skin_joint_transforms(
    skin: &awsm_renderer_editor_protocol::SkinnedMeshRef,
    ext_skin: &awsm_renderer_glb_export::ExtractedSkin,
) -> Option<Vec<TransformKey>> {
    let b = bridge();
    let nodes = b.nodes.lock().unwrap();
    let mut joints = Vec::with_capacity(ext_skin.joint_node_indices.len());
    for rig_idx in &ext_skin.joint_node_indices {
        let bone_node = skin
            .joints
            .iter()
            .find(|sj| sj.index == *rig_idx as u32)
            .map(|sj| sj.node)?;
        match nodes.get(&bone_node).map(|e| e.transform_key) {
            Some(tk) => joints.push(tk),
            None => {
                tracing::warn!(
                    "skinned materialize: bone node {:?} (rig joint idx {}) not yet in \
                     bridge — falling back",
                    bone_node,
                    rig_idx
                );
                return None;
            }
        }
    }
    Some(joints)
}

/// A live, already-materialized skinned drawable of the SAME rig geometry
/// (same source / rig node index / primitive) on ANOTHER scene node — the
/// geometry donor a duplicated `SkinnedMesh` shares GPU buffers with instead
/// of re-uploading (axis 4: clone must never clone data). Returns the donor's
/// `MeshKey`; `None` when this node is the first of its geometry (the normal
/// first-materialize) or no candidate is live/skinned right now.
/// Find a live node whose materialized drawable can serve as the geometry
/// donor for a `NodeKind::Mesh` referencing the same mesh ASSET — the static
/// counterpart of [`find_skinned_geometry_donor`]. Only node-owned drawables
/// (`model_meshes`) qualify; liveness is validated against the renderer, and
/// instanced meshes are skipped (an instancer's drawable carries per-instance
/// state a plain duplicate must not inherit).
fn find_static_geometry_donor(
    r: &awsm_renderer::AwsmRenderer,
    node_id: NodeId,
    mesh_asset: crate::engine::scene::AssetId,
) -> Option<awsm_renderer::meshes::MeshKey> {
    let b = bridge();
    let nodes = b.nodes.lock().unwrap();
    for entry in nodes.values() {
        if entry.node_id == node_id {
            continue;
        }
        let NodeKind::Mesh { mesh: other, .. } = entry.node.kind.get_cloned() else {
            continue;
        };
        if other.0 != mesh_asset {
            continue;
        }
        for mk in entry.model_meshes.lock().unwrap().iter() {
            if let Ok(m) = r.meshes.get(*mk) {
                if !m.instanced {
                    return Some(*mk);
                }
            }
        }
    }
    None
}

/// Materialize a `NodeKind::Mesh` by SHARING an existing node's geometry:
/// duplicate the donor's mesh (refcounted resource — no re-upload) with this
/// node's own material/shadow/visibility. Returns `false` (having created
/// nothing) when there is no donor or the resolved material needs a pass
/// representation the donor's geometry doesn't carry — the caller then takes
/// the fresh `upload_simple_mesh` path.
async fn materialize_static_duplicate(
    entry: &Arc<RendererNode>,
    mesh_asset: crate::engine::scene::AssetId,
    material: Option<&awsm_renderer_editor_protocol::dynamic_material::MaterialInstance>,
    declare_only: bool,
) -> bool {
    let visible = effective_visible(entry.node_id);
    let shadow_cfg = entry
        .node
        .kind
        .get_cloned()
        .mesh_shadow()
        .copied()
        .unwrap_or_default();
    let shadow_flags = mesh_shadow_flags_from_config(&shadow_cfg);

    let handle = renderer_handle();
    let mut r = handle.lock().await;
    let Some(donor_mk) = find_static_geometry_donor(&r, entry.node_id, mesh_asset) else {
        return false;
    };
    // Same geometry ⇒ same vertex-colour classification as the donor.
    let vertex_color_set = mesh_vertex_color_set(&r, donor_mk);
    let mat_key = resolve_assigned_material(&mut r, material, vertex_color_set);
    // The duplicate reuses the donor's uploaded representations — a material
    // routed to a pass the donor's geometry has no representation for can't
    // share; fall back to the fresh-upload path, which packs the right kind.
    let donor_has_rep = if r.materials.is_transparency_pass(mat_key) {
        r.meshes
            .transparency_geometry_data_buffer_offset(donor_mk)
            .is_ok()
    } else {
        r.meshes
            .visibility_geometry_data_buffer_offset(donor_mk)
            .is_ok()
    };
    if !donor_has_rep {
        r.remove_material(mat_key);
        return false;
    }
    let sub_tk = r
        .transforms
        .insert(Transform::IDENTITY, Some(entry.transform_key));
    match r.duplicate_mesh_with_transform(donor_mk, sub_tk) {
        Ok(mk) => {
            let _ = r.set_mesh_material(mk, mat_key);
            let _ = r.set_mesh_hidden(mk, !visible);
            let _ = r.set_mesh_shadow_flags(mk, shadow_flags);
            if !declare_only {
                if let Err(e) = r
                    .commit_load(crate::engine::activity::commit_phase_handler())
                    .await
                {
                    tracing::warn!("commit_load (static duplicate): {e}");
                }
            }
            drop(r);
            entry.model_meshes.lock().unwrap().push(mk);
            entry.model_transforms.lock().unwrap().push(sub_tk);
            entry.material_keys.lock().unwrap().push(mat_key);
            bridge().register_mesh(mk, entry.node_id);
            true
        }
        Err(e) => {
            r.transforms.remove(sub_tk);
            r.remove_material(mat_key);
            tracing::warn!("static duplicate: duplicate_mesh_with_transform failed: {e}");
            false
        }
    }
}

fn find_skinned_geometry_donor(
    r: &awsm_renderer::AwsmRenderer,
    node_id: NodeId,
    skin: &awsm_renderer_editor_protocol::SkinnedMeshRef,
) -> Option<awsm_renderer::meshes::MeshKey> {
    let b = bridge();
    let nodes = b.nodes.lock().unwrap();
    for entry in nodes.values() {
        if entry.node_id == node_id {
            continue;
        }
        let NodeKind::SkinnedMesh { skin: other, .. } = entry.node.kind.get_cloned() else {
            continue;
        };
        if other.source != skin.source
            || other.rig_node_index != skin.rig_node_index
            || other.primitive_index != skin.primitive_index
        {
            continue;
        }
        // Node-owned drawables only (`model_meshes`) — the legacy
        // template-reuse path's meshes are template-owned and never land here.
        // Validate liveness against the renderer (the entry could be
        // mid-rematerialize). No skin requirement: a MORPH-ONLY source's
        // drawable carries no skin, and same source/rig-node/primitive ⇒ same
        // skinned-ness — the caller branches on the rig decode.
        for mk in entry.model_meshes.lock().unwrap().iter() {
            if r.meshes.get(*mk).is_ok() {
                return Some(*mk);
            }
        }
    }
    None
}

/// Materialize a DUPLICATED `SkinnedMesh` node by SHARING an existing node's
/// geometry: duplicate the donor's mesh (refcounted resource — no geometry
/// re-upload) onto a fresh per-instance skin cloned over THIS node's bones.
/// The per-instance GPU data is the skin's joint-matrix palette (+ a morph
/// WEIGHTS slot when the rig is morphed) — exactly the prefab-instantiate
/// model the scene-loader uses, so editor duplicates and player prefab
/// instances agree on one sharing model. Returns `false` (having created
/// nothing) when there is no donor / no rig decode / a bone isn't bridged yet
/// / the resolved material needs a pass representation the donor's geometry
/// doesn't carry — the caller then takes the normal `add_raw_mesh` path.
async fn materialize_skinned_duplicate(
    entry: &Arc<RendererNode>,
    skin: &awsm_renderer_editor_protocol::SkinnedMeshRef,
    material: Option<&awsm_renderer_editor_protocol::dynamic_material::MaterialInstance>,
    declare_only: bool,
) -> bool {
    let visible = effective_visible(entry.node_id);
    let shadow_cfg = entry
        .node
        .kind
        .get_cloned()
        .mesh_shadow()
        .copied()
        .unwrap_or_default();
    let shadow_flags = mesh_shadow_flags_from_config(&shadow_cfg);

    let handle = renderer_handle();
    let mut r = handle.lock().await;
    // Donor first — it's the cheap check, and on the COMMON first-materialize
    // (no donor) it bails before paying for a rig decode clone.
    let Some(donor_mk) = find_skinned_geometry_donor(&r, entry.node_id, skin) else {
        return false;
    };
    // Joint order comes from the rig decode (the order the donor's skin — and
    // every renderer skin of this source — was inserted in); an uncached rig
    // can't take this path. A MORPH-ONLY node (rig decode has no skin) shares
    // geometry too — it skips the skin clone and mints only a fresh
    // morph-weights instance (`duplicate_mesh_with_transform`).
    let Some(decode) = super::skinned_bake_cache::get_rig_node_decode(
        skin.source,
        skin.rig_node_index,
        skin.primitive_index,
    ) else {
        return false;
    };
    // Skinned: resolve the donor skin + this node's cloned joints up front
    // (before any allocation) so every bail below stays create-nothing.
    let skin_parts = match decode.skin.as_ref() {
        Some(ext_skin) => {
            let Some(donor_skin) = r.meshes.mesh_skin_key(donor_mk).flatten() else {
                return false;
            };
            let Some(instance_joints) = resolve_skin_joint_transforms(skin, ext_skin) else {
                return false;
            };
            Some((donor_skin, instance_joints))
        }
        None => None,
    };

    // Same geometry ⇒ same vertex-colour classification as the donor.
    let vertex_color_set = mesh_vertex_color_set(&r, donor_mk);
    let mat_key = resolve_assigned_material(&mut r, material, vertex_color_set);
    // The duplicate reuses the donor's uploaded representations — a material
    // routed to a pass the donor's geometry has no representation for (an
    // opaque↔blend flip relative to how the donor committed) can't share;
    // fall back to the fresh-upload path, which packs the right kind.
    let donor_has_rep = if r.materials.is_transparency_pass(mat_key) {
        r.meshes
            .transparency_geometry_data_buffer_offset(donor_mk)
            .is_ok()
    } else {
        r.meshes
            .visibility_geometry_data_buffer_offset(donor_mk)
            .is_ok()
    };
    if !donor_has_rep {
        r.remove_material(mat_key);
        return false;
    }

    // Same placement rule as the fresh path: skinned drawables ride an
    // IDENTITY transform under the renderer root (the skin deforms via the
    // joints; the glTF rule ignores a skinned mesh node's own transform).
    let root = r.transforms.root_node;
    let sub_tk = r.transforms.insert(Transform::IDENTITY, Some(root));
    let dup_result = match skin_parts {
        Some((donor_skin, instance_joints)) => {
            let new_skin = match r.clone_skin_for_joints(donor_skin, instance_joints) {
                Ok(k) => k,
                Err(e) => {
                    r.transforms.remove(sub_tk);
                    r.remove_material(mat_key);
                    tracing::warn!("skinned duplicate: clone_skin_for_joints failed: {e}");
                    return false;
                }
            };
            r.duplicate_skinned_mesh_with_skin(donor_mk, sub_tk, new_skin)
                .inspect_err(|_| {
                    r.meshes.skins.remove(new_skin, None);
                })
        }
        // Morph-only: shared geometry, fresh per-instance morph weights.
        None => r.duplicate_mesh_with_transform(donor_mk, sub_tk),
    };
    match dup_result {
        Ok(mk) => {
            let _ = r.set_mesh_material(mk, mat_key);
            let _ = r.set_mesh_hidden(mk, !visible);
            let _ = r.set_mesh_shadow_flags(mk, shadow_flags);
            if !declare_only {
                if let Err(e) = r
                    .commit_load(crate::engine::activity::commit_phase_handler())
                    .await
                {
                    tracing::warn!("commit_load (skinned duplicate): {e}");
                }
            }
            drop(r);
            entry.model_meshes.lock().unwrap().push(mk);
            entry.model_transforms.lock().unwrap().push(sub_tk);
            entry.material_keys.lock().unwrap().push(mat_key);
            bridge().register_mesh(mk, entry.node_id);
            true
        }
        Err(e) => {
            // A failed skinned duplicate already freed its cloned skin (see
            // the map_err above); only the shared allocations remain.
            r.transforms.remove(sub_tk);
            r.remove_material(mat_key);
            tracing::warn!("skinned duplicate: duplicate failed: {e}");
            false
        }
    }
}

/// Materialize (or re-materialize) a `SkinnedMesh` node. **The unified path** decodes
/// the clean rig glb (our-format) into a NODE-OWNED skinned drawable via
/// [`raw_mesh_from_rig`] + `add_raw_mesh` with the CURRENT material, so an
/// opaque↔blend material flip (or any edit) rebuilds through the SAME
/// teardown+`apply_kind` path as static geometry — no more `set_mesh_material` on a
/// shared populate template (which couldn't rebuild a never-built kind → the vanish
/// bug). The drawable is pushed to `model_meshes`, so `teardown` frees it.
///
/// Falls back to the legacy template-reuse path ([`materialize_skinned_from_template`])
/// only when `raw_mesh_from_rig` returns `None` — i.e. no rig decode is cached (a
/// legacy project, or a source whose rig glb wasn't persisted). Both skinned AND
/// morph-only nodes otherwise go node-owned through `raw_mesh_from_rig` (skin + morph
/// are each optional there).
async fn materialize_skinned_mesh(
    entry: Arc<RendererNode>,
    skin: awsm_renderer_editor_protocol::SkinnedMeshRef,
    material: Option<awsm_renderer_editor_protocol::dynamic_material::MaterialInstance>,
    declare_only: bool,
) {
    // GEOMETRY-SHARING duplicate path (axis 4): when another node already
    // materialized this exact rig geometry (a duplicated character), don't
    // re-upload it — duplicate that mesh over the shared resource with a
    // fresh per-instance skin bound to THIS node's (cloned) bones.
    if materialize_skinned_duplicate(&entry, &skin, material.as_ref(), declare_only).await {
        return;
    }

    let Some(raw) = raw_mesh_from_rig(&skin) else {
        // No rig decode cached → the legacy template-reuse fallback (a SAFETY NET for
        // legacy projects / sources whose rig glb wasn't persisted). Kept on purpose:
        // its edge cases (uncached rig, plus the rare transient below) can't all be
        // retired with confidence, and node-owned materialise needs the rig decode.
        //
        // ⭐ TRANSACTION PRINCIPLE (this module's docs): loading is ONE
        // `begin_load → declare ops (transforms BEFORE the geometry that references
        // them) → commit_load` transaction. `raw_mesh_from_rig` can ALSO return
        // `None` transiently when a joint's bone scene-node isn't in `bridge.nodes`
        // yet — an ORDERING issue (skinned geometry declared before its bone
        // transforms). That ordering is now handled by the transforms-first bulk
        // load (the join-barrier `establish_forest_transforms`), so this is rare; the
        // fix is the ordering, NOT a post-hoc re-materialise. Do NOT "fix" reload by
        // re-running materialise.
        materialize_skinned_from_template(entry, skin, material, declare_only).await;
        return;
    };

    let visible = effective_visible(entry.node_id);
    let shadow_cfg = entry
        .node
        .kind
        .get_cloned()
        .mesh_shadow()
        .copied()
        .unwrap_or_default();
    let shadow_flags = mesh_shadow_flags_from_config(&shadow_cfg);
    // Per the glTF rule "the transform of a skinned mesh node is ignored" — the skin
    // deforms via the joints — the drawable rides an IDENTITY transform under the
    // renderer root (independent of this editor node's transform, matching the prior
    // populate behaviour). `teardown` frees it via `model_transforms`.
    let handle = renderer_handle();
    let mut r = handle.lock().await;
    let root = r.transforms.root_node;
    let vertex_color_set = raw.colors.as_ref().filter(|c| !c.is_empty()).map(|_| 0u32);
    let mat_key = resolve_assigned_material(&mut r, material.as_ref(), vertex_color_set);
    let sub_tk = r.transforms.insert(Transform::IDENTITY, Some(root));
    match r.add_raw_mesh(raw, sub_tk, mat_key) {
        Ok(mk) => {
            let _ = r.set_mesh_hidden(mk, !visible);
            let _ = r.set_mesh_shadow_flags(mk, shadow_flags);
            if !declare_only {
                if let Err(e) = r
                    .commit_load(crate::engine::activity::commit_phase_handler())
                    .await
                {
                    tracing::warn!("commit_load (skinned mesh): {e}");
                }
            }
            drop(r);
            entry.model_meshes.lock().unwrap().push(mk);
            entry.model_transforms.lock().unwrap().push(sub_tk);
            entry.material_keys.lock().unwrap().push(mat_key);
            bridge().register_mesh(mk, entry.node_id);
        }
        Err(e) => {
            r.transforms.remove(sub_tk);
            r.remove_material(mat_key);
            tracing::error!("materialize skinned mesh (rig) failed: {e}");
        }
    }
}

/// Materialize a view-only [`NodeKind::ClusterMesh`] through the renderer's cluster
/// pipeline — the SAME `scene-loader::materialize_cluster_mesh` the player uses, so a
/// huge mesh renders as cluster (bounded draw + VRAM) with no in-editor re-bake and
/// no dense visibility-geometry explode. The cluster DAG comes from the import-time
/// [`super::cluster_cache`]; the render mesh rides a child of the NODE's transform so
/// moving/scaling the node moves it. Tracked in `model_*` for teardown like any node.
async fn materialize_cluster_mesh_node(
    entry: Arc<RendererNode>,
    cluster: awsm_renderer_editor_protocol::ClusterMeshRef,
    material: Option<awsm_renderer_editor_protocol::dynamic_material::MaterialInstance>,
    declare_only: bool,
) {
    let Some(cm) = super::cluster_cache::get(cluster.source) else {
        tracing::warn!(
            "ClusterMesh source {:?}: not in the cluster cache (re-import the cluster asset) — renders empty",
            cluster.source
        );
        return;
    };
    let visible = effective_visible(entry.node_id);
    let shadow_cfg = entry
        .node
        .kind
        .get_cloned()
        .mesh_shadow()
        .copied()
        .unwrap_or_default();
    let shadow_flags = mesh_shadow_flags_from_config(&shadow_cfg);
    let parent_tk = entry.transform_key;
    let handle = renderer_handle();
    let mut r = handle.lock().await;
    let mat_key = resolve_assigned_material(&mut r, material.as_ref(), None);
    let sub_tk = r.transforms.insert(Transform::IDENTITY, Some(parent_tk));
    let label = cluster.source.0.to_string();
    match awsm_renderer_scene_loader::materialize_cluster_mesh(&mut r, &cm, &label, sub_tk, mat_key)
        .await
    {
        Ok(Some(mk)) => {
            let _ = r.set_mesh_hidden(mk, !visible);
            let _ = r.set_mesh_shadow_flags(mk, shadow_flags);
            if !declare_only {
                if let Err(e) = r
                    .commit_load(crate::engine::activity::commit_phase_handler())
                    .await
                {
                    tracing::warn!("commit_load (cluster mesh): {e}");
                }
            }
            drop(r);
            entry.model_meshes.lock().unwrap().push(mk);
            entry.model_transforms.lock().unwrap().push(sub_tk);
            entry.material_keys.lock().unwrap().push(mat_key);
            bridge().register_mesh(mk, entry.node_id);
        }
        Ok(None) => {
            r.transforms.remove(sub_tk);
            r.remove_material(mat_key);
            tracing::warn!(
                "ClusterMesh {label}: materialize returned None (virtual_geometry off / empty mesh)"
            );
        }
        Err(e) => {
            r.transforms.remove(sub_tk);
            r.remove_material(mat_key);
            tracing::error!("materialize cluster mesh failed: {e}");
        }
    }
}

/// Legacy skinned materialise: reuse the populate-built skinned mesh from the import
/// template and (re)assign this node's material + shadow flags via `set_mesh_material`.
/// SAFETY NET, retained on purpose for the nodes [`raw_mesh_from_rig`] can't serve —
/// a legacy project / source whose rig glb isn't cached (no rig decode). The populate
/// mesh keys are template-owned (they survive teardown), so they are NOT pushed to
/// `model_meshes`.
async fn materialize_skinned_from_template(
    entry: Arc<RendererNode>,
    skin: awsm_renderer_editor_protocol::SkinnedMeshRef,
    material: Option<awsm_renderer_editor_protocol::dynamic_material::MaterialInstance>,
    declare_only: bool,
) {
    // The populate skinned mesh is hidden by `hide_template_meshes` (now hides all
    // template meshes); un-hide it here since this path renders that copy directly.
    let Some(template) = bridge().get_template(skin.source) else {
        tracing::warn!(
            "SkinnedMesh {:?}: no import template cached (session-local — survives \
             only within the import session); renders empty",
            skin.source
        );
        return;
    };
    let Some(tnode) = template.find_by_node_index(skin.node_index) else {
        tracing::warn!(
            "SkinnedMesh node_index {} not in template; renders empty",
            skin.node_index
        );
        return;
    };
    // The skinned renderer mesh key(s) for this node. `primitive_index = Some(i)`
    // peels one primitive (a destructured multi-material node); `None` = all.
    let mesh_keys: Vec<awsm_renderer::meshes::MeshKey> = match skin.primitive_index {
        None => tnode.mesh_keys.clone(),
        Some(i) => tnode
            .mesh_keys
            .get(i as usize)
            .copied()
            .into_iter()
            .collect(),
    };
    if mesh_keys.is_empty() {
        return;
    }

    let visible = effective_visible(entry.node_id);
    let shadow_cfg = entry
        .node
        .kind
        .get_cloned()
        .mesh_shadow()
        .copied()
        .unwrap_or_default();
    let shadow_flags = mesh_shadow_flags_from_config(&shadow_cfg);

    let mut material_keys = Vec::new();
    let mut to_register = Vec::new();
    {
        let handle = renderer_handle();
        let mut r = handle.lock().await;
        for mk in &mesh_keys {
            let _ = r.set_mesh_hidden(*mk, !visible);
            let _ = r.set_mesh_shadow_flags(*mk, shadow_flags);
            // Resolve PER PRIMITIVE because vertex-colour usage is geometry-derived
            // (matches the renderer's native per-primitive behaviour).
            let vertex_color_set = mesh_vertex_color_set(&r, *mk);
            let mat_key = match material.as_ref() {
                Some(inst) => {
                    if let Some(mut merged) = builtin_merged(inst) {
                        merged.vertex_colors_enabled = vertex_color_set.is_some();
                        material::insert_material_vc(&mut r, &merged, vertex_color_set)
                    } else if let Some(k) = super::dynamic::insert_custom(&mut r, inst) {
                        k
                    } else {
                        material::insert_magenta(&mut r)
                    }
                }
                None => material::insert_magenta(&mut r),
            };
            let _ = r.set_mesh_material(*mk, mat_key);
            material_keys.push(mat_key);
            to_register.push(*mk);
        }
        // Commit the staged content through the one compile path (finalize textures
        // + compile new pipelines) — UNLESS the bulk-load join commits once at the end.
        if !declare_only {
            if let Err(e) = r
                .commit_load(crate::engine::activity::commit_phase_handler())
                .await
            {
                tracing::warn!("commit_load (skinned mesh): {e}");
            }
        }
    }

    for mk in &to_register {
        bridge().register_mesh(*mk, entry.node_id);
    }
    // Only the inserted materials are owned by this node; the skinned mesh keys
    // belong to the populate pass and must survive teardown (so they keep
    // deforming) — do NOT add them to `model_meshes`.
    entry.material_keys.lock().unwrap().extend(material_keys);
}

/// Authored polyline (`NodeKind::Line`) → fat-line strip. The fat-line pipeline
/// reads world-space positions, so the node transform is baked in CPU-side.
async fn materialize_line(entry: Arc<RendererNode>, def: awsm_renderer_editor_protocol::LineDef) {
    if def.points.len() < 2 {
        return;
    }
    let parent_tk = entry.transform_key;
    let positions: Vec<Vec3> = def.points.iter().map(|p| Vec3::from_array(p.pos)).collect();
    let colors: Vec<Vec4> = def
        .points
        .iter()
        .map(|p| Vec4::from_array(p.color))
        .collect();
    let entry2 = entry.clone();
    let line_key = with_renderer_mut(move |r| {
        let world = r
            .transforms
            .get_world(parent_tk)
            .copied()
            .unwrap_or(glam::Mat4::IDENTITY);
        let positions_world: Vec<Vec3> = positions
            .iter()
            .map(|p| world.transform_point3(*p))
            .collect();
        match r.add_line_strip(
            &positions_world,
            &colors,
            def.width_px,
            def.depth_test_always,
        ) {
            Ok(key) => key,
            Err(err) => {
                tracing::warn!("materialize_line: add_line_strip failed: {err}");
                None
            }
        }
    })
    .await;
    if let Some(key) = line_key {
        entry2.line_keys.lock().unwrap().push(key);
    }
}

/// Curve viz (`NodeKind::Curve`) → a sampled Catmull-Rom polyline drawn as a
/// magenta fat-line (the curve itself emits no game geometry; sweeps/instances
/// consume it). World-space, parent transform baked in.
async fn materialize_curve_viz(
    entry: Arc<RendererNode>,
    def: awsm_renderer_editor_protocol::CurveDef,
) {
    if def.control_points.len() < 2 {
        return;
    }
    let parent_tk = entry.transform_key;
    let entry2 = entry.clone();
    let line_key = with_renderer_mut(move |r| {
        use awsm_renderer_curves::{CatmullRomCurve, Curve3};
        let curve = CatmullRomCurve::new(
            def.control_points
                .iter()
                .map(|p| Vec3::from_array(*p))
                .collect(),
            def.closed,
        );
        let samples = def.sample_count.max(2) as usize;
        let mut positions = curve.get_spaced_points(samples);
        if positions.is_empty() {
            return None;
        }
        let world = r
            .transforms
            .get_world(parent_tk)
            .copied()
            .unwrap_or(glam::Mat4::IDENTITY);
        for p in positions.iter_mut() {
            *p = world.transform_point3(*p);
        }
        if def.closed {
            if let Some(first) = positions.first().copied() {
                positions.push(first);
            }
        }
        let colors: Vec<Vec4> = vec![Vec4::new(1.0, 0.45, 0.85, 0.95); positions.len()];
        // Wider than a hairline so the curve reads clearly in the viewport —
        // a thin line is nearly invisible against the ground grid, especially
        // for flat (default) curves.
        match r.add_line_strip(&positions, &colors, 3.0, false) {
            Ok(key) => key,
            Err(err) => {
                tracing::warn!("materialize_curve_viz: add_line_strip failed: {err}");
                None
            }
        }
    })
    .await;
    if let Some(key) = line_key {
        entry2.line_keys.lock().unwrap().push(key);
    }
}

/// Textured/tinted quad (`NodeKind::Sprite`) → a `sprite_quad` mesh with the
/// renderer's billboard mode. Single-cell unlit-ish quad (the flipbook-animated
/// variant is the follow-on); sprites don't cast/receive shadows.
async fn materialize_sprite(
    entry: Arc<RendererNode>,
    def: awsm_renderer_editor_protocol::SpriteDef,
    declare_only: bool,
) {
    use awsm_renderer::meshes::mesh::BillboardMode;
    use awsm_renderer_meshgen::sprite_quad;

    let mesh = sprite_quad(def.size[0], def.size[1]);
    let raw = RawMeshData {
        positions: mesh.positions,
        normals: mesh.normals,
        uv_sets: mesh.uvs,
        colors: mesh.colors,
        indices: mesh.indices,
        ..Default::default()
    };
    let sprite_mat = awsm_renderer_editor_protocol::MaterialDef {
        base_color: def.tint,
        metallic: 0.0,
        roughness: 1.0,
        emissive: [def.tint[0] * 1.8, def.tint[1] * 1.8, def.tint[2] * 1.8],
        double_sided: true,
        ..awsm_renderer_editor_protocol::MaterialDef::default()
    };
    let mode = match def.billboard {
        awsm_renderer_editor_protocol::BillboardMode::None => BillboardMode::None,
        awsm_renderer_editor_protocol::BillboardMode::YAxis => BillboardMode::YAxis,
        awsm_renderer_editor_protocol::BillboardMode::Full => BillboardMode::Full,
    };
    let parent_tk = entry.transform_key;

    let handle = renderer_handle();
    let mut r = handle.lock().await;
    let mat_key = material::insert_material(&mut r, &sprite_mat);
    let sub_tk = r.transforms.insert(Transform::IDENTITY, Some(parent_tk));
    // The mesh's pass (visibility/transparency) is resolved at commit from the
    // material — `add_raw_mesh` handles opaque and transparent uniformly now.
    match r.add_raw_mesh(raw, sub_tk, mat_key) {
        Ok(mk) => {
            if !declare_only {
                if let Err(e) = r
                    .commit_load(crate::engine::activity::commit_phase_handler())
                    .await
                {
                    tracing::warn!("sprite commit_load: {e}");
                }
            }
            let _ = r.set_mesh_billboard_mode(mk, mode);
            drop(r);
            entry.model_meshes.lock().unwrap().push(mk);
            entry.model_transforms.lock().unwrap().push(sub_tk);
            entry.material_keys.lock().unwrap().push(mat_key);
            bridge().register_mesh(mk, entry.node_id);
        }
        Err(e) => {
            r.transforms.remove(sub_tk);
            r.remove_material(mat_key);
            tracing::error!("materialize sprite failed: {e}");
        }
    }
}

/// Collider (`NodeKind::Collider`) → an editor-overlay wireframe of the shape,
/// drawn as a world-baked fat-line segment list. Idempotent: it drains any
/// existing wireframe first, so it doubles as the re-bake called by both the kind
/// observer (shape change) and the transform observer (move/rotate). The geometry
/// is world-baked (not parented to the node transform), so it must be rebuilt
/// whenever the node's world changes or the wireframe would lie about where the
/// collider is.
async fn materialize_collider(
    entry: Arc<RendererNode>,
    shape: awsm_renderer_editor_protocol::ColliderShape,
) {
    let key = entry.transform_key;
    let node_id = entry.node_id;
    let entry2 = entry.clone();
    // Drain existing lines so this re-bakes cleanly (the transform observer calls
    // it on every move; the kind observer's teardown already drained, so this is a
    // harmless no-op on the initial materialize).
    let old_lines: Vec<_> = entry.line_keys.lock().unwrap().drain(..).collect();
    let line_key = with_renderer_mut(move |r| {
        for lk in old_lines {
            r.remove_line(lk);
        }
        // Fresh world = parent's CURRENT world × this node's local. `get_world`
        // returns a cached matrix only refreshed by the render loop's
        // update_transforms, so reading this node's own cached world right after a
        // `set_local` would be stale; recompute from the parent (unchanged when the
        // collider itself moves) instead.
        let local = r
            .transforms
            .get_local(key)
            .map(|t| t.to_matrix())
            .unwrap_or(glam::Mat4::IDENTITY);
        let parent_world = r
            .transforms
            .get_parent(key)
            .ok()
            .and_then(|p| r.transforms.get_world(p).ok().copied())
            .unwrap_or(glam::Mat4::IDENTITY);
        let world = parent_world * local;
        // A Rapier collider has no scale — its size comes entirely from the
        // ColliderShape extents and its placement is an isometry (translation +
        // rotation). Strip scale from the world matrix so the wireframe shows the
        // exact dimensions/placement physics sees, instead of folding node (or
        // ancestor) scale on top of the shape extents. (FIXES.md #1.)
        let (scale, rot, trans) = world.to_scale_rotation_translation();
        // The collider's own scale is locked to unit (see SetTransform), so any
        // non-unit world scale here is an ANCESTOR's — which the runtime ignores,
        // re-introducing the gizmo/physics divergence #2 warns about. Flag it so
        // the divergence is visible rather than silent. (FIXES.md #2 caveat.)
        if (scale - glam::Vec3::ONE).abs().max_element() > 1e-3 {
            tracing::warn!(
                "collider node {node_id:?} has non-unit ancestor scale {:?}; \
                 the runtime ignores scale on colliders, so the wireframe (and \
                 physics) use shape extents only — the visual may diverge from \
                 scaled siblings",
                scale.to_array(),
            );
        }
        let world = glam::Mat4::from_rotation_translation(rot, trans);
        let (positions, colors) = super::collider_wire::build(&shape, &world);
        if positions.is_empty() {
            return None;
        }
        match r.add_line_segments(&positions, &colors, 1.5, false) {
            Ok(key) => key,
            Err(err) => {
                tracing::warn!("materialize_collider: add_line_segments failed: {err}");
                None
            }
        }
    })
    .await;
    if let Some(key) = line_key {
        entry2.line_keys.lock().unwrap().push(key);
    }
}

/// Projection decal (`NodeKind::Decal`) → inserts the renderer decal (inert
/// until a texture is assigned) plus a unit-cube volume wireframe so the decal
/// is placeable/visible in the editor (the projection volume).
///
/// Idempotent: drains any previously-inserted decal + wireframe first, so it
/// doubles as the re-bake the transform observer calls on every move/rotate/
/// scale (mirrors `materialize_collider`). Both the renderer decal (world-baked
/// `inverse_transform` + AABB) and the wireframe (world-baked line geometry)
/// are snapshots of the node's world matrix — without the re-bake they'd stay
/// at the materialize-time placement while the node moved on.
///
/// `declare_only` mirrors the other materializers (⭐ TRANSACTION PRINCIPLE):
/// when the decal's texture resolve stages a FRESH pool upload on a LIVE edit
/// (`declare_only == false`), this issues the `commit_load` that finalizes it;
/// a bulk load leaves the staged upload for the Replace join's single commit.
async fn materialize_decal(
    entry: Arc<RendererNode>,
    cfg: awsm_renderer_editor_protocol::DecalConfig,
    declare_only: bool,
) {
    let key = entry.transform_key;
    let entry2 = entry.clone();
    let alpha = cfg.alpha;
    let texture = cfg.texture;
    let old_decals: Vec<_> = entry.decal_keys.lock().unwrap().drain(..).collect();
    let old_lines: Vec<_> = entry.line_keys.lock().unwrap().drain(..).collect();
    let handle = renderer_handle();
    let mut r = handle.lock().await;
    for dk in old_decals {
        r.remove_decal(dk);
    }
    for lk in old_lines {
        r.remove_line(lk);
    }
    // Fresh world = parent's CURRENT world × this node's local. `get_world`
    // on the node itself is a cached matrix only refreshed by the render
    // loop's update_transforms, so reading it right after `set_local` (the
    // transform-observer re-bake) or right after the transform was first
    // established (whose cached world is seeded local-only, missing
    // ancestors) would bake a stale/incomplete placement into the decal —
    // same fix as `materialize_collider`.
    let local = r
        .transforms
        .get_local(key)
        .map(|t| t.to_matrix())
        .unwrap_or(glam::Mat4::IDENTITY);
    let parent_world = r
        .transforms
        .get_parent(key)
        .ok()
        .and_then(|p| r.transforms.get_world(p).ok().copied())
        .unwrap_or(glam::Mat4::IDENTITY);
    let world = parent_world * local;
    // A cache MISS below means the resolve STAGES a fresh pool upload
    // (procedural assets upload lazily at first bind) — probe it BEFORE the
    // resolve registers the key, so we know whether to commit afterwards.
    let texture_was_cached = texture.as_ref().map(|tref| {
        super::material::texture_binding_cached(
            tref.asset,
            true,
            awsm_renderer_core::texture::mipmap::MipmapTextureKind::Albedo,
        )
    });
    // Resolve the decal's texture asset to the pool's flat index the decal
    // shader unpacks (`array_index * stride + layer_index` — the same
    // packing the scene-loader uses). This was hard-coded to 0, which
    // sampled whatever texture happened to occupy pool slot (0,0) — an
    // editor decal never projected its ASSIGNED texture.
    let resolved = texture.as_ref().and_then(|tref| {
        super::material::resolve_texture_binding(
            &mut r,
            tref,
            true,
            awsm_renderer_core::texture::mipmap::MipmapTextureKind::Albedo,
        )
    });
    let texture_index = resolved
        .and_then(|(key, _sampler)| {
            let stride = awsm_renderer::decals::decal_texture_index_stride(&r.gpu);
            r.textures
                .get_entry(key)
                .ok()
                .map(|e| (e.array_index as u32) * stride + e.layer_index as u32)
        })
        .unwrap_or(0);
    match r.insert_decal(world, texture_index, alpha) {
        Ok(key) => entry2.decal_keys.lock().unwrap().push(key),
        Err(err) => tracing::warn!("insert_decal: {err:?}"),
    }
    // The decal volume is the ±1 oriented unit cube (2×2×2 m under unit
    // scale) — half-extents 1.0, so the wireframe shows the TRUE
    // projection volume. (Was 0.5: the affordance covered an eighth of
    // the volume the decal actually projects into.)
    let (positions, colors) = super::collider_wire::build(
        &awsm_renderer_editor_protocol::ColliderShape::Box {
            half_extents: [1.0, 1.0, 1.0],
        },
        &world,
    );
    if !positions.is_empty() {
        if let Ok(Some(lk)) = r.add_line_segments(&positions, &colors, 1.5, false) {
            entry2.line_keys.lock().unwrap().push(lk);
        }
    }
    // The resolve above staged a FRESH texture upload (first bind of a
    // procedural / captured-raster asset): the image only lives in the pool's
    // CPU staging until `commit_load` finalizes it — uploads the array,
    // rebuilds the decal pass's texture-pool bind group, and recompiles the
    // pool-baked shaders. No other path commits for a decal (it inserts no
    // mesh), so without this the decal projected the pool PLACEHOLDER (solid
    // white) until some unrelated edit happened to commit. Cache hits (incl.
    // every transform-observer re-bake) skip it, and a bulk load
    // (`declare_only`) defers to the Replace join's single commit — loading
    // is ONE transaction.
    if !declare_only && resolved.is_some() && texture_was_cached == Some(false) {
        if let Err(e) = r
            .commit_load(crate::engine::activity::commit_phase_handler())
            .await
        {
            tracing::warn!("materialize_decal commit_load: {e}");
        }
    }
}

/// The single curve node referenced by a sweep/instances node, if it exists and
/// is a `Curve`.
fn lookup_curve_def(node_id: NodeId) -> Option<awsm_renderer_editor_protocol::CurveDef> {
    let b = bridge();
    let entry = b.nodes.lock().unwrap().get(&node_id).cloned()?;
    match entry.node.kind.get_cloned() {
        NodeKind::Curve(c) => Some(c),
        _ => None,
    }
}

/// Insert an inline-material mesh + track it on the node (the shared path for
/// procedural geometry that isn't a primitive: sweeps, instances, shared mesh).
async fn upload_simple_mesh(
    entry: Arc<RendererNode>,
    raw: RawMeshData,
    mat: MeshMaterial,
    declare_only: bool,
) -> Option<awsm_renderer::meshes::MeshKey> {
    let parent_tk = entry.transform_key;
    let handle = renderer_handle();
    let mut r = handle.lock().await;
    // Vertex-colour usage is geometry-derived: painted meshes carry a non-empty
    // `colors` (uploaded as `COLOR_0`), so bind set 0 on the assigned built-in.
    let vertex_color_set = raw.colors.as_ref().filter(|c| !c.is_empty()).map(|_| 0u32);
    let mat_key = match &mat {
        MeshMaterial::Assigned(material) => {
            resolve_assigned_material(&mut r, material.as_ref(), vertex_color_set)
        }
        MeshMaterial::Flat(def) => material::insert_material(&mut r, def),
    };
    let sub_tk = r.transforms.insert(Transform::IDENTITY, Some(parent_tk));
    // `add_raw_mesh` is material-agnostic now: the geometry kind (visibility vs
    // transparency) is decided at commit from the bound material, so a
    // transmissive/blended captured mesh (e.g. an imported glass model) resolves
    // to transparency geometry without a separate entry point.
    match r.add_raw_mesh(raw, sub_tk, mat_key) {
        Ok(mk) => {
            // Apply the node's CURRENT shadow flags + visibility at creation,
            // exactly like the skinned / cluster materialize paths. Without
            // this, a `shadow.cast` (or any kind) edit re-materializes the
            // mesh back to renderer DEFAULTS: the kind observer tears down and
            // rebuilds, the visibility observer doesn't re-fire (node.visible
            // didn't change), and the shadow flags were never applied at all —
            // so `set_mesh_shadow cast=false` silently kept casting.
            let shadow_cfg = entry
                .node
                .kind
                .get_cloned()
                .mesh_shadow()
                .copied()
                .unwrap_or_default();
            let _ = r.set_mesh_shadow_flags(mk, mesh_shadow_flags_from_config(&shadow_cfg));
            let _ = r.set_mesh_hidden(mk, !effective_visible(entry.node_id));
            if !declare_only {
                if let Err(e) = r
                    .commit_load(crate::engine::activity::commit_phase_handler())
                    .await
                {
                    tracing::warn!("upload_simple_mesh commit_load: {e}");
                }
            }
            drop(r);
            entry.model_meshes.lock().unwrap().push(mk);
            entry.model_transforms.lock().unwrap().push(sub_tk);
            entry.material_keys.lock().unwrap().push(mat_key);
            bridge().register_mesh(mk, entry.node_id);
            Some(mk)
        }
        Err(e) => {
            r.transforms.remove(sub_tk);
            r.remove_material(mat_key);
            tracing::error!("upload_simple_mesh failed: {e}");
            None
        }
    }
}

/// Place copies of a source mesh along the referenced curve
/// (`NodeKind::InstancesAlongCurve`) via GPU instancing. Renders once both its
/// `curve_node` (a Curve) and `source_node` (a Mesh) point at real nodes.
async fn materialize_instances(
    entry: Arc<RendererNode>,
    def: awsm_renderer_editor_protocol::InstancesAlongCurveDef,
    declare_only: bool,
) {
    use awsm_renderer::instances::InstanceAttr;
    use awsm_renderer_curves::{CatmullRomCurve, Curve3, FrameSequence};

    // Both refs are optional; a nil sentinel just means "not wired up yet" — the
    // node renders empty until the user picks a curve + a source mesh.
    if def.curve_node.is_nil() || def.source_node.is_nil() {
        return;
    }
    let Some(curve_def) = lookup_curve_def(def.curve_node) else {
        tracing::warn!("InstancesAlongCurve references missing curve node");
        return;
    };
    // The source is a Mesh node; its baked geometry lives in the mesh cache.
    let mesh = {
        let b = bridge();
        let src = b.nodes.lock().unwrap().get(&def.source_node).cloned();
        let mesh_ref = match src.map(|e| e.node.kind.get_cloned()) {
            Some(NodeKind::Mesh { mesh, .. }) => mesh,
            _ => {
                tracing::warn!("InstancesAlongCurve source node is missing/not a Mesh");
                return;
            }
        };
        match super::mesh_cache::get_raw(mesh_ref.0) {
            Some(raw) => MeshData {
                positions: raw.positions,
                normals: raw.normals,
                uvs: raw.uv_sets,
                colors: raw.colors,
                indices: raw.indices,
            },
            None => {
                tracing::warn!("InstancesAlongCurve source mesh not in capture cache");
                return;
            }
        }
    };

    let curve = CatmullRomCurve::new(
        curve_def
            .control_points
            .iter()
            .map(|p| Vec3::from_array(*p))
            .collect(),
        curve_def.closed,
    );
    let total_len = curve.total_length(curve_def.sample_count.max(8) as usize);
    let spacing = def.spacing.max(0.05);
    let count = ((total_len / spacing).floor() as usize).max(1);
    let frames = FrameSequence::parallel_transport(&curve, count.max(2), Vec3::Y);

    let has_colors = !def.per_instance_colors.is_empty();
    let mut transforms = Vec::with_capacity(count);
    let mut attrs = Vec::with_capacity(count);
    for (i, frame) in frames.frames.iter().enumerate() {
        let mut translation = frame.position;
        if def.side_offset.abs() > 1.0e-4 {
            translation += frame.binormal * def.side_offset;
        }
        let rotation = if def.orient_to_tangent {
            frame.rotation()
        } else {
            Quat::IDENTITY
        };
        transforms.push(Transform {
            translation,
            rotation,
            scale: Vec3::ONE,
        });
        let rgba = if has_colors {
            def.per_instance_colors[i.min(def.per_instance_colors.len() - 1)]
        } else {
            [1.0, 1.0, 1.0, 1.0]
        };
        attrs.push(InstanceAttr::from_rgba_alpha_size(rgba, 1.0, 1.0));
    }

    let raw = RawMeshData {
        positions: mesh.positions,
        normals: mesh.normals,
        uv_sets: mesh.uvs,
        colors: mesh.colors,
        indices: mesh.indices,
        ..Default::default()
    };
    let mesh_key = upload_simple_mesh(
        entry,
        raw,
        MeshMaterial::Flat(awsm_renderer_editor_protocol::MaterialDef::default()),
        declare_only,
    )
    .await;
    if let Some(mk) = mesh_key {
        with_renderer_mut(move |r| {
            if let Err(err) = r.enable_mesh_instancing_opaque(mk, &transforms) {
                tracing::warn!("enable_mesh_instancing_opaque failed: {err}");
            }
            if has_colors {
                if let Ok(tk) = r.meshes.get(mk).map(|m| m.transform_key) {
                    if let Err(err) = r.set_mesh_instance_attrs(tk, &attrs) {
                        tracing::warn!("set_mesh_instance_attrs failed: {err}");
                    }
                }
            }
        })
        .await;
    }
}

/// Explicit instancer (`NodeKind::Instancer`): draw the referenced mesh ASSET
/// once with the node's authored per-instance transforms via GPU instancing —
/// ONE geometry upload (`upload_simple_mesh`) + one instance buffer
/// (`enable_mesh_instancing_opaque`), exactly like [`materialize_instances`]
/// but with the transform list stored on the node instead of derived from a
/// curve. Per-instance colors ride the same instance-attribute path.
async fn materialize_instancer(
    entry: Arc<RendererNode>,
    def: awsm_renderer_editor_protocol::InstancerDef,
    declare_only: bool,
) {
    use awsm_renderer::instances::InstanceAttr;

    // A nil mesh ref just means "not wired up yet" — the node renders empty
    // until the user picks a mesh (mirrors InstancesAlongCurve's nil refs).
    // An empty transform list is likewise a valid authored state (instancing
    // requires ≥ 1 transform, so there is nothing to draw).
    if def.mesh.0.is_nil() || def.transforms.is_empty() {
        return;
    }
    let Some(raw) = super::mesh_cache::get_raw(def.mesh.0) else {
        tracing::warn!("Instancer mesh {} not in the capture cache", def.mesh.0);
        return;
    };

    let transforms: Vec<Transform> = def.transforms.iter().map(trs_to_transform).collect();
    let has_colors = !def.per_instance_colors.is_empty();
    let attrs: Vec<InstanceAttr> = if has_colors {
        // Expand to the instance count, repeating the last authored value (the
        // def's documented semantics — same as InstancesAlongCurve).
        (0..transforms.len())
            .map(|i| {
                let rgba = def.per_instance_colors[i.min(def.per_instance_colors.len() - 1)];
                InstanceAttr::from_rgba_alpha_size(rgba, 1.0, 1.0)
            })
            .collect()
    } else {
        Vec::new()
    };

    let mesh_key = upload_simple_mesh(
        entry,
        raw,
        MeshMaterial::Flat(awsm_renderer_editor_protocol::MaterialDef::default()),
        declare_only,
    )
    .await;
    if let Some(mk) = mesh_key {
        with_renderer_mut(move |r| {
            if let Err(err) = r.enable_mesh_instancing_opaque(mk, &transforms) {
                tracing::warn!("enable_mesh_instancing_opaque failed: {err}");
            }
            if has_colors {
                if let Ok(tk) = r.meshes.get(mk).map(|m| m.transform_key) {
                    if let Err(err) = r.set_mesh_instance_attrs(tk, &attrs) {
                        tracing::warn!("set_mesh_instance_attrs failed: {err}");
                    }
                }
            }
        })
        .await;
    }
}

/// Particle emitter (`NodeKind::ParticleEmitter`) → an auto-playing simulator +
/// instanced billboard quad, ticked each frame by the render loop.
async fn materialize_particle(
    entry: Arc<RendererNode>,
    def: awsm_renderer_editor_protocol::ParticleEmitterDef,
    declare_only: bool,
) {
    let parent_tk = entry.transform_key;
    let node_id = entry.node_id;
    // §14: the blend route compiles a transparent instancing pipeline, so the
    // materialize is async — hold the renderer lock across it (like `commit_load`
    // below) instead of the sync `with_renderer_mut` closure.
    {
        let handle = renderer_handle();
        let mut r = handle.lock().await;
        let world_pos = r
            .transforms
            .get_world(parent_tk)
            .map(|m| m.w_axis.truncate())
            .unwrap_or(Vec3::ZERO);
        super::particles::materialize(&mut r, node_id, parent_tk, world_pos, &def).await;
    }
    // The emitter inserts a PBR material (a feature-set variant) whose pipeline
    // must compile — route through the one compile path. Skipped under the bulk-load
    // join (which commits once at the end). render() no longer compiles reactively.
    if declare_only {
        return;
    }
    if let Err(e) = renderer_handle()
        .lock()
        .await
        .commit_load(crate::engine::activity::commit_phase_handler())
        .await
    {
        tracing::warn!("particle commit_load: {e}");
    }
}

async fn apply_light(entry: Arc<RendererNode>, cfg: LightConfig) {
    let node_id = entry.node_id;
    // A hidden light node contributes NO light: honor EFFECTIVE visibility
    // (own eye AND ancestors' — a light inside a hidden group is off too) at
    // materialize time (mirrors meshes applying hidden-at-creation), so the
    // outliner eye survives re-materialization and a project saved with a
    // hidden light loads dark. Still registered in `light_node_ids` so the
    // viewport icon keeps marking the node. The visibility observer
    // re-materializes on show (see `apply_subtree_visibility`).
    if !effective_visible(entry.node_id) {
        bridge().light_node_ids.lock().unwrap().insert(node_id);
        return;
    }
    let trs = entry.node.transform.get();
    let pos = Vec3::from_array(trs.translation);
    let dir = (Quat::from_array(trs.rotation) * Vec3::NEG_Z).normalize_or_zero();
    let light = light_from_config(&cfg, pos, dir);

    let shadow_params = light_shadow_params_from_config(cfg.shadow());
    let casts = shadow_params.cast;
    let parent_tk = entry.transform_key;
    let key = with_renderer_mut(move |r| {
        let key = r.insert_light(light, Some(shadow_params));
        // Bind the light to its node transform so the per-frame
        // `update_from_transforms` re-derives position/direction whenever the
        // light node moves/rotates — without this a directional light's
        // direction is frozen at materialize time and casts no useful shadow.
        if let Ok(k) = key {
            r.lights.bind_transform(k, parent_tk);
        }
        key
    })
    .await;
    // Lazily compile the shadow pipelines when a casting light first lands so the
    // next frame can draw shadows (no-op once compiled / when nothing casts).
    if casts {
        let handle = renderer_handle();
        let mut r = handle.lock().await;
        if let Err(e) = r.ensure_shadow_pipelines_compiled().await {
            tracing::warn!("ensure_shadow_pipelines_compiled: {e:?}");
        }
    }
    match key {
        Ok(k) => {
            *entry.light_key.lock().unwrap() = Some(k);
            bridge().light_node_ids.lock().unwrap().insert(node_id);
        }
        Err(e) => tracing::error!("insert_light failed: {e:?}"),
    }
}

/// Materialize a `Camera` node into the renderer's camera-params store. The node
/// has no GPU geometry — this slot mirrors the node's `CameraConfig` and is what
/// an `AnimationTarget::Camera` channel mutates. The render loop reads this slot
/// (not the node config directly) so an animated camera is live; for a static
/// camera the slot equals the config, so the projection is unchanged.
///
/// `apply_kind` tears down (removing any prior slot) before this runs, and the
/// kind observer re-fires on every `SetKind`, so editing the camera config
/// re-inserts a slot that reflects the new config — keeping store and config in
/// sync without a separate observer.
async fn materialize_camera(
    entry: Arc<RendererNode>,
    cfg: awsm_renderer_editor_protocol::CameraConfig,
) {
    let params = camera_params_from_config(&cfg);
    let key = with_renderer_mut(move |r| r.cameras.insert(params)).await;
    *entry.camera_key.lock().unwrap() = Some(key);
}

pub(crate) fn trs_to_transform(trs: &Trs) -> Transform {
    Transform {
        translation: Vec3::from_array(trs.translation),
        rotation: Quat::from_array(trs.rotation),
        scale: Vec3::from_array(trs.scale),
    }
}

/// Re-materialize every `NodeKind::Mesh` node — the `mesh_sync` observer's
/// response to a `mesh_revision` bump. `SetMeshData` replaces an editable mesh's
/// bytes in the store *without* changing the node kind, so the per-node `kind`
/// observer wouldn't re-fire on its own; this re-runs `apply_kind` (which re-reads
/// `mesh_cache::get_raw`) for the affected nodes.
pub(crate) async fn rematerialize_mesh_nodes() {
    // Hold the `WaitRenderSettled` barrier open across the sweep (see the kind
    // observer's guard) — a mesh-edit re-sync compiles too.
    let _guard = crate::controller::CompileGuard::new();
    let entries: Vec<Arc<RendererNode>> =
        bridge().nodes.lock().unwrap().values().cloned().collect();
    let mut declared = false;
    for entry in entries {
        let kind = entry.node.kind.get_cloned();
        // Instancers read the same mesh cache, so a geometry edit to a shared
        // mesh asset must re-upload them too (their kind didn't change either).
        if matches!(kind, NodeKind::Mesh { .. } | NodeKind::Instancer(_)) {
            // ⭐ TRANSACTION grain = the USER OP, not the node: one mesh edit
            // can re-materialize MANY referencing nodes (shared mesh asset →
            // several Mesh/Instancer nodes) — declare each, commit ONCE below
            // (was: a full commit_load per node inside apply_kind).
            apply_kind(entry, kind, true).await;
            declared = true;
        }
    }
    if declared {
        commit_bulk_load().await;
    }
}

#[cfg(test)]
mod load_settle_barrier_tests {
    use super::{arm_load_settle_barrier, release_load_settle_barrier};

    /// The load-settle barrier must pair arm/release exactly onto
    /// `compile_pending` (what `wait_render_settled` polls), tolerate an
    /// UNARMED release (a New-Project root Replace runs the release path with
    /// nothing armed), never underflow, and support nested arming (two loads
    /// in flight → two Replaces → two releases).
    #[test]
    fn barrier_pairs_and_tolerates_unarmed_release() {
        crate::controller::init();
        let pending = || crate::controller::controller().compile_pending.get();
        let base = pending();
        // Unarmed release is a no-op (never underflows the settle counter).
        release_load_settle_barrier();
        assert_eq!(pending(), base);
        // One load: armed holds the barrier, release drops it.
        arm_load_settle_barrier();
        assert_eq!(pending(), base + 1);
        release_load_settle_barrier();
        assert_eq!(pending(), base);
        // Nested loads pair symmetrically; the extra release is a no-op.
        arm_load_settle_barrier();
        arm_load_settle_barrier();
        assert_eq!(pending(), base + 2);
        release_load_settle_barrier();
        release_load_settle_barrier();
        release_load_settle_barrier();
        assert_eq!(pending(), base);
    }
}

#[cfg(test)]
mod slot_texture_tests {
    use super::merge_slot_texture;
    use awsm_renderer_editor_protocol::{AssetId, TextureRef};

    // §11 regression guard: a per-mesh inline texture must ENABLE a slot the
    // shared variant lacks (the old code forced `None` here → flat render).
    #[test]
    fn inline_enables_slot_the_variant_lacks() {
        let t = TextureRef::new(AssetId::new());
        assert_eq!(merge_slot_texture(Some(t), None, None), Some(t));
    }

    #[test]
    fn inline_beats_override_and_variant() {
        let inline = TextureRef::new(AssetId::new());
        let over = TextureRef::new(AssetId::new());
        let def = TextureRef::new(AssetId::new());
        assert_eq!(
            merge_slot_texture(Some(inline), Some(over), Some(def)),
            Some(inline)
        );
    }

    #[test]
    fn override_beats_variant_without_inline() {
        let over = TextureRef::new(AssetId::new());
        let def = TextureRef::new(AssetId::new());
        assert_eq!(merge_slot_texture(None, Some(over), Some(def)), Some(over));
    }

    #[test]
    fn variant_default_is_the_fallback() {
        let def = TextureRef::new(AssetId::new());
        assert_eq!(merge_slot_texture(None, None, Some(def)), Some(def));
        assert_eq!(merge_slot_texture(None, None, None), None);
    }
}

#[cfg(test)]
mod builtin_merge_tests {
    use super::merged_builtin_def;
    use awsm_renderer_editor_protocol::dynamic_material::MaterialInstance;
    use awsm_renderer_editor_protocol::material::{ClearcoatExt, MaterialDef};
    use awsm_renderer_editor_protocol::{AssetId, TextureRef};

    fn inst(inline: MaterialDef) -> MaterialInstance {
        MaterialInstance {
            asset: AssetId::new(),
            inline,
            ..Default::default()
        }
    }

    fn with_clearcoat(factor: f32, roughness_factor: f32) -> MaterialDef {
        MaterialDef {
            extensions: awsm_renderer_editor_protocol::material::PbrExtensions {
                clearcoat: Some(ClearcoatExt {
                    factor,
                    roughness_factor,
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    // Extensions are STRICT capabilities: the ENABLE set is variant-only.
    // Library None + inline Some(clearcoat) must NOT enable the extension —
    // an inline-only extension has no compiled code to render through, so
    // the merge drops it (enabling clearcoat is a material edit).
    #[test]
    fn inline_extension_cannot_enable_what_the_library_lacks() {
        let variant = MaterialDef::default();
        let merged = merged_builtin_def(&inst(with_clearcoat(1.0, 0.0)), &variant);
        assert!(
            merged.extensions.clearcoat.is_none(),
            "extension enables are variant-only; inline-only clearcoat must be dropped"
        );
    }

    // Library Some(factor 0) + inline Some(factor 1): the inline factors must
    // reach the merged def (they feed the per-mesh uniforms).
    #[test]
    fn inline_extension_factors_beat_the_library() {
        let variant = with_clearcoat(0.0, 0.5);
        let merged = merged_builtin_def(&inst(with_clearcoat(1.0, 0.0)), &variant);
        let cc = merged.extensions.clearcoat.unwrap();
        assert_eq!(cc.factor, 1.0);
        assert_eq!(cc.roughness_factor, 0.0);
    }

    // An enabled-but-inline-unseeded extension carries the library's AUTHORED
    // factors, not struct defaults.
    #[test]
    fn library_authored_factors_fill_an_unseeded_inline() {
        let variant = with_clearcoat(0.7, 0.3);
        let merged = merged_builtin_def(&inst(MaterialDef::default()), &variant);
        let cc = merged.extensions.clearcoat.unwrap();
        assert_eq!(cc.factor, 0.7);
        assert_eq!(cc.roughness_factor, 0.3);
    }

    // Export parity: `flatten_builtin_materials` writes the merged def into the
    // bundle node's `inline`, and the player re-resolves that inline over the
    // same library def. Editor rendering == player rendering therefore requires
    // the merge to be a FIXED POINT over its own output — including extensions
    // and texture slots.
    #[test]
    fn merge_is_idempotent_over_the_flattened_inline() {
        let mut variant = with_clearcoat(0.7, 0.3);
        variant.base_color_texture = Some(TextureRef::new(AssetId::new()));
        variant.metallic = 1.0;
        let mut inline = with_clearcoat(1.0, 0.0);
        inline.normal_texture = Some(TextureRef::new(AssetId::new()));
        inline.base_color = [0.5, 0.25, 0.125, 1.0];

        let merged = merged_builtin_def(&inst(inline), &variant);
        // Simulate the player's view of the flattened bundle: inline = merged,
        // per-instance override maps empty (they don't ship in the bundle).
        let reflattened = merged_builtin_def(&inst(merged.clone()), &variant);
        assert_eq!(reflattened, merged);
    }

    // Alpha MODE is variant-only routing: a per-node inline mode must never
    // reroute the mesh. Only the Mask cutoff VALUE (a uniform) carries from
    // inline, and only when the variant is Mask.
    #[test]
    fn alpha_mode_is_variant_only() {
        use awsm_renderer_editor_protocol::material::MaterialAlphaMode;
        let variant = MaterialDef::default(); // Opaque
        let inline = MaterialDef {
            alpha_mode: MaterialAlphaMode::Mask { cutoff: 0.5 },
            ..Default::default()
        };
        let merged = merged_builtin_def(&inst(inline), &variant);
        assert_eq!(merged.alpha_mode, MaterialAlphaMode::Opaque);

        let mask_variant = MaterialDef {
            alpha_mode: MaterialAlphaMode::Mask { cutoff: 0.5 },
            ..Default::default()
        };
        let inline = MaterialDef {
            alpha_mode: MaterialAlphaMode::Mask { cutoff: 0.25 },
            ..Default::default()
        };
        let merged = merged_builtin_def(&inst(inline), &mask_variant);
        assert_eq!(merged.alpha_mode, MaterialAlphaMode::Mask { cutoff: 0.25 });
    }

    // Texture binds are pure data: an inline image binds into ANY slot (the
    // slot code is always compiled; unbound slots sample the 1×1 neutral).
    #[test]
    fn inline_textures_bind_any_slot() {
        let variant = MaterialDef::default();
        let inline = MaterialDef {
            normal_texture: Some(TextureRef::new(AssetId::new())),
            emissive_texture: Some(TextureRef::new(AssetId::new())),
            ..Default::default()
        };
        let merged = merged_builtin_def(&inst(inline), &variant);
        assert!(merged.normal_texture.is_some());
        assert!(merged.emissive_texture.is_some());
    }
}
