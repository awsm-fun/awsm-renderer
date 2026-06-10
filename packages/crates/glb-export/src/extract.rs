//! Read geometry back **out** of a source glTF/GLB (the reverse of [`write_glb`]).
//!
//! The editor's Model nodes reference one node inside an imported glTF file (by
//! node index, optionally one primitive of it). Exporting such a node means
//! re-reading that node's mesh from the original file and lowering its accessors
//! into a plain [`MeshData`] — the same plain-data shape every other geometry kind
//! bakes to before [`write_glb`]. This is pure (no GPU / wasm), so it is natively
//! unit-testable.
//!
//! ## Transforms (do NOT double-transform)
//!
//! The returned geometry is the **raw accessor positions** in the glTF node's own
//! local space — no node matrix is applied. The editor mirrors each glTF node's
//! local transform onto the corresponding editor node, and the exporter writes
//! that transform onto the `ExportNode`, which places the geometry. Applying the
//! node's transform here too would double-transform it.

use std::collections::HashMap;

use crate::{ExportNode, ExportSkin, GlbScene, MeshData, MorphTarget, Trs};

/// Re-export a source glTF/GLB into a **clean** [`GlbScene`] — geometry + skin rig
/// (skeleton, joints, inverse-bind, per-vertex JOINTS/WEIGHTS) + morph targets,
/// with **materials, animations, cameras, lights, and images dropped**. Feed the
/// result to [`write_glb`](crate::write_glb) to produce the bundle's clean
/// `assets/<id>.glb` (the "re-export everything through our writer" path: uniform
/// encoding, no source-material/animation cruft, no orphaned accessors).
///
/// Node hierarchy + transforms are preserved (so the skin's joint refs + our
/// clips' joint-node targets stay valid). Returns `None` if the bytes don't parse
/// or carry no default/first scene.
pub fn reexport_clean(bytes: &[u8]) -> Option<GlbScene> {
    let (doc, buffers, _images) = gltf::import_slice(bytes).ok()?;
    let buffers: Vec<Vec<u8>> = buffers.into_iter().map(|b| b.0).collect();
    reexport_clean_scene(&doc, &buffers)
}

/// Map each source glTF node index → its index in the **clean re-export**: the
/// depth-first (root-first, children in order) flatten over the default scene,
/// exactly the order [`reexport_clean_scene`] builds [`GlbScene::nodes`] and
/// [`write_glb`](crate::write_glb) assigns glTF node indices. Nodes outside the
/// default scene are absent.
///
/// Use this to translate a source joint node index into the index the player's
/// loader will see after it loads the re-exported `assets/<id>.glb` — the basis
/// for binding our animation clips' bone targets to the rig's baked joints.
pub fn scene_node_flat_indices(doc: &gltf::Document) -> HashMap<usize, usize> {
    let mut flat_of: HashMap<usize, usize> = HashMap::new();
    let Some(scene) = doc.default_scene().or_else(|| doc.scenes().next()) else {
        return flat_of;
    };
    fn index_walk(node: &gltf::Node, flat_of: &mut HashMap<usize, usize>, next: &mut usize) {
        flat_of.insert(node.index(), *next);
        *next += 1;
        for c in node.children() {
            index_walk(&c, flat_of, next);
        }
    }
    let mut next = 0usize;
    for r in scene.nodes() {
        index_walk(&r, &mut flat_of, &mut next);
    }
    flat_of
}

