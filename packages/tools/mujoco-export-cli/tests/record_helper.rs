//! Shared recorder for the integration tests: steps a release model and returns
//! the capture, or `None` when there is no MuJoCo to test against.

use std::path::PathBuf;

use awsm_renderer_mujoco_export_cli::sidecar;
use awsm_renderer_mujoco_format::capture::{Capture, Frame};

#[allow(dead_code)]
pub fn record(rel: &str, seconds: f64, fps: f64) -> Option<Capture> {
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
    // Body poses only for a model with a deformable — same rule the recorder
    // binary applies, for the same reason.
    let nbody = match model.nflex() > 0 {
        true => model.nbody(),
        false => 0,
    };
    let mut out = Capture::new(source, model.ngeom() as u32).with_bodies(nbody as u32);
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
            let bpos = data.xpos();
            let bquat = data.xquat();
            for b in 0..nbody {
                let p = &bpos[b * 3..b * 3 + 3];
                let q = &bquat[b * 4..b * 4 + 4];
                f.push_body(
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
