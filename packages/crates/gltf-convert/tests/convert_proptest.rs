//! Property tests for the pure-data convert pipeline (no GPU/browser).
//!
//! The two invariants that make the mesh-authoring round-trip trustworthy:
//!   1. converting arbitrary geometry yields a canonical glb that preserves the
//!      geometry and is stamped `AWSM_format`;
//!   2. convert is idempotent — re-converting our own output passes the exact
//!      bytes through (`convert(convert(x)) == convert(x)`).
//!
//! `proptest` varies vertex count, which attribute channels are present, and the
//! index topology, so the matrix is swept rather than hand-enumerated.

use awsm_glb_export::{extract_node_mesh_from_bytes, write_glb, ExportNode, GlbScene, MeshData};
use awsm_gltf_convert::{awsm_format_version, convert, is_canonical, AWSM_FORMAT_VERSION};
use proptest::prelude::*;

fn mesh_data_strategy() -> impl Strategy<Value = MeshData> {
    (3usize..30usize).prop_flat_map(|vcount| {
        let positions = prop::collection::vec(prop::array::uniform3(-1000.0f32..1000.0), vcount);
        let normals =
            prop::option::of(prop::collection::vec(prop::array::uniform3(-1.0f32..1.0), vcount));
        let uvs =
            prop::option::of(prop::collection::vec(prop::array::uniform2(0.0f32..8.0), vcount));
        let colors =
            prop::option::of(prop::collection::vec(prop::array::uniform4(0.0f32..1.0), vcount));
        let indices =
            prop::collection::vec(0u32..(vcount as u32), 3..=60).prop_map(|mut v| {
                let keep = (v.len() / 3) * 3;
                v.truncate(keep.max(3));
                v
            });
        (positions, normals, uvs, colors, indices).prop_map(
            |(positions, normals, uvs, colors, indices)| MeshData {
                positions,
                normals,
                uvs,
                colors,
                indices,
            },
        )
    })
}

fn glb_of(md: &MeshData) -> Vec<u8> {
    write_glb(&GlbScene {
        nodes: vec![ExportNode::new("m").with_mesh(md.clone())],
        ..Default::default()
    })
}

proptest! {
    /// Foreign geometry → canonical glb: geometry preserved, stamped AWSM_format.
    #[test]
    fn convert_preserves_geometry_and_stamps(md in mesh_data_strategy()) {
        let source = glb_of(&md);
        let out = convert(&source).expect("convert");
        prop_assert!(!out.is_already_canonical);

        let got = extract_node_mesh_from_bytes(&out.glb, 0, None)
            .expect("canonical glb yields geometry");
        prop_assert_eq!(got.positions.len(), md.positions.len());
        prop_assert_eq!(&got.indices, &md.indices);

        let (doc, _, _) = gltf::import_slice(&out.glb).expect("reparse");
        prop_assert!(is_canonical(&doc));
        prop_assert_eq!(awsm_format_version(&doc), Some(AWSM_FORMAT_VERSION));
    }

    /// Idempotency: a second convert detects the marker and passes through the
    /// exact same bytes.
    #[test]
    fn convert_is_idempotent(md in mesh_data_strategy()) {
        let once = convert(&glb_of(&md)).expect("convert 1");
        let twice = convert(&once.glb).expect("convert 2");
        prop_assert!(twice.is_already_canonical);
        prop_assert_eq!(&twice.glb, &once.glb);
    }
}