/// Like [`reexport_clean`] but operating on an already-parsed
/// [`gltf::Document`] + its raw buffer blobs — so a caller that already decoded
/// the source (e.g. the editor's import, which holds the doc before it's consumed
/// by the renderer) can build the clean rig without re-parsing bytes.
pub fn reexport_clean_scene(doc: &gltf::Document, buffers: &[Vec<u8>]) -> Option<GlbScene> {
    let scene = doc.default_scene().or_else(|| doc.scenes().next())?;

    // glTF node index → flat (depth-first) index, matching `write_glb`'s flatten,
    // so skin joint refs (glTF node indices) become our flat indices.
    let flat_of = scene_node_flat_indices(doc);

    let nodes: Vec<ExportNode> = scene
        .nodes()
        .map(|r| build_clean_node(&r, buffers))
        .collect();

    let skins: Vec<ExportSkin> = doc
        .skins()
        .map(|skin| {
            let joints: Vec<usize> = skin
                .joints()
                .filter_map(|j| flat_of.get(&j.index()).copied())
                .collect();
            let reader = skin.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
            let inverse_bind_matrices: Vec<[f32; 16]> = reader
                .read_inverse_bind_matrices()
                .map(|it| {
                    it.map(|m| {
                        // glTF/our IBM are both column-major 4x4; flatten cols.
                        let mut out = [0.0f32; 16];
                        for (c, col) in m.iter().enumerate() {
                            out[c * 4..c * 4 + 4].copy_from_slice(col);
                        }
                        out
                    })
                    .collect()
                })
                .unwrap_or_default();
            let skeleton = skin
                .skeleton()
                .and_then(|n| flat_of.get(&n.index()).copied());
            ExportSkin {
                joints,
                inverse_bind_matrices,
                skeleton,
            }
        })
        .collect();

    Some(GlbScene {
        nodes,
        skins,
        ..Default::default()
    })
}

/// One node → a clean `ExportNode` (geometry + skin attrs + morph; no material).
fn build_clean_node(node: &gltf::Node, buffers: &[Vec<u8>]) -> ExportNode {
    let (translation, rotation, scale) = node.transform().decomposed();
    let mut out = ExportNode {
        name: node.name().unwrap_or("").to_string(),
        transform: Trs {
            translation,
            rotation,
            scale,
        },
        skin: node.skin().map(|s| s.index()),
        ..Default::default()
    };

    if let Some(mesh) = node.mesh() {
        let mut positions: Vec<[f32; 3]> = Vec::new();
        let mut normals: Vec<[f32; 3]> = Vec::new();
        let mut uvs: Vec<[f32; 2]> = Vec::new();
        let mut colors: Vec<[f32; 4]> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let mut joints: Vec<[u16; 4]> = Vec::new();
        let mut weights: Vec<[f32; 4]> = Vec::new();
        // Per-target accumulated deltas (positions/normals), one Vec per target.
        let mut morph_pos: Vec<Vec<[f32; 3]>> = Vec::new();
        let mut morph_nrm: Vec<Vec<[f32; 3]>> = Vec::new();
        let (mut all_n, mut all_uv, mut all_c, mut all_jw) = (true, true, true, true);
        let mut any = false;

        for primitive in mesh.primitives() {
            let reader = primitive.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
            let prim_pos: Vec<[f32; 3]> = match reader.read_positions() {
                Some(p) => p.collect(),
                None => continue,
            };
            let base = positions.len() as u32;
            let vcount = prim_pos.len();
            positions.extend(prim_pos);
            any = true;
            match reader.read_normals() {
                Some(n) => normals.extend(n),
                None => all_n = false,
            }
            match reader.read_tex_coords(0) {
                Some(t) => uvs.extend(t.into_f32()),
                None => all_uv = false,
            }
            match reader.read_colors(0) {
                Some(c) => colors.extend(c.into_rgba_f32()),
                None => all_c = false,
            }
            match (reader.read_joints(0), reader.read_weights(0)) {
                (Some(j), Some(w)) => {
                    joints.extend(j.into_u16());
                    weights.extend(w.into_f32());
                }
                _ => all_jw = false,
            }
            match reader.read_indices() {
                Some(idx) => indices.extend(idx.into_u32().map(|x| x + base)),
                None => indices.extend((0..vcount as u32).map(|x| x + base)),
            }
            // Morph targets: concatenate each target's deltas across primitives.
            let targets = reader.read_morph_targets();
            for (ti, (tp, tn, _tt)) in targets.enumerate() {
                if morph_pos.len() <= ti {
                    morph_pos.push(Vec::new());
                    morph_nrm.push(Vec::new());
                }
                match tp {
                    Some(p) => morph_pos[ti].extend(p),
                    None => morph_pos[ti].extend(std::iter::repeat_n([0.0; 3], vcount)),
                }
                match tn {
                    Some(n) => morph_nrm[ti].extend(n),
                    None => morph_nrm[ti].extend(std::iter::repeat_n([0.0; 3], vcount)),
                }
            }
        }

        if any {
            out.mesh = Some(MeshData {
                positions,
                normals: all_n.then_some(normals),
                uvs: all_uv.then_some(uvs),
                colors: all_c.then_some(colors),
                indices,
            });
            if all_jw && !joints.is_empty() {
                out.joints = Some(joints);
                out.weights = Some(weights);
            }
            out.morph_targets = morph_pos
                .into_iter()
                .zip(morph_nrm)
                .map(|(pos, nrm)| MorphTarget {
                    // Names are cosmetic + the gltf reader's `extras` feature is
                    // off, so target order (which our clips index by) is what we
                    // preserve; names can be carried later if needed.
                    name: None,
                    positions: pos,
                    normals: if nrm.iter().any(|d| *d != [0.0; 3]) {
                        Some(nrm)
                    } else {
                        None
                    },
                })
                .collect();
            out.morph_weights = mesh.weights().map(|w| w.to_vec()).unwrap_or_default();
        }
    }

    out.children = node
        .children()
        .map(|c| build_clean_node(&c, buffers))
        .collect();
    out
}

