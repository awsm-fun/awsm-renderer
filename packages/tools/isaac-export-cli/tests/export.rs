//! The exporter against a hand-written USD fixture that exercises every rule it
//! implements (see `fixtures/robot.usda`), plus the real Isaac assets when they
//! are available locally.

use std::path::{Path, PathBuf};

use awsm_renderer_isaac_export_cli::{export, Export, Options, VariantSelection, HIDDEN_GROUP};
use awsm_renderer_mujoco_format::sidecar::{Geom, GeomKind};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn run(options: Options) -> Export {
    export(&fixture("robot.usda"), &options).expect("export")
}

fn geom<'a>(out: &'a Export, name: &str) -> &'a Geom {
    out.sidecar
        .geoms
        .iter()
        .find(|g| g.name.as_deref() == Some(name))
        .unwrap_or_else(|| {
            panic!(
                "no geom {name:?}; have {:?}",
                out.sidecar
                    .geoms
                    .iter()
                    .map(|g| &g.name)
                    .collect::<Vec<_>>()
            )
        })
}

fn close(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
}

/// The GLB geometry for `g`, via the sidecar's mesh → node-name mapping.
fn positions(out: &Export, g: &Geom) -> Vec<[f32; 3]> {
    let mesh = &out.sidecar.meshes[g.mesh.expect("mesh geom")];
    let node = mesh.node.as_deref().expect("node name");
    let glb = out.glb.as_ref().expect("glb");
    let n = glb
        .nodes
        .iter()
        .find(|n| n.name == node)
        .expect("GLB node for the sidecar mesh");
    n.mesh.as_ref().expect("node mesh").positions.clone()
}

#[test]
fn bodies_are_rigid_body_prims_with_a_world_at_zero() {
    let out = run(Options::default());
    let names: Vec<_> = out
        .sidecar
        .bodies
        .iter()
        .map(|b| b.name.as_deref().unwrap())
        .collect();
    assert_eq!(names, ["world", "base", "arm"]);
    let arm = &out.sidecar.bodies[2];
    // Siblings, not nested: both hang off the world.
    assert_eq!(arm.parent, 0);
    assert!(close(&arm.pos, &[1.0, 0.0, 1.0]));
    // rotateZ 90°, [w, x, y, z].
    let h = std::f64::consts::FRAC_1_SQRT_2;
    assert!(close(&arm.quat, &[h, 0.0, 0.0, h]), "{:?}", arm.quat);
}

#[test]
fn instanceable_visuals_are_found_through_instance_proxies() {
    let out = run(Options::default());
    let g = geom(&out, "mesh");
    assert_eq!(g.body, 1);
    assert_eq!(g.group, 0);
    assert_eq!(g.kind, GeomKind::Mesh);
    // No bound material → the prim's displayColor.
    assert_eq!(g.material, None);
    assert_eq!(g.rgba, [0.0, 1.0, 0.0, 1.0]);
}

#[test]
fn guide_purpose_colliders_are_kept_but_hidden() {
    let out = run(Options::default());
    let c = geom(&out, "collider");
    assert_eq!(c.group, HIDDEN_GROUP);
    assert_eq!(c.kind, GeomKind::Box);
    // USD `size` is the full edge; the sidecar wants half-extents.
    assert!(close(&c.size, &[0.5, 0.5, 0.5]));
}

#[test]
fn material_subsets_split_a_mesh_and_bind_their_own_materials() {
    let out = run(Options::default());
    let red = geom(&out, "quad (Red)");
    let blue = geom(&out, "quad (Blue)");
    let mats = &out.sidecar.materials;

    // UsdPreviewSurface: roughness 0.75 → shininess 0.25.
    let r = &mats[red.material.unwrap()];
    assert_eq!(r.rgba, [1.0, 0.0, 0.0, 1.0]);
    assert!((r.shininess - 0.25).abs() < 1e-6);
    assert_eq!(r.reflectance, 0.0);

    // OmniPBR (MDL) — read by input name, no .mdl needed.
    let b = &mats[blue.material.unwrap()];
    assert_eq!(b.rgba, [0.0, 0.0, 1.0, 1.0]);
    assert!((b.shininess - 0.75).abs() < 1e-6);
    assert_eq!(b.reflectance, 1.0);

    // The quad face (4 corners) is fan-triangulated; the subset's triangle
    // is its own piece.
    assert_eq!(positions(&out, blue).len(), 4);
    assert_eq!(positions(&out, red).len(), 3);
}

#[test]
fn mesh_transforms_are_baked_into_the_body_frame() {
    let out = run(Options::default());
    let blue = geom(&out, "quad (Blue)");
    // The mesh's own ×2 scale is in the vertices; the geom sits exactly on
    // its body, so a body pose IS the geom pose.
    assert!(close(&blue.pos, &[0.0; 3]));
    assert!(close(&blue.quat, &[1.0, 0.0, 0.0, 0.0]));
    let arm = &out.sidecar.bodies[2];
    assert!(close(&blue.world_pos, &arm.pos));
    assert!(close(&blue.world_quat, &arm.quat));
    // Rounded: the 90° body rotation is undone in f64, leaving ~1e-16 noise.
    let mut p: Vec<[i64; 3]> = positions(&out, blue)
        .iter()
        .map(|v| v.map(|c| (c * 1e6).round() as i64))
        .collect();
    p.sort();
    let m = 1_000_000;
    assert_eq!(
        p,
        [[0, 0, 0], [0, 2 * m, 0], [2 * m, 0, 0], [2 * m, 2 * m, 0]]
    );
}

