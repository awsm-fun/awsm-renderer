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

use awsm_glb_export::{
    extract_node_mesh_from_bytes, reexport_clean, write_glb, ExportNode, ExportSkin, GlbScene,
    MeshData, MorphTarget,
};
use proptest::prelude::*;

/// A finite-valued `MeshData` with a randomized mix of present/absent attribute
/// channels and a valid (in-range, multiple-of-three) index buffer.
fn mesh_data_strategy() -> impl Strategy<Value = MeshData> {
    (3usize..30usize).prop_flat_map(|vcount| {
        // Bounded ranges keep values finite (proptest's bare f32 strategy emits
        // NaN/inf, which don't compare equal to themselves and aren't meaningful
        // geometry). Positions span a model-ish range; the rest their natural ones.
        let positions = prop::collection::vec(prop::array::uniform3(-1000.0f32..1000.0), vcount);
        let normals = prop::option::of(prop::collection::vec(
            prop::array::uniform3(-1.0f32..1.0),
            vcount,
        ));
        let uvs = prop::option::of(prop::collection::vec(
            prop::array::uniform2(0.0f32..8.0),
            vcount,
        ));
        let colors = prop::option::of(prop::collection::vec(
            prop::array::uniform4(0.0f32..1.0),
            vcount,
        ));
        // 1..=20 triangles; every index references an existing vertex.
        let indices = prop::collection::vec(0u32..(vcount as u32), 3..=60).prop_map(|mut v| {
            let keep = (v.len() / 3) * 3;
            v.truncate(keep.max(3));
            v
        });
        (positions, normals, uvs, colors, indices).prop_map(
            |(positions, normals, uvs, colors, indices)| MeshData {
                positions,
                normals,
                uvs: uvs.into_iter().collect(),
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

/// A base mesh (positions + a valid index buffer) of `vcount` vertices — the
/// carrier for the skin/morph attribute round-trips below. Geometry-channel
/// fidelity is already pinned above, so this keeps the base minimal.
fn base_mesh_strategy(vcount: usize) -> impl Strategy<Value = MeshData> {
    let positions = prop::collection::vec(prop::array::uniform3(-100.0f32..100.0), vcount);
    let indices = prop::collection::vec(0u32..(vcount as u32), 3..=30).prop_map(|mut v| {
        let keep = (v.len() / 3) * 3;
        v.truncate(keep.max(3));
        v
    });
    (positions, indices).prop_map(|(positions, indices)| MeshData {
        positions,
        normals: None,
        uvs: vec![],
        colors: None,
        indices,
    })
}

/// A skinned node: a base mesh + per-vertex `JOINTS_0`/`WEIGHTS_0`, plus a skin
/// with `njoints` joints and their inverse-bind matrices. Returns
/// `(mesh, njoints, joints, weights, inverse_bind_matrices)`.
type SkinnedCase = (
    MeshData,
    usize,
    Vec<[u16; 4]>,
    Vec<[f32; 4]>,
    Vec<[f32; 16]>,
);
fn skinned_mesh_strategy() -> impl Strategy<Value = SkinnedCase> {
    (3usize..20usize, 2usize..6usize).prop_flat_map(|(vcount, njoints)| {
        let mesh = base_mesh_strategy(vcount);
        // Per-vertex joint indices reference the skin's joint list (0..njoints);
        // weights are bounded finite (the writer stores them verbatim — no
        // normalization — so the round-trip is bit-exact).
        let joints = prop::collection::vec(prop::array::uniform4(0u16..(njoints as u16)), vcount);
        let weights = prop::collection::vec(prop::array::uniform4(0.0f32..1.0), vcount);
        let ibms = prop::collection::vec(prop::array::uniform16(-10.0f32..10.0), njoints);
        (mesh, Just(njoints), joints, weights, ibms)
    })
}

/// One morph target's deltas (positions + optional normals), each `vcount` long
/// (the writer drops a target whose delta length ≠ vertex count).
fn morph_target_strategy(vcount: usize) -> impl Strategy<Value = MorphTarget> {
    let positions = prop::collection::vec(prop::array::uniform3(-5.0f32..5.0), vcount);
    let normals = prop::option::of(prop::collection::vec(
        prop::array::uniform3(-1.0f32..1.0),
        vcount,
    ));
    (positions, normals).prop_map(|(positions, normals)| MorphTarget {
        name: None,
        positions,
        normals,
    })
}

/// A morphed node: a base mesh + `ntargets` morph targets + default weights
/// (one per target). Returns `(mesh, targets, weights)`.
type MorphCase = (MeshData, Vec<MorphTarget>, Vec<f32>);
fn morphed_mesh_strategy() -> impl Strategy<Value = MorphCase> {
    (3usize..20usize, 1usize..4usize).prop_flat_map(|(vcount, ntargets)| {
        let mesh = base_mesh_strategy(vcount);
        let targets = prop::collection::vec(morph_target_strategy(vcount), ntargets);
        let weights = prop::collection::vec(0.0f32..1.0, ntargets);
        (mesh, targets, weights)
    })
}

proptest! {
    /// Skin round-trip: a skinned node written to a GLB and re-extracted via
    /// `reexport_clean` reproduces, bit-exactly, the per-vertex `JOINTS_0`/
    /// `WEIGHTS_0`, the skin's joint flatten-index list, and the inverse-bind
    /// matrices. This is the fidelity net the "everything through the clean glb"
    /// decision rests on (docs/plans/todo.md §4) — the skin the editor decodes
    /// back from the rig glb must equal the one it exported.
    #[test]
    fn skin_roundtrips_bit_exact((md, njoints, joints, weights, ibms) in skinned_mesh_strategy()) {
        // Scene: one root mesh node (flatten index 0) skinned by skin 0, with
        // `njoints` child joint nodes (flatten indices 1..=njoints).
        let joint_children: Vec<ExportNode> =
            (0..njoints).map(|j| ExportNode::new(format!("joint{j}"))).collect();
        let mut mesh_node = ExportNode::new("skinned").with_mesh(md.clone());
        mesh_node.skin = Some(0);
        mesh_node.joints = Some(joints.clone());
        mesh_node.weights = Some(weights.clone());
        mesh_node.children = joint_children;
        let joint_flat: Vec<usize> = (1..=njoints).collect();
        let scene = GlbScene {
            nodes: vec![mesh_node],
            skins: vec![ExportSkin {
                joints: joint_flat.clone(),
                inverse_bind_matrices: ibms.clone(),
                skeleton: None,
            }],
            ..Default::default()
        };
        let bytes = write_glb(&scene);
        let clean = reexport_clean(&bytes).expect("round-tripped skinned glb must re-extract");

        // Per-vertex skin attributes on the re-extracted node.
        let got = &clean.nodes[0];
        prop_assert_eq!(got.skin, Some(0), "skin binding diverged");
        prop_assert_eq!(got.joints.as_ref(), Some(&joints), "per-vertex JOINTS_0 diverged");
        prop_assert_eq!(got.weights.as_ref(), Some(&weights), "per-vertex WEIGHTS_0 diverged");

        // The skin itself (joint flatten indices + inverse-bind matrices).
        prop_assert_eq!(clean.skins.len(), 1, "skin count diverged");
        prop_assert_eq!(&clean.skins[0].joints, &joint_flat, "skin joint list diverged");
        prop_assert_eq!(
            &clean.skins[0].inverse_bind_matrices, &ibms,
            "inverse-bind matrices diverged"
        );
    }

    /// Morph round-trip: a morphed node's per-target position/normal deltas and
    /// its default morph weights reproduce bit-exactly through
    /// `write_glb` → `reexport_clean` (docs/plans/todo.md §4).
    #[test]
    fn morph_roundtrips_bit_exact((md, targets, weights) in morphed_mesh_strategy()) {
        let mut node = ExportNode::new("morphed").with_mesh(md.clone());
        node.morph_targets = targets.clone();
        node.morph_weights = weights.clone();
        let scene = GlbScene { nodes: vec![node], ..Default::default() };
        let bytes = write_glb(&scene);
        let clean = reexport_clean(&bytes).expect("round-tripped morphed glb must re-extract");

        let got = &clean.nodes[0];
        prop_assert_eq!(got.morph_targets.len(), targets.len(), "morph target count diverged");
        for (i, (a, b)) in got.morph_targets.iter().zip(&targets).enumerate() {
            prop_assert_eq!(&a.positions, &b.positions, "morph target {} positions diverged", i);
            prop_assert_eq!(&a.normals, &b.normals, "morph target {} normals diverged", i);
        }
        prop_assert_eq!(&got.morph_weights, &weights, "morph weights diverged");
    }
}
