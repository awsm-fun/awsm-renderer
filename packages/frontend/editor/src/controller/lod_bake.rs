//! Export-time discrete-LOD bake.
//!
//! Called from [`bake_player_bundle`](super::export::bake_player_bundle), once
//! per LOD-enabled mesh, routed by class:
//! - **static** ([`bake_static_lod`]): from the resolved `MeshData`
//!   (`mesh_cache::get_raw`).
//! - **skinned / morph** ([`bake_skinned_lod`]): from the clean rig glb
//!   (`skinned_bake_cache::get_rig_glb`) — the mesh is simplified while its skin
//!   `JOINTS_0`/`WEIGHTS_0` and morph-target deltas are carried through to the
//!   surviving vertices verbatim (the simplifier's subset property makes this
//!   exact, no interpolation), and the skeleton + skin binding are preserved.
//!
//! Both write `<id>.lod{N}.glb` per level + a [`MeshLodManifest`] sidecar
//! (`<id>.lod.toml`), and cache by content hash so re-export doesn't re-simplify.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use awsm_renderer_editor_protocol::BundleFile;
use awsm_renderer_glb_export::{
    reexport_clean_scene, write_glb, ExportNode, ExtraPrimitive, GlbScene, MeshData, MorphTarget,
};
use awsm_renderer_lod_bake::{
    bounding_sphere_radius, lod_level_filename, lod_manifest_filename, plan_lod_levels, simplify,
    MeshLodLevel, MeshLodManifest, SimplifiedMesh, SimplifyOptions,
};

/// Target triangle-count fractions of the base for each discrete level (level 0
/// is the base mesh itself, always present as `<id>.glb`).
pub const LOD_RATIOS: &[f32] = &[0.5, 0.25, 0.125];

/// Meshes below this triangle count aren't worth simplifying (bake cost with no
/// meaningful runtime win). Mirrors the per-mesh opt-out: this is the automatic
/// floor on top of the explicit toggle.
pub const LOD_MIN_TRIANGLES: usize = 512;

/// Static meshes below this triangle count don't get a cluster-LOD DAG baked
/// (cluster LOD only pays off for dense static geometry; smaller meshes use the
/// discrete chain). Higher than [`LOD_MIN_TRIANGLES`] since clustering carries
/// more bundle weight.
pub const CLUSTER_MIN_TRIANGLES: usize = 4096;

/// Bake the cluster-LOD DAG for one static mesh and return the bundle file
/// (`<id>.clusters.bin`, a JSON `ClusterMesh`) — or empty when below the cluster
/// floor. Consumed at load only when the `virtual_geometry` feature is on.
pub fn bake_static_clusters(asset_id: &str, mesh: &MeshData) -> Vec<BundleFile> {
    if mesh.indices.len() / 3 < CLUSTER_MIN_TRIANGLES {
        return Vec::new();
    }
    let dag = awsm_renderer_lod_bake::build_cluster_dag(
        &mesh.positions,
        &mesh.indices,
        &awsm_renderer_lod_bake::DagOptions::default(),
    );
    let cm = awsm_renderer_lod_bake::ClusterMesh::from_dag(
        &dag,
        mesh.positions.clone(),
        mesh.normals.clone().unwrap_or_default(),
        mesh.uvs.first().cloned().unwrap_or_default(),
        mesh.colors.clone().unwrap_or_default(),
    );
    match serde_json::to_vec(&cm) {
        Ok(bytes) => vec![BundleFile::asset(
            awsm_renderer_lod_bake::cluster_mesh_filename(asset_id),
            bytes,
        )],
        Err(e) => {
            tracing::warn!("cluster bake: serialize failed for {asset_id}: {e}");
            Vec::new()
        }
    }
}

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
    let plan = plan_lod_levels(
        &mesh.positions,
        &mesh.indices,
        LOD_RATIOS,
        LOD_MIN_TRIANGLES,
    );
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

// ── Skinned / morph ───────────────────────────────────────────────────────────

thread_local! {
    /// rig-glb-hash → baked skinned levels (level glbs + manifest).
    static SKINNED_CACHE: RefCell<HashMap<u64, BakedStaticLod>> = RefCell::new(HashMap::new());
}