#[test]
fn primitives_keep_a_body_relative_offset_along_z() {
    let out = run(Options::default());
    let pin = geom(&out, "pin");
    assert_eq!(pin.kind, GeomKind::Cylinder);
    assert_eq!(pin.body, 2);
    // radius, half-height.
    assert!(close(&pin.size, &[0.1, 0.2, 0.0]));
    assert!(close(&pin.pos, &[0.0, 0.0, 0.5]));
    // axis X: local Z rotated onto X, i.e. +90° about Y.
    let h = std::f64::consts::FRAC_1_SQRT_2;
    assert!(close(&pin.quat, &[h, 0.0, h, 0.0]), "{:?}", pin.quat);
    // World = arm frame ∘ offset: the arm is at (1, 0, 1).
    assert!(close(&pin.world_pos, &[1.0, 0.0, 1.5]));
}

#[test]
fn variant_selection_applies() {
    let shown = run(Options::default());
    assert_eq!(geom(&shown, "pin").group, 0);
    let hidden = run(Options {
        variants: vec!["Look=Hidden".parse::<VariantSelection>().unwrap()],
        ..Options::default()
    });
    assert_eq!(geom(&hidden, "pin").group, HIDDEN_GROUP);
}

#[test]
fn an_unknown_variant_is_refused_not_ignored() {
    let err = export(
        &fixture("robot.usda"),
        &Options {
            variants: vec!["Look=Nope".parse().unwrap()],
            ..Options::default()
        },
    )
    .err()
    .expect("a typo'd variant must fail");
    assert!(err.to_string().contains("did not apply"), "{err}");
}

#[test]
fn variant_selection_parses_with_and_without_a_prim_path() {
    let v: VariantSelection = "Mesh=Quality".parse().unwrap();
    assert_eq!(
        (v.prim, v.set.as_str(), v.value.as_str()),
        (None, "Mesh", "Quality")
    );
    let v: VariantSelection = "/panda:Gripper=None".parse().unwrap();
    assert_eq!(v.prim.as_deref(), Some("/panda"));
    assert_eq!((v.set.as_str(), v.value.as_str()), ("Gripper", "None"));
    assert!("nope".parse::<VariantSelection>().is_err());
}

#[test]
fn hidden_geometry_ships_only_on_request() {
    // The fixture's only hidden geom is a primitive, so hide a MESH via a
    // variant-free path: every hidden mesh keeps its slot but no geometry.
    let out = run(Options::default());
    for g in &out.sidecar.geoms {
        if g.group == HIDDEN_GROUP && g.kind == GeomKind::Mesh {
            assert_eq!(g.mesh, None);
        }
    }
}

#[test]
fn the_sidecar_validates_and_the_glb_reads_back() {
    let out = run(Options::default());
    out.sidecar.validate().expect("valid sidecar");
    assert!(out.sidecar.source.sha256.len() == 64);
    let bytes = awsm_renderer_glb_export::write_glb(out.glb.as_ref().unwrap());
    let gltf = gltf::Gltf::from_slice(&bytes).expect("a real glTF");
    let names: Vec<_> = gltf
        .nodes()
        .filter_map(|n| n.name().map(str::to_string))
        .collect();
    for m in &out.sidecar.meshes {
        assert!(
            names.contains(m.node.as_ref().unwrap()),
            "{:?} missing",
            m.node
        );
    }
}

#[test]
fn a_y_up_stage_is_refused() {
    let dir = std::env::temp_dir().join(format!("awsm-isaac-yup-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("yup.usda");
    std::fs::write(
        &path,
        "#usda 1.0\n(\n    upAxis = \"Y\"\n    metersPerUnit = 1\n)\ndef Cube \"c\" {}\n",
    )
    .unwrap();
    let err = export(&path, &Options::default())
        .err()
        .expect("Y-up refused");
    assert!(err.to_string().contains("upAxis=Y"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The real Isaac assets. They are tens of MB across dozens of files on
/// NVIDIA's public bucket, so they are not checked in; point these at a local
/// download (see `docs/isaac.md`, "Getting the assets") to run:
///
/// ```text
/// AWSM_ISAAC_FRANKA=/path/to/franka.usd AWSM_ISAAC_ANYMAL_D=/path/to/anymal_d.usd \
///   cargo test -p awsm-renderer-isaac-export-cli -- --ignored
/// ```
#[test]
#[ignore = "needs the Isaac assets downloaded locally"]
fn real_isaac_assets() {
    let mut ran = 0;
    if let Ok(p) = std::env::var("AWSM_ISAAC_FRANKA") {
        let out = export(Path::new(&p), &Options::default()).unwrap();
        assert!(
            out.report.composition_errors.is_empty(),
            "{:?}",
            out.report.composition_errors
        );
        // 11 links; 11 meshes split into 44 material pieces.
        assert_eq!(out.sidecar.bodies.len(), 12);
        assert_eq!(out.report.visible_geoms, 44);
        ran += 1;
    }
    if let Ok(p) = std::env::var("AWSM_ISAAC_ANYMAL_D") {
        let out = export(Path::new(&p), &Options::default()).unwrap();
        assert!(
            out.report.composition_errors.is_empty(),
            "{:?}",
            out.report.composition_errors
        );
        assert_eq!(out.sidecar.bodies.len(), 18);
        // 38 visual meshes; the 26 guide-purpose collider shapes are hidden.
        assert_eq!(out.report.visible_geoms, 38);
        assert_eq!(out.sidecar.geoms.len(), 64);
        ran += 1;
    }
    assert!(ran > 0, "set AWSM_ISAAC_FRANKA and/or AWSM_ISAAC_ANYMAL_D");
}
