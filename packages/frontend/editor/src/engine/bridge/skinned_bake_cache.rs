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
//! triple a [`SkinnedMeshRef`](awsm_renderer_editor_protocol::SkinnedMeshRef) carries — so a
//! `drop_skinning` on any skinned node resolves its bake directly.
//!
//! Cross-reload persistence (DONE): `persistence::bind_pose_files` writes each
//! skinned node's bind-pose bytes to `assets/<source>.<node>.<prim>.bake.bin` on
//! Save, and `restore_bind_poses` re-stashes them on Load — so `drop_skinning`
//! works after a cold reload. The clean rig glb (`rig_glb_files`/
//! `restore_skinned_templates`) similarly makes `SkinnedMesh` nodes re-render
//! without a re-import. (This store itself stays session-local; the side files are
//! the persisted source of truth.)

use std::cell::RefCell;
use std::collections::HashMap;

use awsm_renderer_editor_protocol::AssetId;
use awsm_renderer_glb_export::{ExtractedNodeMesh, MeshData};

/// Cache key: `(source file AssetId, glTF node_index, primitive_index)` — the
/// triple a `SkinnedMeshRef` carries.
type BakeKey = (AssetId, u32, Option<u32>);

thread_local! {
    static SKINNED_BAKES: RefCell<HashMap<BakeKey, MeshData>> = RefCell::new(HashMap::new());
    /// Per-`(source, RIG-GLB node_index, primitive_index)` decode of the clean rig
    /// glb — geometry + 2nd UV set + the per-node skin (joints/weights/IBMs). The
    /// MATERIALISER reads this to rebuild a skinned drawable from our-format; cached
    /// so repeated (re-)materialise of the same node doesn't re-parse the glb.
    /// Keyed by the RIG-GLB index (`SkinnedMeshRef::rig_node_index`), NOT the
    /// original `node_index`. Session-local (same caveat as the bind-pose cache).
    static RIG_NODE_DECODES: RefCell<HashMap<BakeKey, ExtractedNodeMesh>> =
        RefCell::new(HashMap::new());
    /// Per-imported-source the **clean rig glb** (geometry + skeleton + joints/
    /// weights + morph, re-exported through our writer; materials/anims dropped),
    /// keyed by the source-file `AssetId`. This is what the player bundle ships
    /// for the import's skinned nodes (`assets/<source>.glb`). Session-local (same
    /// caveat as the bind-pose cache above).
    static SOURCE_RIG_GLB: RefCell<HashMap<AssetId, Vec<u8>>> = RefCell::new(HashMap::new());
}

/// Stash the clean rig glb for an imported source (keyed by its source-file id).
pub fn store_rig_glb(source: AssetId, glb: Vec<u8>) {
    SOURCE_RIG_GLB.with(|c| c.borrow_mut().insert(source, glb));
}

/// The clean rig glb for an imported source, if present (the bundle reads this).
pub fn get_rig_glb(source: AssetId) -> Option<Vec<u8>> {
    SOURCE_RIG_GLB.with(|c| c.borrow().get(&source).cloned())
}

/// Decode the clean rig glb for `source` at `rig_node_index` (+ optional
/// primitive) into geometry + skin — the MATERIALISER's per-node our-format read.
/// Lazily parses the rig glb on first request and caches the result per
/// `(source, rig_node_index, primitive_index)`. Returns `None` when no rig glb is
/// cached for the source or the node carries no extractable mesh.
pub fn get_rig_node_decode(
    source: AssetId,
    rig_node_index: u32,
    primitive_index: Option<u32>,
) -> Option<ExtractedNodeMesh> {
    let key = (source, rig_node_index, primitive_index);
    if let Some(hit) = RIG_NODE_DECODES.with(|c| c.borrow().get(&key).cloned()) {
        return Some(hit);
    }
    let bytes = get_rig_glb(source)?;
    let decoded = awsm_renderer_glb_export::extract_node_mesh_with_skin_from_bytes(
        &bytes,
        rig_node_index,
        primitive_index,
    )?;
    RIG_NODE_DECODES.with(|c| c.borrow_mut().insert(key, decoded.clone()));
    Some(decoded)
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

/// Drop one import's cached bakes + rig glb (its last scene instance deleted
/// mid-session). Counterpart to [`store`]/[`store_rig_glb`].
pub fn remove(source: AssetId) {
    SKINNED_BAKES.with(|c| c.borrow_mut().retain(|(s, _, _), _| *s != source));
    RIG_NODE_DECODES.with(|c| c.borrow_mut().retain(|(s, _, _), _| *s != source));
    SOURCE_RIG_GLB.with(|c| {
        c.borrow_mut().remove(&source);
    });
}

/// Drop every cached bake + rig glb (project reset).
pub fn clear() {
    SKINNED_BAKES.with(|c| c.borrow_mut().clear());
    RIG_NODE_DECODES.with(|c| c.borrow_mut().clear());
    SOURCE_RIG_GLB.with(|c| c.borrow_mut().clear());
}