/// Bake the discrete LOD chain for one skinned/morph mesh from its clean rig glb
/// and return the bundle files (level rig glbs + manifest). Each level is a full
/// rig glb — same skeleton + skin binding as the base, but with every mesh
/// primitive simplified and its `JOINTS_0`/`WEIGHTS_0` + morph deltas gathered
/// onto the surviving vertices. Returns empty when below the floor / no level
/// reduced.
///
/// `source_id` is the skin source asset id stringified, so files line up with
/// the base rig glb `<source>.glb`.
pub fn bake_skinned_lod(source_id: &str, rig_glb: &[u8]) -> Vec<BundleFile> {
    let Some(base) = parse_rig_scene(rig_glb) else {
        return Vec::new();
    };
    let base_tris = scene_triangle_count(&base);
    if base_tris < LOD_MIN_TRIANGLES {
        return Vec::new();
    }

    let key = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        rig_glb.hash(&mut h);
        for r in LOD_RATIOS {
            r.to_bits().hash(&mut h);
        }
        h.finish()
    };
    let baked = SKINNED_CACHE.with(|c| c.borrow().get(&key).cloned());
    let baked = match baked {
        Some(b) => b,
        None => {
            let b = compute_skinned(&base, base_tris);
            SKINNED_CACHE.with(|c| c.borrow_mut().insert(key, b.clone()));
            b
        }
    };

    if baked.levels.is_empty() {
        return Vec::new();
    }
    let mut files = Vec::with_capacity(baked.levels.len() + 1);
    for (idx, glb) in &baked.levels {
        files.push(BundleFile::asset(
            lod_level_filename(source_id, *idx),
            glb.clone(),
        ));
    }
    match toml::to_string(&baked.manifest) {
        Ok(s) => files.push(BundleFile::asset(
            lod_manifest_filename(source_id),
            s.into_bytes(),
        )),
        Err(e) => {
            tracing::warn!("lod bake: failed to serialize skinned manifest for {source_id}: {e}");
            return Vec::new();
        }
    }
    files
}

/// Run the simplifier per ratio over a clone of the base rig scene, gathering
/// skin + morph through each level, and encode each reducing level to a glb.
fn compute_skinned(base: &GlbScene, base_tris: usize) -> BakedStaticLod {
    let bounds_radius = bounding_sphere_radius(&scene_positions(base));
    let mut levels = Vec::new();
    let mut manifest_levels = Vec::new();
    let mut prev_tris = base_tris;
    let mut file_index = 1u32;

    for &ratio in LOD_RATIOS {
        let mut scene = base.clone();
        let mut tris = 0usize;
        let mut error = 0.0f32;
        for node in &mut scene.nodes {
            simplify_node_tree(node, ratio, &mut tris, &mut error);
        }
        if tris == 0 || tris >= prev_tris {
            continue;
        }
        prev_tris = tris;
        let glb = write_glb(&scene);
        levels.push((file_index, glb));
        manifest_levels.push(MeshLodLevel {
            index: file_index,
            error,
            triangle_count: tris as u32,
        });
        file_index += 1;
    }

    BakedStaticLod {
        levels,
        manifest: MeshLodManifest {
            bounds_radius,
            base_triangle_count: base_tris as u32,
            levels: manifest_levels,
        },
    }
}

/// Parse rig glb bytes into the export scene (skeleton + skin + morph + meshes).
fn parse_rig_scene(rig_glb: &[u8]) -> Option<GlbScene> {
    let (doc, buffers, _images) = gltf::import_slice(rig_glb).ok()?;
    let buffers: Vec<Vec<u8>> = buffers.into_iter().map(|b| b.0).collect();
    reexport_clean_scene(&doc, &buffers)
}

/// Recursively simplify every mesh primitive in a node subtree at `ratio`,
/// accumulating the resulting triangle count + max error.
fn simplify_node_tree(node: &mut ExportNode, ratio: f32, tris: &mut usize, error: &mut f32) {
    if let Some(mesh) = node.mesh.take() {
        let (mesh, sm) = simplify_primitive(mesh, ratio);
        if let Some(sm) = &sm {
            *tris += sm.triangle_count();
            *error = error.max(sm.error);
            gather_skin_morph(
                sm,
                &mut node.joints,
                &mut node.weights,
                &mut node.morph_targets,
            );
        }
        node.mesh = Some(mesh);
    }
    for ep in &mut node.extra_primitives {
        simplify_extra_primitive(ep, ratio, tris, error);
    }
    for child in &mut node.children {
        simplify_node_tree(child, ratio, tris, error);
    }
}

