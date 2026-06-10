//! Property-based round-trip tests for the glTF/GLB geometry pipeline.
//!
//! These guard the *data* leg of the mesh-authoring round-trip: an authored
//! `MeshData` written to a `.glb` and read back must reproduce the same geometry
//! — every attribute channel (positions / normals / uvs / colors), present or
//! absent, and the index buffer. The writer stores all vertex attributes as
//! non-normalized `F32` (and indices as integers), so a faithful round-trip is
//! *bit-exact* — we assert exact equality, not a tolerance.
//!
//! `proptest` varies vertex count, which channels are present, and the index
//! topology, so the matrix of "has normals? has uvs? has vertex colors?" is
//! swept automatically rather than hand-enumerated.
//!
//! NOTE: this is the serialization/fidelity net. It does NOT exercise the
//! renderer's GPU-buffer construction (visibility-vs-transparency geometry kind,
//! tangent generation) — that divergence is covered by a separate buffer-parity
//! test, because those concerns live below the data layer (a tangent-less mesh
//! round-trips as tangent-less, which is "correct" data yet still needs tangents
//! generated at render time).

use awsm_glb_export::{extract_node_mesh_from_bytes, write_glb, ExportNode, GlbScene, MeshData};
use proptest::prelude::*;

/// A finite-valued `MeshData` with a randomized mix of present/absent attribute
/// channels and a valid (in-range, multiple-of-three) index buffer.
fn mesh_data_strategy() -> impl Strategy<Value = MeshData> {
    (3usize..30usize).prop_flat_map(|vcount| {
        // Bounded ranges keep values finite (proptest's bare f32 strategy emits
        // NaN/inf, which don't compare equal to themselves and aren't meaningful
        // geometry). Positions span a model-ish range; the rest their natural ones.
        let positions = prop::collection::vec(prop::array::uniform3(-1000.0f32..1000.0), vcount);
        let normals =
            prop::option::of(prop::collection::vec(prop::array::uniform3(-1.0f32..1.0), vcount));
        let uvs =
            prop::option::of(prop::collection::vec(prop::array::uniform2(0.0f32..8.0), vcount));
        let colors =
            prop::option::of(prop::collection::vec(prop::array::uniform4(0.0f32..1.0), vcount));
        // 1..=20 triangles; every index references an existing vertex.
        let indices = prop::collection::vec(0u32..(vcount as u32), 3..=60)
            .prop_map(|mut v| {
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

proptest! {
    /// MeshData → write_glb → extract_node_mesh_from_bytes reproduces every
    /// channel bit-exactly, and preserves channel presence (a `None` channel
    /// stays `None`; a `Some` channel comes back `Some` with identical values).
    #[test]
    fn geometry_roundtrips_bit_exact(md in mesh_data_strategy()) {
        let node = ExportNode::new("m").with_mesh(md.clone());
        let scene = GlbScene { nodes: vec![node], ..Default::default() };
        let bytes = write_glb(&scene);

        // Single root mesh node ⇒ flatten index 0.
        let got = extract_node_mesh_from_bytes(&bytes, 0, None)
            .expect("round-tripped glb must yield the mesh back");

        prop_assert_eq!(&got.positions, &md.positions, "positions diverged");
        prop_assert_eq!(&got.normals, &md.normals, "normals channel diverged");
        prop_assert_eq!(&got.uvs, &md.uvs, "uv channel diverged");
        prop_assert_eq!(&got.colors, &md.colors, "vertex-color channel diverged");
        prop_assert_eq!(&got.indices, &md.indices, "index buffer diverged");
    }
}
