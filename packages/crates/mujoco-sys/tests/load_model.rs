//! Loads real MuJoCo models through the real library. Self-skips (loudly) when no
//! MuJoCo install is around, so the workspace test run stays green on machines
//! without one — but on a machine that HAS one, these are the only proof the
//! struct layout is being read correctly.

use std::path::PathBuf;

use awsm_renderer_mujoco_sys::{mjtObj, Library, Model};

/// The models shipped inside the MuJoCo release itself, so there is nothing to
/// download separately. `MUJOCO_DIR/model/...`.
fn release_model(rel: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("MUJOCO_DIR")?)
        .join("model")
        .join(rel);
    dir.exists().then_some(dir)
}

/// Skipping is only ever allowed when there is NO install to test against. Once
/// `MUJOCO_DIR` is set the user has pointed at one, and every failure past that —
/// wrong version, unreadable dylib, missing model — is a real failure. (A blanket
/// skip-on-any-error hid a genuine version-check bug here once: every test
/// "passed" while nothing had loaded at all.)
fn skip_or_panic(what: std::fmt::Arguments<'_>) {
    if std::env::var_os("MUJOCO_DIR").is_some() {
        panic!("MUJOCO_DIR is set, so this must work: {what}");
    }
    eprintln!("SKIP: {what}");
}

fn library() -> Option<Library> {
    match Library::load() {
        Ok(lib) => Some(lib),
        Err(e) => {
            skip_or_panic(format_args!("{e}"));
            None
        }
    }
}

fn with_model<F: FnOnce(&Model<'_>)>(rel: &str, f: F) {
    let Some(lib) = library() else { return };
    let Some(path) = release_model(rel) else {
        skip_or_panic(format_args!("no {rel} under $MUJOCO_DIR/model"));
        return;
    };
    let model = lib.load_model(&path).expect("model should compile");
    f(&model);
}

#[test]
fn loads_the_humanoid() {
    with_model("humanoid/humanoid.xml", |m| {
        // The DeepMind humanoid: a torso-rooted ragdoll plus the world body and a
        // ground plane. Exact counts are a layout canary — if the struct were
        // misaligned these would be garbage, not merely different.
        assert_eq!(m.nbody(), 17, "16 humanoid bodies + world");
        assert_eq!(m.ngeom(), 20, "19 body geoms + the floor plane");
        assert_eq!(m.nmesh(), 0, "humanoid.xml is all primitives");

        // Every accessor's length must agree with its count x stride.
        assert_eq!(m.geom_type().len(), m.ngeom());
        assert_eq!(m.geom_size().len(), m.ngeom() * 3);
        assert_eq!(m.geom_quat().len(), m.ngeom() * 4);
        assert_eq!(m.body_parentid().len(), m.nbody());

        // Body 0 is always the world, and it is its own parent.
        assert_eq!(m.name(mjtObj::mjOBJ_BODY, 0), Some("world"));
        assert_eq!(m.body_parentid()[0], 0);
        assert_eq!(m.id(mjtObj::mjOBJ_BODY, "torso"), Some(1));
        assert_eq!(m.id(mjtObj::mjOBJ_BODY, "no_such_body"), None);

        // Geom 0 is the floor plane; a plane's size is (x half-extent, y, grid
        // spacing) and this one is authored infinite, i.e. 0 half-extents.
        assert_eq!(m.name(mjtObj::mjOBJ_GEOM, 0), Some("floor"));
        assert_eq!(
            m.geom_kind(0),
            Some(awsm_renderer_mujoco_sys::mjtGeom::mjGEOM_PLANE)
        );

        // Values, not just shapes: quaternions are unit and sizes are positive
        // metres. Garbage reads would fail both.
        for g in 0..m.ngeom() {
            let q = &m.geom_quat()[g * 4..g * 4 + 4];
            let norm = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
            assert!((norm - 1.0).abs() < 1e-9, "geom {g} quat not unit: {q:?}");
            let s = &m.geom_size()[g * 3..g * 3 + 3];
            assert!(
                s.iter().all(|v| v.is_finite() && *v >= 0.0),
                "geom {g} size {s:?}"
            );
            assert!(
                s.iter().all(|v| *v < 1e3),
                "geom {g} size implausible {s:?}"
            );
        }

        // Groups: the humanoid authors everything visible, so nothing above 2.
        assert!(m.geom_group().iter().all(|g| (0..=2).contains(g)));
    });
}

#[test]
fn loads_a_mesh_and_material_model() {
    // The release's own flex demo carries meshes, materials and textures — the
    // parts of mjModel the exporter's non-primitive path will read.
    with_model("flex/flag.xml", |m| {
        assert!(m.nflex() > 0, "flag.xml should have a flex");
        assert_eq!(m.mat_rgba().len(), m.nmat() * 4);
        assert_eq!(m.mat_specular().len(), m.nmat());
        for c in m.mat_rgba() {
            assert!((0.0..=1.0).contains(c), "material rgba out of range: {c}");
        }
    });
}

#[test]
fn rejects_a_nonexistent_file() {
    let Some(lib) = library() else { return };
    let err = lib
        .load_model(&PathBuf::from("/definitely/not/a/model.xml"))
        .expect_err("should not load");
    // MuJoCo's own compiler error, surfaced verbatim — we never parse MJCF, so its
    // diagnostics are the only ones users should ever see.
    assert!(format!("{err}").contains("model.xml"), "{err}");
}

#[test]
fn missing_library_is_a_clean_error() {
    // The no-MuJoCo path must be a typed error with install instructions, not a
    // panic — this is what a fresh checkout hits.
    let err = unsafe { Library::load_from(std::path::Path::new("/nope/libmujoco.dylib")) }
        .expect_err("should not load");
    let msg = format!("{err}");
    assert!(msg.contains("MUJOCO_DIR"), "{msg}");
}