/// Read the geometry of a single glTF node out of an already-loaded
/// [`gltf::Document`] + its buffer blobs, into a plain [`MeshData`].
///
/// - `node_index` selects the glTF node; its mesh is read.
/// - `primitive_index`: `Some(i)` reads only that one primitive; `None` merges
///   every primitive on the node into one mesh (concatenating vertices and
///   offsetting each primitive's indices).
///
/// Returns `None` when the node index is out of range, the node has no mesh, the
/// requested primitive index is out of range, or a primitive carries no positions.
/// Missing normals/uvs/colors are simply left empty/`None` (the writer recomputes
/// or omits them) — mirroring how the renderer-bridge tolerates partial meshes.
///
/// Positions/normals/uvs are the **raw** accessor values (the node's own local
/// space); see the module docs on why no transform is applied.
pub fn extract_node_mesh(
    doc: &gltf::Document,
    buffers: &[Vec<u8>],
    node_index: u32,
    primitive_index: Option<u32>,
) -> Option<MeshData> {
    let node = doc.nodes().find(|n| n.index() == node_index as usize)?;
    let mesh = node.mesh()?;

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    // Track whether *every* read primitive supplied normals/uvs/colors; if any
    // didn't, the channel is dropped wholesale (a partial channel would misalign
    // with positions). The writer fills the gaps (recompute normals / omit uvs).
    let mut any_primitive = false;
    let mut all_have_normals = true;
    let mut all_have_uvs = true;
    let mut all_have_colors = true;

    for (i, primitive) in mesh.primitives().enumerate() {
        if let Some(want) = primitive_index {
            if i as u32 != want {
                continue;
            }
        }
        let reader = primitive.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
        let prim_positions: Vec<[f32; 3]> = match reader.read_positions() {
            Some(p) => p.collect(),
            None => continue, // a primitive with no positions can't contribute.
        };
        let base = positions.len() as u32;
        let vert_count = prim_positions.len();
        positions.extend(prim_positions);
        any_primitive = true;

        match reader.read_normals() {
            Some(n) => normals.extend(n),
            None => all_have_normals = false,
        }
        match reader.read_tex_coords(0) {
            Some(t) => uvs.extend(t.into_f32()),
            None => all_have_uvs = false,
        }
        match reader.read_colors(0) {
            Some(c) => colors.extend(c.into_rgba_f32()),
            None => all_have_colors = false,
        }

        match reader.read_indices() {
            Some(idx) => indices.extend(idx.into_u32().map(|x| x + base)),
            // Non-indexed primitive: emit a trivial 0..n index run (offset by base).
            None => indices.extend((0..vert_count as u32).map(|x| x + base)),
        }
    }

    if !any_primitive {
        return None;
    }

    Some(MeshData {
        positions,
        normals: all_have_normals.then_some(normals),
        uvs: all_have_uvs.then_some(uvs),
        colors: all_have_colors.then_some(colors),
        indices,
    })
}

