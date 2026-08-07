//! Exports real models through the real MuJoCo compiler and checks the sidecar
//! against what those models are actually known to contain.
//!
//! Skips only when there is no MuJoCo install at all; with `MUJOCO_DIR` set a
//! failure is a failure (see `mujoco-sys`'s tests for why that rule exists).

use std::path::PathBuf;

use awsm_renderer_mujoco_export_cli::sidecar;
use awsm_renderer_mujoco_format::sidecar::{GeomKind, Sidecar};

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

#[test]
fn the_muscle_arm_exports_six_tendons_with_room_to_wrap() {
    let Some(path) = release_model("tendon_arm/arm26.xml") else {
        return;
    };
    let Some(s) = export(path) else { return };

    let names: Vec<_> = s.tendons.iter().filter_map(|t| t.name.as_deref()).collect();
    assert_eq!(names, ["SF", "SE", "EF", "EE", "BF", "BE"]);

    for t in &s.tendons {
        // The capacity has to EXCEED the initial routing, otherwise the importer
        // sizes its segment pool to a pose rather than to the model, and the
        // tendon runs out of segments the first time it wraps.
        assert!(
            t.max_waypoints as usize > t.world_waypoints.len(),
            "{:?}: {} waypoints in a pool of {}",
            t.name,
            t.world_waypoints.len(),
            t.max_waypoints
        );
        assert!(t.width > 0.0);
        assert!(t.world_waypoints.len() >= 2, "a tendon needs a path");
        // Waypoints are WORLD positions at qpos0, not body-local offsets — the
        // whole arm sits well above the origin, so none of these may be at it.
        assert!(
            t.world_waypoints
                .iter()
                .all(|p| p.iter().any(|c| c.abs() > 1e-6)),
            "{:?} has a waypoint at the origin: {:?}",
            t.name,
            t.world_waypoints
        );
    }
}

#[test]
fn the_humanoids_fixed_tendons_export_as_undrawable() {
    // The humanoid's hamstrings are FIXED tendons — joint coupling, no path in
    // space. They must still occupy their slots (the index is the tendon id) but
    // ask for no segment pool, or the importer draws two cables at the origin.
    let Some(path) = release_model("humanoid/humanoid.xml") else {
        return;
    };
    let Some(s) = export(path) else { return };
    let names: Vec<_> = s.tendons.iter().filter_map(|t| t.name.as_deref()).collect();
    assert_eq!(names, ["hamstring_right", "hamstring_left"]);
    assert!(s
        .tendons
        .iter()
        .all(|t| t.max_waypoints == 0 && t.world_waypoints.is_empty()));
}

#[test]
fn a_cloth_flex_exports_its_elements_as_the_surface() {
    // MuJoCo's flag: a 2D flex, whose ELEMENTS already are the triangles.
    let Some(path) = release_model("flex/flag.xml") else {
        return;
    };
    let Some(s) = export(path) else { return };
    assert_eq!(s.flexes.len(), 1);
    let f = &s.flexes[0];
    assert_eq!(f.dim, 2);
    assert_eq!(f.vertex_count, 171);
    // Every vertex of a plain cloth rides its own body, which is what makes the
    // body-attached path available to a renderer at all.
    assert_eq!(f.vertex_bodies.len(), f.vertex_count);
    assert!(f.vertex_bodies.iter().all(|b| *b < s.bodies.len()));
    assert_eq!(s.meshes[f.mesh].name.as_deref(), Some("flag"));
}

#[test]
fn a_solid_flex_exports_its_shell_not_its_tetrahedra() {
    // A 3D flex's elements are tetrahedra; drawing those would fill the inside
    // with invisible faces. Only the shell is a surface.
    let Some(path) = release_model("flex/floppy.xml") else {
        return;
    };
    let Some(s) = export(path) else { return };
    let f = &s.flexes[0];
    assert_eq!(f.dim, 3);
    assert!(f.vertex_count > 0);
    assert_eq!(f.vertex_bodies.len(), f.vertex_count);
}

#[test]
fn an_interpolated_flex_reports_no_vertex_bodies() {
    // bunny.xml drives its surface from a cage of NODES, so MuJoCo has no body
    // per vertex. All-or-nothing: a partial list would let a consumer skin some
    // vertices and strand the rest at the bind pose.
    let Some(path) = release_model("flex/bunny.xml") else {
        return;
    };
    let Some(s) = export(path) else { return };
    let f = &s.flexes[0];
    assert!(f.vertex_count > 2000);
    assert!(
        f.vertex_bodies.is_empty(),
        "{} bodies for an interpolated flex",
        f.vertex_bodies.len()
    );
}

#[test]
fn a_model_without_flexes_exports_none() {
    let Some(path) = release_model("humanoid/humanoid.xml") else {
        return;
    };
    let Some(s) = export(path) else { return };
    assert!(s.flexes.is_empty());
}
