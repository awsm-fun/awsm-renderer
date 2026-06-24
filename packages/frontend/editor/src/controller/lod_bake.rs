//! Export-time discrete-LOD bake for **static** meshes.
//!
//! Called from [`bake_player_bundle`](super::export::bake_player_bundle) per
//! LOD-enabled `RuntimeMesh::Glb` mesh asset. Generates the simplified level
//! chain with the shared pure-Rust simplifier ([`awsm_renderer_lod_bake`]),
//! writes one geometry-only glb per level (`<id>.lod{N}.glb`) plus the
//! [`MeshLodManifest`] sidecar (`<id>.lod.toml`), and caches the result by
//! geometry hash so re-exporting an unchanged mesh doesn't re-simplify.
//!
//! Skinned / morph meshes take a different source (`get_rig_glb`) and need skin
//! weight + morph delta carry-through — that path is baked separately.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use awsm_renderer_editor_protocol::BundleFile;
use awsm_renderer_glb_export::{write_glb, ExportNode, GlbScene, MeshData};
use awsm_renderer_lod_bake::{
    lod_level_filename, lod_manifest_filename, plan_lod_levels, MeshLodManifest,
};

/// Target triangle-count fractions of the base for each discrete level (level 0
/// is the base mesh itself, always present as `<id>.glb`).
pub const LOD_RATIOS: &[f32] = &[0.5, 0.25, 0.125];

/// Meshes below this triangle count aren't worth simplifying (bake cost with no
/// meaningful runtime win). Mirrors the per-mesh opt-out: this is the automatic
/// floor on top of the explicit toggle.
pub const LOD_MIN_TRIANGLES: usize = 512;

/// One geometry-baking result, cached by geometry hash (filenames are applied
/// per-asset by the caller, so the cache is shared across assets with identical
/// geometry).
#[derive(Clone)]
struct BakedStaticLod {
    /// `(1-based level index, glb bytes)` per emitted simplified level.
    levels: Vec<(u32, Vec<u8>)>,
    manifest: MeshLodManifest,
}

thread_local! {
    /// geometry-hash → baked levels. The editor is single-threaded (wasm), so a
    /// `thread_local` `RefCell` is the natural session cache.
    static CACHE: RefCell<HashMap<u64, BakedStaticLod>> = RefCell::new(HashMap::new());
}

/// Bake the discrete LOD chain for one static mesh and return the bundle files
/// to add (level glbs + manifest). Returns empty when the mesh is below
/// [`LOD_MIN_TRIANGLES`] or no level actually reduced the triangle count.
///
/// `asset_id` is the mesh asset's id stringified (so files line up with the
/// base `<id>.glb`); `mesh` is the already-resolved base geometry.
pub fn bake_static_lod(asset_id: &str, mesh: &MeshData) -> Vec<BundleFile> {
    if mesh.indices.len() / 3 < LOD_MIN_TRIANGLES {
        return Vec::new();
    }

    let key = geometry_key(mesh);
    let baked = CACHE.with(|c| c.borrow().get(&key).cloned());
    let baked = match baked {
        Some(b) => b,
        None => {
            let b = compute(mesh);
            CACHE.with(|c| c.borrow_mut().insert(key, b.clone()));
            b
        }
    };

    if baked.levels.is_empty() {
        return Vec::new();
    }

    let mut files = Vec::with_capacity(baked.levels.len() + 1);
    for (idx, glb) in &baked.levels {
        files.push(BundleFile::asset(
            lod_level_filename(asset_id, *idx),
            glb.clone(),
        ));
    }
    match toml::to_string(&baked.manifest) {
        Ok(s) => files.push(BundleFile::asset(
            lod_manifest_filename(asset_id),
            s.into_bytes(),
        )),
        Err(e) => {
            tracing::warn!("lod bake: failed to serialize manifest for {asset_id}: {e}");
            // Without a manifest the runtime can't discover the levels, so drop
            // the orphan level glbs rather than ship dead weight.
            return Vec::new();
        }
    }
    files
}

/// Plan the chain (shared crate), then gather attributes + encode each level to
/// a glb. Pure (no cache / no filenames) so it's trivially cacheable.
fn compute(mesh: &MeshData) -> BakedStaticLod {
    let plan = plan_lod_levels(&mesh.positions, &mesh.indices, LOD_RATIOS, LOD_MIN_TRIANGLES);
    let manifest = plan.manifest();
    let levels = plan
        .levels
        .iter()
        .map(|lvl| {
            let sm = &lvl.mesh;
            let level_mesh = MeshData {
                positions: sm.gather(&mesh.positions),
                normals: mesh.normals.as_ref().map(|n| sm.gather(n)),
                uvs: mesh.uvs.iter().map(|set| sm.gather(set)).collect(),
                colors: mesh.colors.as_ref().map(|c| sm.gather(c)),
                indices: sm.indices.clone(),
            };
            let glb = write_glb(&GlbScene {
                nodes: vec![ExportNode::new("mesh").with_mesh(level_mesh)],
                ..Default::default()
            });
            (lvl.index, glb)
        })
        .collect();
    BakedStaticLod { levels, manifest }
}

/// Stable hash over the exact inputs that determine the bake: positions,
/// indices, and the ratio schedule. Float bits are hashed directly (the bake is
/// deterministic in the literal geometry, not an epsilon-compare).
fn geometry_key(mesh: &MeshData) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    mesh.indices.hash(&mut h);
    for p in &mesh.positions {
        for v in p {
            v.to_bits().hash(&mut h);
        }
    }
    for r in LOD_RATIOS {
        r.to_bits().hash(&mut h);
    }
    h.finish()
}

// The bake *decisions* (floor, level filtering, manifest, monotonicity) are
// unit-tested in `awsm-renderer-lod-bake`'s `plan` module — this editor module
// is only attribute gather + glb encode + filename + caching, and the editor
// crate has no native lib target to host tests anyway. End-to-end coverage is
// the player-bundle export self-verify.
