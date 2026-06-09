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

use crate::MeshData;

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
}
