//! In-memory captured-mesh cache. "Capture as Mesh asset" freezes a procedural
//! node's geometry into a [`CapturedMesh`] under a fresh [`AssetId`]; later
//! `NodeKind::Mesh` nodes referencing that id load the geometry back here. This
//! is the session-local store; persisting captures to the project's
//! `assets/<id>.mesh` side files is the follow-on.

use std::cell::RefCell;
use std::collections::HashMap;

use awsm_meshgen::MeshData;
use awsm_renderer::raw_mesh::RawMeshData;
use awsm_scene_schema::{AssetId, CapturedMesh};

thread_local! {
    static CAPTURED: RefCell<HashMap<AssetId, CapturedMesh>> = RefCell::new(HashMap::new());
}

/// Freeze a generated mesh into a `CapturedMesh`.
pub fn from_mesh_data(m: MeshData) -> CapturedMesh {
    CapturedMesh {
        positions: m.positions,
        normals: m.normals,
        uvs: m.uvs,
        colors: m.colors,
        indices: m.indices,
    }
}

/// Store a captured mesh under a fresh id and return it (for a `MeshRef`).
pub fn store(captured: CapturedMesh) -> AssetId {
    let id = AssetId::new();
    CAPTURED.with(|c| c.borrow_mut().insert(id, captured));
    id
}

/// Resolve a captured-mesh id to renderer-ready geometry, if present.
pub fn get_raw(id: AssetId) -> Option<RawMeshData> {
    CAPTURED.with(|c| {
        c.borrow().get(&id).map(|m| RawMeshData {
            positions: m.positions.clone(),
            normals: m.normals.clone(),
            uvs: m.uvs.clone(),
            colors: m.colors.clone(),
            indices: m.indices.clone(),
        })
    })
}
