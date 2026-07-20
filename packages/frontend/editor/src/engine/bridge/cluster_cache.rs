//! Session-local cache of parsed **cluster-LOD ("cluster") meshes** for view-only
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

/// Drop a cached cluster mesh — called from `node_sync::remove_node` when the last
/// `ClusterMesh` node referencing this source is deleted, so a view-only cluster
/// import doesn't leak its parsed DAG (tens of MB) for the rest of the session.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `remove` drops only the targeted source; others stay cached. Guards the
    /// teardown leak fix (a deleted ClusterMesh node frees its DAG, siblings keep
    /// theirs).
    #[test]
    fn remove_drops_only_the_targeted_source() {
        let a = AssetId::new();
        let b = AssetId::new();
        insert(a, ClusterMesh::default());
        insert(b, ClusterMesh::default());
        assert!(get(a).is_some() && get(b).is_some());
        remove(a);
        assert!(get(a).is_none(), "removed source must be gone");
        assert!(get(b).is_some(), "other source must remain");
        remove(b);
        assert!(get(b).is_none());
    }
}
