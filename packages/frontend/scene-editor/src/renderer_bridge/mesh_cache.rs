//! Editor-side captured-mesh asset cache.
//!
//! `NodeKind::Mesh` references geometry via an `AssetSource::Mesh(MeshDef)`
//! entry in the project's asset table. The geometry itself lives in a side
//! file (`assets/<asset-id>.mesh.bin`) so `project.json` stays small.
//!
//! This module is the editor's load-on-demand decoder for those side files:
//! first look-up bitcode-decodes the bytes via either `pending_assets`
//! (freshly captured this session, or fetched after a project load) or the
//! project directory on disk; subsequent calls hit the cache.
//!
//! Mirrors `material_cache` / `texture_cache` in shape. Process-global
//! because the bridge's `with_renderer_mut` closures sit deep in the async
//! stack — threading the cache through every signature would balloon arity
//! for no benefit.
//!
//! Note: this cache holds *decoded* geometry (`CapturedMesh`). The renderer
//! side translates it to `RawMeshData` at materialize time — keeping the
//! decoded form here means a re-materialize doesn't repeat the bitcode
//! decode.

use std::collections::HashMap;
use std::sync::Mutex;

use awsm_scene_schema::{AssetId, AssetSource, CapturedMesh, MeshRef};

use crate::state::app_state;

static CACHE: Mutex<Option<HashMap<AssetId, CapturedMesh>>> = Mutex::new(None);

fn with_cache<R>(f: impl FnOnce(&mut HashMap<AssetId, CapturedMesh>) -> R) -> R {
    let mut guard = CACHE.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

/// Insert a freshly-captured mesh into the cache without going through
/// disk. Called by the "Capture as asset" action immediately after it
/// writes the bytes into `pending_assets`. Subsequent materializes hit
/// the cache directly; the disk-write happens on Save.
pub fn insert(asset_id: AssetId, mesh: CapturedMesh) {
    with_cache(|m| {
        m.insert(asset_id, mesh);
    });
}

/// Resolve a `MeshRef` to a `CapturedMesh`, loading from disk on first
/// access. Returns `None` if the asset is missing from the table, isn't
/// a `Mesh` source, or the side file can't be read / decoded.
pub async fn get_or_load(mesh_ref: MeshRef) -> Option<CapturedMesh> {
    let asset_id = mesh_ref.0;
    if let Some(hit) = with_cache(|m| m.get(&asset_id).cloned()) {
        return Some(hit);
    }
    let state = app_state();
    // Must be a Mesh source — bail otherwise.
    {
        let table = state.scene.assets.lock().unwrap();
        match table.get(asset_id).map(|e| &e.source) {
            Some(AssetSource::Mesh(_)) => {}
            other => {
                tracing::warn!(
                    "mesh_cache::get_or_load: asset {asset_id} isn't a Mesh source ({other:?})"
                );
                return None;
            }
        }
    }
    // pending_assets carries freshly-captured bytes (or anything Load
    // pre-fetched into memory). On miss, fall through to disk. The
    // pending-lookup + the directory snapshot run inside scopes that
    // drop their locks before any await — holding a sync Mutex across
    // an await would deadlock under the bridge's async pattern.
    let pending = state.pending_assets.lock().unwrap().get(&asset_id).cloned();
    let bytes = match pending {
        Some(b) => b,
        None => {
            let dir = state.project.lock().unwrap().directory.clone()?;
            let entry = state.scene.assets.lock().unwrap().get(asset_id).cloned()?;
            let disk_path = awsm_scene_schema::asset_disk_path(asset_id, &entry)?;
            match dir.read_bytes(&disk_path).await {
                Ok(b) => b,
                Err(err) => {
                    tracing::warn!(
                        "mesh_cache::get_or_load: read {disk_path} failed for asset {asset_id}: {err}"
                    );
                    return None;
                }
            }
        }
    };
    match bitcode::deserialize::<CapturedMesh>(&bytes) {
        Ok(mesh) => {
            with_cache(|m| {
                m.insert(asset_id, mesh.clone());
            });
            Some(mesh)
        }
        Err(err) => {
            tracing::warn!("mesh_cache::get_or_load: bitcode decode failed for {asset_id}: {err}");
            None
        }
    }
}

/// Clear the cache. Called between project loads so a recycled AssetId
/// never reuses prior project's geometry.
pub fn clear() {
    with_cache(|m| m.clear());
}

/// Vertex + triangle counts for the captured mesh behind `mesh_ref`,
/// or `None` if the cache hasn't loaded it yet (the inspector renders
/// a "loading…" placeholder in that case). Sync — never triggers the
/// disk fetch; the next materialize for this node will.
pub struct MeshStats {
    pub vertex_count: usize,
    pub triangle_count: usize,
}

pub fn stats(mesh_ref: MeshRef) -> Option<MeshStats> {
    with_cache(|m| {
        m.get(&mesh_ref.0).map(|c| MeshStats {
            vertex_count: c.positions.len(),
            triangle_count: c.indices.len() / 3,
        })
    })
}
