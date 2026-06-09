//! Session-local cache of **bind-pose geometry** for imported skinned glTF nodes.
//!
//! A `NodeKind::SkinnedMesh` is rendered + deformed by the renderer's glTF skin
//! path; it has no captured-mesh asset. `drop_skinning` (the terminal bridge to
//! editing) needs the skinned node's **bind-pose** triangles to bake into a
//! static, editable `Mesh{ stack:{ base: Captured } }`. Rather than read those
//! back from the GPU skin, we stash them here at import time: `extract_node_mesh`
//! reads positions/normals/uvs/colors from the document accessors **without**
//! JOINTS/WEIGHTS, which IS the bind pose.
//!
//! Keyed by `(source file AssetId, glTF node_index, primitive_index)` — the same
//! triple a [`SkinnedMeshRef`](awsm_editor_protocol::SkinnedMeshRef) carries — so a
//! `drop_skinning` on any skinned node resolves its bake directly.
//!
//! TODO(cross-reload persistence): like the old `model_source_cache`, this is
//! session-local — it does NOT survive a project Save → reload. A reloaded
//! project's `SkinnedMesh` nodes re-render only if the source is re-imported; a
//! `drop_skinning` after a cold reload (no cached bake) currently errors. Full
//! persistence would write each skinned node's bind-pose bytes to the project's
//! `assets/` on Save and read them back on Load.

use std::cell::RefCell;
use std::collections::HashMap;

use awsm_editor_protocol::AssetId;
use awsm_glb_export::MeshData;

/// Cache key: `(source file AssetId, glTF node_index, primitive_index)` — the
/// triple a `SkinnedMeshRef` carries.
type BakeKey = (AssetId, u32, Option<u32>);

thread_local! {
    static SKINNED_BAKES: RefCell<HashMap<BakeKey, MeshData>> = RefCell::new(HashMap::new());
}

/// Stash a skinned node's bind-pose geometry under its `(source, node_index,
/// primitive_index)` key (idempotent — re-storing replaces).
pub fn store(source: AssetId, node_index: u32, primitive_index: Option<u32>, mesh: MeshData) {
    SKINNED_BAKES.with(|c| {
        c.borrow_mut()
            .insert((source, node_index, primitive_index), mesh)
    });
}

/// The cached bind-pose geometry for a skinned node, if present.
pub fn get(source: AssetId, node_index: u32, primitive_index: Option<u32>) -> Option<MeshData> {
    SKINNED_BAKES.with(|c| {
        c.borrow()
            .get(&(source, node_index, primitive_index))
            .cloned()
    })
}

/// Drop every cached bake (project reset).
pub fn clear() {
    SKINNED_BAKES.with(|c| c.borrow_mut().clear());
}
