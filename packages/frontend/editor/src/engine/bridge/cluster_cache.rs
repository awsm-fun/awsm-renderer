//! Session-local cache of parsed **cluster-LOD ("nanite") meshes** for view-only
//! [`NodeKind::ClusterMesh`](awsm_renderer_scene::tree::NodeKind::ClusterMesh)
//! nodes.
//!
//! A `ClusterMesh` node is rendered by the renderer's cluster pipeline from a
//! pre-baked DAG (`assets/<source>.clusters.bin`). At import we fetch + parse that
//! file once and stash it here, keyed by the node's source [`AssetId`]; the bridge
//! materializer ([`super::node_sync`]) reads it to drive
//! `scene-loader::materialize_cluster_mesh` — the SAME path the player uses.
//!
//! TODO(cross-reload persistence): like `skinned_bake_cache`, this is session-local
//! — it does NOT survive a project Save → reload. Full persistence would write the
//! `.clusters.bin` into the project's `assets/` on Save and re-read it on Load (and
//! re-populate this cache). For now a reloaded project's `ClusterMesh` nodes
//! re-render only after the source is re-imported.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use awsm_renderer_editor_protocol::AssetId;
use awsm_renderer_lod_bake::ClusterMesh;

thread_local! {
    static CLUSTER_MESHES: RefCell<HashMap<AssetId, Rc<ClusterMesh>>> =
        RefCell::new(HashMap::new());
}

/// Stash a parsed cluster mesh under its source asset id (called at import).
pub fn insert(source: AssetId, cm: ClusterMesh) {
    CLUSTER_MESHES.with(|c| c.borrow_mut().insert(source, Rc::new(cm)));
}

/// Fetch the cached cluster mesh for a source asset id, if present.
pub fn get(source: AssetId) -> Option<Rc<ClusterMesh>> {
    CLUSTER_MESHES.with(|c| c.borrow().get(&source).cloned())
}

/// Drop a cached cluster mesh (e.g. when its node + asset are removed).
#[allow(dead_code)] // teardown hook — wired when ClusterMesh node removal lands
pub fn remove(source: AssetId) {
    CLUSTER_MESHES.with(|c| {
        c.borrow_mut().remove(&source);
    });
}