fn simplify_extra_primitive(
    ep: &mut ExtraPrimitive,
    ratio: f32,
    tris: &mut usize,
    error: &mut f32,
) {
    let mesh = std::mem::take(&mut ep.mesh);
    let (mesh, sm) = simplify_primitive(mesh, ratio);
    if let Some(sm) = &sm {
        *tris += sm.triangle_count();
        *error = error.max(sm.error);
        gather_skin_morph(sm, &mut ep.joints, &mut ep.weights, &mut ep.morph_targets);
    }
    ep.mesh = mesh;
}

/// Simplify one primitive's geometry to `ratio` and gather its vertex attributes
/// onto the survivors. Returns the rewritten mesh and the remap (`None` if the
/// primitive has no triangles or didn't need simplifying).
fn simplify_primitive(mut mesh: MeshData, ratio: f32) -> (MeshData, Option<SimplifiedMesh>) {
    let base = mesh.indices.len() / 3;
    if base == 0 {
        return (mesh, None);
    }
    let target = ((base as f32 * ratio).round() as usize).max(1);
    let sm = simplify(
        &mesh.positions,
        &mesh.indices,
        SimplifyOptions::with_target(target),
    );
    // Gather geometry attributes (read originals first, then overwrite).
    let positions = sm.gather(&mesh.positions);
    let normals = mesh.normals.as_ref().map(|n| sm.gather(n));
    let uvs = mesh.uvs.iter().map(|set| sm.gather(set)).collect();
    let colors = mesh.colors.as_ref().map(|c| sm.gather(c));
    mesh.positions = positions;
    mesh.normals = normals;
    mesh.uvs = uvs;
    mesh.colors = colors;
    mesh.indices = sm.indices.clone();
    (mesh, Some(sm))
}

/// Carry per-vertex skin (`JOINTS_0`/`WEIGHTS_0`) and morph-target deltas through
/// the surviving-vertex remap. Exact (subset gather, no interpolation).
fn gather_skin_morph(
    sm: &SimplifiedMesh,
    joints: &mut Option<Vec<[u16; 4]>>,
    weights: &mut Option<Vec<[f32; 4]>>,
    morph_targets: &mut [MorphTarget],
) {
    if let Some(j) = joints.as_ref() {
        *joints = Some(sm.gather(j));
    }
    if let Some(w) = weights.as_ref() {
        *weights = Some(sm.gather(w));
    }
    for t in morph_targets.iter_mut() {
        t.positions = sm.gather(&t.positions);
        if let Some(n) = t.normals.as_ref() {
            t.normals = Some(sm.gather(n));
        }
    }
}

/// Total triangles across all mesh primitives in the scene (main + extra).
fn scene_triangle_count(scene: &GlbScene) -> usize {
    fn node_tris(node: &ExportNode) -> usize {
        let mut n = node.mesh.as_ref().map_or(0, |m| m.indices.len() / 3);
        for ep in &node.extra_primitives {
            n += ep.mesh.indices.len() / 3;
        }
        for c in &node.children {
            n += node_tris(c);
        }
        n
    }
    scene.nodes.iter().map(node_tris).sum()
}

/// All mesh positions across the scene (for the bounding-sphere radius).
fn scene_positions(scene: &GlbScene) -> Vec<[f32; 3]> {
    fn collect(node: &ExportNode, out: &mut Vec<[f32; 3]>) {
        if let Some(m) = &node.mesh {
            out.extend_from_slice(&m.positions);
        }
        for ep in &node.extra_primitives {
            out.extend_from_slice(&ep.mesh.positions);
        }
        for c in &node.children {
            collect(c, out);
        }
    }
    let mut out = Vec::new();
    for n in &scene.nodes {
        collect(n, &mut out);
    }
    out
}
