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

/// Freeze a generated mesh into a `CapturedMesh`. Procedural meshes are single-UV
/// (`MeshData` carries no 2nd set), so `uvs1` is `None`.
pub fn from_mesh_data(m: MeshData) -> CapturedMesh {
    let mut uv_sets = m.uvs.into_iter();
    CapturedMesh {
        positions: m.positions,
        normals: m.normals,
        uvs: uv_sets.next(),
        uvs1: uv_sets.next(),
        colors: m.colors,
        indices: m.indices,
    }
}

/// Inverse of [`from_mesh_data`]: a `CapturedMesh` (the bitcode-serializable
/// persisted form) back into a `MeshData`. Used by `persistence` to restore
/// skinned bind-pose bakes (which `skinned_bake_cache` stores as `MeshData`)
/// from their persisted `.bake.bin` side files.
pub fn to_mesh_data(c: CapturedMesh) -> MeshData {
    MeshData {
        positions: c.positions,
        normals: c.normals,
        // Fold the captured set 0 + optional set 1 back into the N-set uvs vec.
        uvs: c.uvs.into_iter().chain(c.uvs1).collect(),
        colors: c.colors,
        indices: c.indices,
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
            uv_sets: m.uvs.clone().into_iter().chain(m.uvs1.clone()).collect(),
            colors: m.colors.clone(),
            indices: m.indices.clone(),
            ..Default::default()
        })
    })
}
