//! In-memory captured-mesh store. "Capture as Mesh asset" freezes a procedural
//! node's geometry into a [`CapturedMesh`] under a fresh [`AssetId`]; later
//! `NodeKind::Mesh` nodes referencing that id load the geometry back here.
//!
//! Persistence (Phase 2): `controller::persistence` serializes each entry to the
//! project's `assets/<id>.mesh.bin` side file on Save (via [`get_captured`]) and
//! restores it on Load (via [`store_with_id`], **before** nodes materialize so
//! `get_raw` resolves). The `get_raw`/`store` API is unchanged so `node_sync`
//! stays untouched.

use std::cell::RefCell;
use std::collections::HashMap;

use awsm_editor_protocol::{AssetId, CapturedMesh};
use awsm_meshgen::MeshData;
use awsm_renderer::raw_mesh::RawMeshData;

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

/// Store (or replace) a captured mesh under a **known** id — the Load path
/// restoring `assets/<id>.mesh.bin`, and the raw-edit command path
/// (`SetMeshData`) overwriting an editable mesh in place.
pub fn store_with_id(id: AssetId, captured: CapturedMesh) {
    CAPTURED.with(|c| c.borrow_mut().insert(id, captured));
}

/// The stored `CapturedMesh` for `id`, if present — used to serialize the side
/// file on Save and to capture the prior bytes for an undo inverse.
pub fn get_captured(id: AssetId) -> Option<CapturedMesh> {
    CAPTURED.with(|c| c.borrow().get(&id).cloned())
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
