//! Property tests for the pure-data convert pipeline (no GPU/browser).
//!
//! The invariants that make the mesh-authoring round-trip trustworthy:
//!   1. converting arbitrary geometry yields a canonical glb that preserves the
//!      geometry and is stamped `AWSM_format`;
//!   2. convert is idempotent — re-converting our own output passes the exact
//!      bytes through (`convert(convert(x)) == convert(x)`);
//!   3. base-PBR material factors + alpha mode survive convert;
//!   4. an animation channel's sampler (times + flattened values) survives.
//!
//! `proptest` sweeps the matrix (vertex count, channel presence, index topology,
//! material factors, animation paths/interpolation) rather than hand-enumerating.

use awsm_glb_export::{extract_node_mesh_from_bytes, write_glb, ExportNode, GlbScene, MeshData};
use awsm_gltf_convert::{awsm_format_version, convert, is_canonical, AlphaMode, AWSM_FORMAT_VERSION};
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
        let indices = prop::collection::vec(0u32..(vcount as u32), 3..=60).prop_map(|mut v| {
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

fn pbr_material_strategy() -> impl Strategy<Value = awsm_glb_export::PbrMaterial> {
    use awsm_glb_export::{AlphaMode as GlbAlpha, PbrMaterial};
    (
        prop::array::uniform4(0.0f32..1.0),
        0.0f32..1.0,
        0.0f32..1.0,
        prop::array::uniform3(0.0f32..1.0),
        prop_oneof![
            Just(GlbAlpha::Opaque),
            (0.0f32..1.0).prop_map(|c| GlbAlpha::Mask { cutoff: c }),
            Just(GlbAlpha::Blend),
        ],
        any::<bool>(),
    )
        .prop_map(
            |(base_color, metallic, roughness, emissive, alpha_mode, double_sided)| PbrMaterial {
                name: "m".into(),
                base_color,
                metallic,
                roughness,
                emissive,
                alpha_mode,
                double_sided,
                ..Default::default()
            },
        )
}

fn anim_channel_strategy() -> impl Strategy<Value = awsm_glb_export::ExportAnimChannel> {
    use awsm_glb_export::{AnimInterp, AnimPath, ExportAnimChannel};
    let path = prop_oneof![
        Just((AnimPath::Translation, 3usize)),
        Just((AnimPath::Rotation, 4usize)),
        Just((AnimPath::Scale, 3usize)),
    ];
    let interp = prop_oneof![Just(AnimInterp::Linear), Just(AnimInterp::Step)];
    (path, interp, 1usize..6).prop_map(|((path, comps), interpolation, keys)| {
        // strictly increasing times 0, 1, 2, …
        let times: Vec<f32> = (0..keys).map(|i| i as f32).collect();
        let values: Vec<f32> = (0..keys * comps).map(|i| (i % 7) as f32 * 0.1).collect();
        ExportAnimChannel {
            node_index: 0,
            path,
            interpolation,
            times,
            values,
        }
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

    /// Idempotency: a second convert detects the marker and passes through.
    #[test]
    fn convert_is_idempotent(md in mesh_data_strategy()) {
        let once = convert(&glb_of(&md)).expect("convert 1");
        let twice = convert(&once.glb).expect("convert 2");
        prop_assert!(twice.is_already_canonical);
        prop_assert_eq!(&twice.glb, &once.glb);
    }

    /// Base-PBR material factors + alpha mode + double-sided survive convert.
    #[test]
    fn material_factors_round_trip(pbr in pbr_material_strategy()) {
        use awsm_glb_export::{AlphaMode as GlbAlpha, ExportMaterial};
        let mut node = ExportNode::new("m").with_mesh(awsm_meshgen::box_mesh(glam::Vec3::ONE));
        node.material = Some(ExportMaterial::Pbr(pbr.clone()));
        let glb = write_glb(&GlbScene { nodes: vec![node], ..Default::default() });

        let out = convert(&glb).expect("convert");
        prop_assert_eq!(out.materials.len(), 1);
        let m = &out.materials[0];
        prop_assert_eq!(m.base_color, pbr.base_color);
        prop_assert_eq!(m.metallic, pbr.metallic);
        prop_assert_eq!(m.roughness, pbr.roughness);
        prop_assert_eq!(m.emissive, pbr.emissive);
        prop_assert_eq!(m.double_sided, pbr.double_sided);
        let expected = match pbr.alpha_mode {
            GlbAlpha::Opaque => AlphaMode::Opaque,
            GlbAlpha::Mask { cutoff } => AlphaMode::Mask { cutoff },
            GlbAlpha::Blend => AlphaMode::Blend,
        };
        prop_assert_eq!(m.alpha_mode, expected);
    }

    /// An animation channel's sampler (times + flattened values) survives convert.
    #[test]
    fn animation_sampler_round_trips(ch in anim_channel_strategy()) {
        use awsm_glb_export::ExportAnimation;
        let node = ExportNode::new("m").with_mesh(awsm_meshgen::box_mesh(glam::Vec3::ONE));
        let glb = write_glb(&GlbScene {
            nodes: vec![node],
            animations: vec![ExportAnimation { name: "a".into(), channels: vec![ch.clone()] }],
            ..Default::default()
        });

        let out = convert(&glb).expect("convert");
        prop_assert_eq!(out.animations.len(), 1);
        let got = &out.animations[0].channels[0];
        prop_assert_eq!(&got.times, &ch.times);
        prop_assert_eq!(&got.values, &ch.values);
    }
}