/// Parse glTF/GLB bytes and extract one node's geometry in a single call — the
/// editor's export path uses this on the cached source bytes of an imported model.
///
/// Returns `None` if the bytes don't parse or the node has no extractable mesh.
/// Only self-contained sources resolve here: `.glb` and `.gltf` with embedded /
/// data-URI buffers (no external `.bin` side-files), which is what the editor
/// caches at import time.
pub fn extract_node_mesh_from_bytes(
    bytes: &[u8],
    node_index: u32,
    primitive_index: Option<u32>,
) -> Option<MeshData> {
    let (doc, buffers, _images) = gltf::import_slice(bytes).ok()?;
    let buffers: Vec<Vec<u8>> = buffers.into_iter().map(|b| b.0).collect();
    extract_node_mesh(&doc, &buffers, node_index, primitive_index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{write_glb, ExportNode, GlbScene, Trs};
    use awsm_meshgen::box_mesh;
    use glam::Vec3;

    /// Round-trip: write a 2-node GLB (a parent transform + a child cube mesh),
    /// re-read its bytes, extract the child node's geometry, and assert the
    /// vertex/index counts match the source cube.
    #[test]
    fn extract_child_node_mesh_roundtrip() {
        let src = box_mesh(Vec3::splat(2.0));
        let child = ExportNode::new("Cube").with_mesh(src.clone());
        let mut parent = ExportNode::new("Parent");
        parent.transform = Trs::IDENTITY;
        parent.children = vec![child];
        let scene = GlbScene {
            nodes: vec![parent],
            ..Default::default()
        };
        let glb = write_glb(&scene);

        // write_glb flattens depth-first: node 0 = Parent (no mesh), node 1 = Cube.
        let mesh = extract_node_mesh_from_bytes(&glb, 1, None).expect("child node mesh");
        assert_eq!(mesh.positions.len(), src.positions.len());
        assert_eq!(mesh.indices.len(), src.indices.len());

        // Node 0 has no mesh ⇒ None.
        assert!(extract_node_mesh_from_bytes(&glb, 0, None).is_none());
        // Out-of-range node ⇒ None.
        assert!(extract_node_mesh_from_bytes(&glb, 99, None).is_none());
    }

    /// Merging multiple primitives concatenates vertices and offsets indices, so
    /// no index references another primitive's vertex range.
    #[test]
    fn merge_primitives_offsets_indices() {
        // Two mesh nodes written separately, then re-read; the writer emits one
        // primitive per node, so to exercise multi-primitive merge we instead read
        // each node and confirm a single-primitive node merges identically.
        let a = box_mesh(Vec3::splat(1.0));
        let node = ExportNode::new("A").with_mesh(a.clone());
        let glb = write_glb(&GlbScene {
            nodes: vec![node],
            ..Default::default()
        });
        // primitive_index None and Some(0) yield the same single primitive.
        let all = extract_node_mesh_from_bytes(&glb, 0, None).unwrap();
        let one = extract_node_mesh_from_bytes(&glb, 0, Some(0)).unwrap();
        assert_eq!(all.positions.len(), one.positions.len());
        assert_eq!(all.indices, one.indices);
        // Every index is in range.
        assert!(all
            .indices
            .iter()
            .all(|&i| (i as usize) < all.positions.len()));
        // Out-of-range primitive ⇒ None.
        assert!(extract_node_mesh_from_bytes(&glb, 0, Some(9)).is_none());
    }

    /// The "re-export everything" core: a skinned + morphed scene survives
    /// write → `reexport_clean` → write → re-parse with skin/morph intact, and
    /// materials are dropped.
    #[test]
    fn reexport_clean_preserves_skin_and_morph() {
        use crate::{ExportMaterial, ExportSkin, MorphTarget, PbrMaterial};

        let tri = MeshData {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: Some(vec![[0.0, 0.0, 1.0]; 3]),
            uvs: None,
            colors: None,
            indices: vec![0, 1, 2],
        };
        let ident = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let src = GlbScene {
            // Armature(0) → J0(1), J1(2); skinned Mesh(3) with a material (dropped).
            nodes: vec![
                ExportNode {
                    name: "Armature".into(),
                    children: vec![ExportNode::new("J0"), ExportNode::new("J1")],
                    ..Default::default()
                },
                ExportNode {
                    name: "Mesh".into(),
                    mesh: Some(tri),
                    material: Some(ExportMaterial::Pbr(PbrMaterial::default())),
                    skin: Some(0),
                    joints: Some(vec![[0, 1, 0, 0]; 3]),
                    weights: Some(vec![[0.5, 0.5, 0.0, 0.0]; 3]),
                    morph_targets: vec![MorphTarget {
                        name: None,
                        positions: vec![[0.0, 0.2, 0.0]; 3],
                        normals: None,
                    }],
                    morph_weights: vec![0.0],
                    ..Default::default()
                },
            ],
            skins: vec![ExportSkin {
                joints: vec![1, 2],
                inverse_bind_matrices: vec![ident, ident],
                skeleton: Some(0),
            }],
            ..Default::default()
        };
        let glb = write_glb(&src);

        // Re-export clean, write again, re-parse: the rig survives, material gone.
        let clean = reexport_clean(&glb).expect("reexport");
        assert_eq!(clean.skins.len(), 1);
        assert_eq!(clean.skins[0].joints, vec![1, 2]);
        let glb2 = crate::write_glb(&clean);
        let (doc, buffers, _i) = gltf::import_slice(&glb2).expect("re-parse cleaned");
        assert_eq!(doc.skins().count(), 1, "skin preserved");
        assert_eq!(doc.materials().count(), 0, "materials dropped");
        let prim = doc.meshes().next().unwrap().primitives().next().unwrap();
        let r = prim.reader(|b| Some(&buffers[b.index()]));
        assert_eq!(r.read_joints(0).expect("joints").into_u16().count(), 3);
        assert_eq!(prim.morph_targets().count(), 1, "morph preserved");
        // The skinned node still binds the skin.
        assert!(doc.nodes().any(|n| n.skin().is_some()));
    }

    /// `reexport_clean` PRESERVES each node's local transform (it does not bake
    /// or strip them). This is the invariant `awsm-scene-loader` relies on: a
    /// skinned rig glb carries the original glTF's root basis-conversion node
    /// (e.g. RiggedSimple's `Z_UP`), so the rig glb is self-placing and the
    /// loader roots it at the renderer root rather than re-applying a scene
    /// transform — otherwise the root rotation double-applies. If a future change
    /// makes reexport flatten/bake transforms, this fails and the loader's
    /// SkinnedMesh placement must be revisited.
    #[test]
    fn reexport_clean_preserves_node_transforms() {
        // A non-identity root transform, like the Z-up→Y-up `Z_UP` node.
        let rot = glam::Quat::from_rotation_x(std::f32::consts::FRAC_PI_2).to_array();
        let mut parent = ExportNode::new("Z_UP");
        parent.transform = Trs {
            translation: [1.0, 2.0, 3.0],
            rotation: rot,
            scale: [1.0, 1.0, 1.0],
        };
        parent.children = vec![ExportNode::new("Cube").with_mesh(box_mesh(Vec3::splat(1.0)))];
        let glb = write_glb(&GlbScene {
            nodes: vec![parent],
            ..Default::default()
        });

        let clean = reexport_clean(&glb).expect("reexport");
        assert_eq!(clean.nodes.len(), 1, "single root preserved");
        let t = &clean.nodes[0].transform;
        for (got, want) in t.translation.iter().zip([1.0, 2.0, 3.0].iter()) {
            assert!((got - want).abs() < 1e-5, "translation {:?}", t.translation);
        }
        for (got, want) in t.rotation.iter().zip(rot.iter()) {
            assert!(
                (got - want).abs() < 1e-5,
                "rotation {:?} vs {:?}",
                t.rotation,
                rot
            );
        }
        assert_eq!(
            clean.nodes[0].children.len(),
            1,
            "child hierarchy preserved"
        );
    }
}
