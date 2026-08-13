//! Records real simulations through the real MuJoCo integrator and checks the
//! capture against what the physics must have done.
//!
//! Skips only when there is no MuJoCo install; with `MUJOCO_DIR` set a failure
//! is a failure (see `mujoco-sys`'s tests for why).

use std::path::PathBuf;

use awsm_renderer_mujoco_export_cli::sidecar;
use awsm_renderer_mujoco_format::capture::{Capture, Frame};

fn record(rel: &str, seconds: f64, fps: f64) -> Option<Capture> {
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
    let path = PathBuf::from(std::env::var_os("MUJOCO_DIR")?)
        .join("model")
        .join(rel);
    if !path.exists() {
        eprintln!("SKIP: no {rel}");
        return None;
    }
    let model = lib.load_model(&path).expect("model should compile");
    let mut data = model.forward_at_initial_pose().unwrap();
    let timestep = model.timestep();
    let steps_per_frame = ((1.0 / fps) / timestep).round().max(1.0) as u64;
    let total = (seconds / timestep).round() as u64;

    let source = sidecar::fingerprint(&path, lib.version_string()).unwrap();
    let mut out = Capture::new(source, model.ngeom() as u32);
    let mut step = 0u64;
    loop {
        if step.is_multiple_of(steps_per_frame) {
            let mut f = Frame {
                time: step as f64 * timestep,
                geom_poses: Vec::new(),
                body_poses: Vec::new(),
            };
            for g in 0..model.ngeom() {
                let p = &data.geom_xpos()[g * 3..g * 3 + 3];
                let q = data.geom_world_quat(g);
                f.push_geom(
                    [p[0] as f32, p[1] as f32, p[2] as f32],
                    [q[0] as f32, q[1] as f32, q[2] as f32, q[3] as f32],
                );
            }
            out.frames.push(f);
        }
        if step >= total {
            break;
        }
        data.step();
        step += 1;
    }
    Some(out)
}

#[test]
fn the_humanoid_ragdoll_actually_falls() {
    let Some(c) = record("humanoid/humanoid.xml", 3.0, 30.0) else {
        return;
    };
    c.validate().unwrap();
    assert_eq!(c.geom_count, 20);
    assert!(c.frames.len() > 80, "{} frames", c.frames.len());
    assert!((c.duration() - 3.0).abs() < 0.05);

    // Geom 1 is the torso. Standing at ~1.28 m, it must end up on the floor —
    // this is the check that the recorder is stepping physics at all rather than
    // dumping the same pose N times.
    let z = |f: &Frame| f.geom(1).unwrap().0[2];
    let first = z(c.frames.first().unwrap());
    let last = z(c.frames.last().unwrap());
    assert!(first > 1.0, "torso should start standing, got {first}");
    assert!(last < 0.5, "torso should have fallen, got {last}");

    // Every quaternion stays unit through the whole run: a drifting norm would
    // mean the matrix→quaternion conversion is wrong somewhere in the range.
    for (i, f) in c.frames.iter().enumerate() {
        for g in 0..c.geom_count as usize {
            let (_, q) = f.geom(g).unwrap();
            let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
            assert!((n - 1.0).abs() < 1e-4, "frame {i} geom {g} quat {q:?}");
        }
    }
}

#[test]
fn recording_is_deterministic() {
    // The fixtures the browser test suite replays are only useful if re-running
    // the recorder reproduces them exactly. Sampling on step counts rather than
    // an accumulated float clock is what makes that true.
    let (Some(a), Some(b)) = (
        record("humanoid/humanoid.xml", 1.0, 30.0),
        record("humanoid/humanoid.xml", 1.0, 30.0),
    ) else {
        return;
    };
    assert_eq!(a, b);
}

#[test]
fn frame_zero_matches_the_sidecars_initial_pose() {
    // The capture and the sidecar must agree about where the model starts, or
    // the first simulated frame would visibly jump away from the imported pose.
    let Some(c) = record("humanoid/humanoid.xml", 0.1, 30.0) else {
        return;
    };
    let lib = awsm_renderer_mujoco_sys::Library::load().unwrap();
    let path =
        PathBuf::from(std::env::var_os("MUJOCO_DIR").unwrap()).join("model/humanoid/humanoid.xml");
    let model = lib.load_model(&path).unwrap();
    let doc = sidecar::build(
        &model,
        sidecar::fingerprint(&path, lib.version_string()).unwrap(),
    )
    .unwrap();

    let frame0 = c.frames.first().unwrap();
    for (g, geom) in doc.geoms.iter().enumerate() {
        let (pos, _) = frame0.geom(g).unwrap();
        for (a, (captured, authored)) in pos.iter().zip(geom.world_pos.iter()).enumerate() {
            assert!(
                (*captured as f64 - authored).abs() < 1e-5,
                "geom {g} axis {a}: capture {captured} vs sidecar {authored}"
            );
        }
    }
}
