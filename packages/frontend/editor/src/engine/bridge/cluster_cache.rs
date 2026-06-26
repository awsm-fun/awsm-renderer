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
//! Cross-reload persistence (like `skinned_bake_cache`): the cache itself is
//! session-local, but [`crate::controller::persistence::cluster_files`] writes each
//! referenced DAG to `assets/<source>.clusters.bin` on Save, and
//! `restore_cluster_meshes` re-reads it back into this cache BEFORE the scene
//! materializes on Load — so a `ClusterMesh` node survives Save → reload. The same
//! file ships in the player bundle (`export::bake_player_bundle`), where the runtime
//! `NodeKind::ClusterMesh` arm fetches it under the identical name.

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

/// Drop ALL cached cluster meshes — models a cold page reload (used by the
/// `ReloadProjectInMemory` round-trip self-test, which then re-reads each DAG from
/// the persisted `assets/<source>.clusters.bin` via `restore_cluster_meshes`).
pub fn clear() {
    CLUSTER_MESHES.with(|c| c.borrow_mut().clear());
}
