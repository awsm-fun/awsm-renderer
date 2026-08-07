//! Exports real models through the real MuJoCo compiler and checks the sidecar
//! against what those models are actually known to contain.
//!
//! Skips only when there is no MuJoCo install at all; with `MUJOCO_DIR` set a
//! failure is a failure (see `mujoco-sys`'s tests for why that rule exists).

use std::path::PathBuf;

use awsm_renderer_mujoco_format::sidecar::{GeomKind, Sidecar};

#[path = "../src/sidecar.rs"]
mod sidecar;

fn export(path: PathBuf) -> Option<Sidecar> {
    let lib = match awsm_renderer_mujoco_sys::Library::load() {
        Ok(lib) => lib,
        Err(e) => {
            assert!(
                std::env::var_os("MUJOCO_DIR").is_none(),
                "MUJOCO_DIR is set, so this must work: {e}"
            );
            eprintln!("SKIP: {e}");
            return None;
        }
    };
    if !path.exists() {
        eprintln!("SKIP: no model at {}", path.display());
        return None;
    }
    let model = lib.load_model(&path).expect("model should compile");
    let source = sidecar::fingerprint(&path, lib.version_string()).unwrap();
    Some(sidecar::build(&model, source).expect("sidecar should build"))
}

fn release_model(rel: &str) -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("MUJOCO_DIR")?)
            .join("model")
            .join(rel),
    )
}

/// A menagerie checkout. Not bundled with MuJoCo, so this one is opt-in via env.
fn menagerie_model(rel: &str) -> Option<PathBuf> {
    let base = std::env::var_os("MUJOCO_MENAGERIE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            let home =
                PathBuf::from(std::env::var_os("HOME")?).join(".local/share/mujoco_menagerie");
            home.is_dir().then_some(home)
        })?;
    Some(base.join(rel))
}

#[test]
fn exports_the_humanoid() {
    let Some(path) = release_model("humanoid/humanoid.xml") else {
        eprintln!("SKIP: no MUJOCO_DIR");
        return;
    };
    let Some(s) = export(path) else { return };

    s.validate().unwrap();
    assert_eq!(s.format, awsm_renderer_mujoco_format::sidecar::MAGIC);
    assert_eq!(s.model_name.as_deref(), Some("Humanoid"));
    assert_eq!(
        s.source.filename, "humanoid.xml",
        "filename only, never a path"
    );
    assert_eq!(s.source.sha256.len(), 64);
    assert_eq!(s.source.mujoco_version, "3.11.0");

    assert_eq!(s.bodies.len(), 17);
    assert_eq!(s.bodies[0].name.as_deref(), Some("world"));
    assert_eq!(s.geoms.len(), 20);
    assert!(s.meshes.is_empty(), "humanoid is all primitives");

    // The floor is a plane, and its size params are a plane's (half-extents +
    // grid spacing), not a box's.
    let floor = s
        .geoms
        .iter()
        .find(|g| g.name.as_deref() == Some("floor"))
        .unwrap();
    assert_eq!(floor.kind, GeomKind::Plane);
    assert_eq!(floor.body, 0, "the floor hangs off the world body");

    // Every geom's body index resolves and every quaternion is unit — the two
    // things that would make the pose binding silently wrong.
    for g in &s.geoms {
        assert!(g.body < s.bodies.len());
        let q = g.quat;
        let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
        assert!((n - 1.0).abs() < 1e-9, "{:?} quat {q:?}", g.name);
    }
}

#[test]
fn exports_a_unitree_with_the_menagerie_group_split() {
    let Some(path) = menagerie_model("unitree_go2/go2.xml") else {
        eprintln!("SKIP: no MUJOCO_MENAGERIE_DIR");
        return;
    };
    let Some(s) = export(path) else { return };
    s.validate().unwrap();

    assert_eq!(s.model_name.as_deref(), Some("go2"));
    assert_eq!(s.bodies.len(), 14, "base + 4 legs x 3 + world");

    // THE thing this phase has to get right: menagerie splits visual meshes
    // (group 2) from collision primitives (group 3). Rendering the wrong set
    // renders the robot's collision capsules instead of the robot.
    let visual: Vec<_> = s.geoms.iter().filter(|g| g.group == 2).collect();
    let collision: Vec<_> = s.geoms.iter().filter(|g| g.group == 3).collect();
    assert_eq!(visual.len(), 33);
    assert_eq!(collision.len(), 23);
    assert_eq!(
        visual.len() + collision.len(),
        s.geoms.len(),
        "no other groups"
    );

    // Visual geoms are all meshes with a resolvable mesh + material...
    assert!(visual.iter().all(|g| g.kind == GeomKind::Mesh));
    for g in &visual {
        let mesh = g.mesh.expect("visual mesh geom must reference a mesh");
        assert!(mesh < s.meshes.len());
        let mat = g.material.expect("menagerie visual geoms are materialed");
        assert!(mat < s.materials.len());
    }
    // ...and collision geoms are all primitives, materialless.
    assert!(collision.iter().all(|g| matches!(
        g.kind,
        GeomKind::Box | GeomKind::Sphere | GeomKind::Cylinder
    )));
    assert!(collision.iter().all(|g| g.mesh.is_none()));

    assert_eq!(s.meshes.len(), 16);
    let names: Vec<_> = s
        .materials
        .iter()
        .filter_map(|m| m.name.as_deref())
        .collect();
    assert_eq!(names, ["metal", "black", "white", "gray"]);
}

#[test]
fn the_fingerprint_tracks_content_not_path() {
    // Two copies of the same bytes under different paths must fingerprint the
    // same, or a harness on another machine could never match its model.
    let dir = std::env::temp_dir().join("awsm-mujoco-fingerprint-test");
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("a.xml");
    let b = dir.join("nested_b.xml");
    std::fs::write(&a, b"<mujoco/>").unwrap();
    std::fs::write(&b, b"<mujoco/>").unwrap();

    let fa = sidecar::fingerprint(&a, "3.11.0".into()).unwrap();
    let fb = sidecar::fingerprint(&b, "3.11.0".into()).unwrap();
    assert_eq!(fa.sha256, fb.sha256);
    assert_ne!(fa.filename, fb.filename);
    assert!(!fa.filename.contains('/'), "filename only: {}", fa.filename);

    std::fs::write(&b, b"<mujoco model='x'/>").unwrap();
    assert_ne!(
        fa.sha256,
        sidecar::fingerprint(&b, "3.11.0".into()).unwrap().sha256
    );
    std::fs::remove_dir_all(&dir).ok();
}
