//! Phase 0.1 of the save/load roundtrip plan (docs/plans/save-load-roundtrip.md):
//! a NO-BROWSER oracle that pins where imported geometry / texture BYTES drop.
//!
//! It loads a fixture glb, runs the SAME CPU extraction the editor's persistence
//! path uses (`extract_node_mesh` + `extract_texture_images`), and asserts every
//! mesh-bearing node yields non-empty geometry and every glTF image yields encoded
//! bytes. If this passes synchronously but the editor still drops data, the bug is
//! the editor feeding extraction INCOMPLETE buffers (async load), not the extraction
//! logic — which is the question this test answers.
//!
//! Fixtures are not committed (a 26 MB robot). Point `SAVELOAD_FIXTURE` at a glb to
//! run; the test self-skips when unset so CI/other devs aren't broken.

use awsm_renderer_glb_export::{extract_node_mesh, extract_texture_images};

fn load(path: &str) -> (gltf::Document, Vec<Vec<u8>>) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let (doc, buffers, _images) =
        gltf::import_slice(&bytes).unwrap_or_else(|e| panic!("parse {path}: {e}"));
    let raw: Vec<Vec<u8>> = buffers.into_iter().map(|b| b.0).collect();
    (doc, raw)
}

struct Census {
    mesh_nodes: usize,
    mesh_ok: usize,
    /// mesh nodes whose source primitives all carry a TANGENT accessor.
    mesh_with_authored_tangent: usize,
    /// of those, how many extraction actually captured (`ExtractedNodeMesh.tangents`).
    tangents_captured: usize,
    images: usize,
    images_ok: usize,
}

/// Census of what extraction recovers from a fully-loaded glb.
fn census(doc: &gltf::Document, buffers: &[Vec<u8>]) -> Census {
    let mesh_nodes: Vec<u32> = doc
        .nodes()
        .filter(|n| n.mesh().is_some())
        .map(|n| n.index() as u32)
        .collect();
    let mut mesh_ok = 0;
    let mut with_authored = 0;
    let mut captured = 0;
    for &ni in &mesh_nodes {
        // Does every primitive on this node author a TANGENT? (extraction keeps the
        // channel only when all do — matches the merge's all-or-nothing rule.)
        let node = doc.nodes().find(|n| n.index() == ni as usize).unwrap();
        let all_tan = node
            .mesh()
            .unwrap()
            .primitives()
            .all(|p| p.get(&gltf::Semantic::Tangents).is_some());
        if all_tan {
            with_authored += 1;
        }
        if let Some(em) = extract_node_mesh(doc, buffers, ni, None) {
            if !em.mesh.positions.is_empty() && !em.mesh.indices.is_empty() {
                mesh_ok += 1;
            }
            if all_tan
                && em
                    .tangents
                    .as_ref()
                    .is_some_and(|t| t.len() == em.mesh.positions.len())
            {
                captured += 1;
            }
        }
    }
    let imgs = extract_texture_images(doc, buffers);
    Census {
        mesh_nodes: mesh_nodes.len(),
        mesh_ok,
        mesh_with_authored_tangent: with_authored,
        tangents_captured: captured,
        images: doc.images().count(),
        images_ok: imgs.values().filter(|im| !im.bytes.is_empty()).count(),
    }
}

#[test]
fn fixture_extraction_is_lossless() {
    let Ok(path) = std::env::var("SAVELOAD_FIXTURE") else {
        eprintln!("SAVELOAD_FIXTURE unset — skipping (set it to a glb to run)");
        return;
    };
    let (doc, buffers) = load(&path);
    let c = census(&doc, &buffers);
    println!(
        "extraction census [{path}]: mesh_nodes={} non_empty={} images={} with_bytes={} \
         authored_tangent_nodes={} tangents_captured={}",
        c.mesh_nodes,
        c.mesh_ok,
        c.images,
        c.images_ok,
        c.mesh_with_authored_tangent,
        c.tangents_captured,
    );
    assert_eq!(
        c.mesh_ok, c.mesh_nodes,
        "every mesh-bearing node must extract non-empty geometry ({}/{})",
        c.mesh_ok, c.mesh_nodes
    );
    assert_eq!(
        c.images_ok, c.images,
        "every glTF image must extract encoded bytes ({}/{})",
        c.images_ok, c.images
    );
    // P0-C: authored tangents must survive extraction (else the captured mesh
    // regenerates them on reload → dark-patch roundtrip).
    assert_eq!(
        c.tangents_captured, c.mesh_with_authored_tangent,
        "every node that authors TANGENT must keep it through extraction ({}/{})",
        c.tangents_captured, c.mesh_with_authored_tangent
    );
}
