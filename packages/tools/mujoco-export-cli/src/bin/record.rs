//! `awsm-renderer-mujoco-record` — the reference capture recorder.
//!
//! Steps a compiled model with MuJoCo's own integrator and writes the resulting
//! world poses as a capture file, which the editor bakes into ordinary animation
//! clips. It exists for two reasons:
//!
//! 1. to be the worked example of a capture *producer*, so a third-party harness
//!    can see exactly what the format expects (there is nothing privileged about
//!    this tool — a Python RL loop writing the same JSON is equally valid);
//! 2. to produce the deterministic fixture captures the browser test suite
//!    replays, so CI never has to run a simulation.
//!
//! It is a **native tool**, like the exporter. No MuJoCo runtime code ever ships
//! in the renderer or the editor (see `docs/mujoco.md`, Non-goals).

use anyhow::{Context, Result};
use awsm_renderer_mujoco_export_cli::sidecar;
use awsm_renderer_mujoco_format::capture::{Capture, Frame};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "awsm-renderer-mujoco-record",
    about = "Step a MuJoCo model and record its geom world poses as a capture file",
    version
)]
struct Args {
    /// Model to simulate: `.xml` (MJCF), `.urdf`, or a version-locked `.mjb`.
    /// Must be the SAME file the sidecar was exported from — the capture carries
    /// its fingerprint, and the editor refuses to bake a mismatch.
    model: PathBuf,

    /// Output directory. Written as `<name>.capture.json`.
    #[arg(short, long, default_value = ".")]
    out_dir: PathBuf,

    /// Base name for the output. Defaults to the compiled model's own name.
    #[arg(short, long)]
    name: Option<String>,

    /// How many seconds of simulation to record.
    #[arg(long, default_value_t = 3.0)]
    seconds: f64,

    /// Frames recorded per second. The sim always steps at the model's own
    /// timestep; this only controls how often a step is *sampled*, so lowering
    /// it shrinks the file without changing the physics.
    #[arg(long, default_value_t = 60.0)]
    fps: f64,

    /// Start from this keyframe instead of `qpos0` (MJCF `<key>` index). Most
    /// menagerie models ship a `home` keyframe as index 0.
    #[arg(long)]
    keyframe: Option<i32>,

    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    anyhow::ensure!(args.seconds > 0.0, "--seconds must be positive");
    anyhow::ensure!(args.fps > 0.0, "--fps must be positive");

    let lib = awsm_renderer_mujoco_sys::Library::load()?;
    let model = lib.load_model(&args.model)?;
    let source = sidecar::fingerprint(&args.model, lib.version_string())?;

    let mut data = match args.keyframe {
        Some(k) => model.reset_to_keyframe(k)?,
        None => model.forward_at_initial_pose()?,
    };

    let timestep = model.timestep();
    anyhow::ensure!(timestep > 0.0, "model has a non-positive timestep");
    // Sample every Nth step rather than accumulating a float clock: the frame
    // times then land exactly on step boundaries, so a re-run produces a
    // byte-identical capture. That determinism is the point of the fixtures.
    let steps_per_frame = ((1.0 / args.fps) / timestep).round().max(1.0) as u64;
    let total_steps = (args.seconds / timestep).round() as u64;

    // The body channel is recorded only for a model with a DEFORMABLE. A flex
    // imports as a skinned mesh whose joints are the bodies its cage rides, so
    // its bodies are the only way to replay the deformation — while for a purely
    // rigid model they would be pure duplication of the geom poses, roughly
    // doubling the file for nothing.
    let nbody = match model.nflex() > 0 {
        true => model.nbody(),
        false => 0,
    };
    let mut out = Capture::new(source, model.ngeom() as u32).with_bodies(nbody as u32);
    let mut step = 0u64;
    loop {
        if step.is_multiple_of(steps_per_frame) {
            out.frames
                .push(sample(&data, model.ngeom(), nbody, step as f64 * timestep));
        }
        if step >= total_steps {
            break;
        }
        data.step();
        step += 1;
    }

    out.validate()
        .map_err(|e| anyhow::anyhow!("recorded an invalid capture: {e}"))?;

    let name = args
        .name
        .or_else(|| model.model_name().map(str::to_string))
        .or_else(|| {
            args.model
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "model".to_string());
    std::fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("creating {}", args.out_dir.display()))?;
    let path = args.out_dir.join(format!("{name}.capture.json"));
    // Compact, unlike the sidecar: a capture is machine-written bulk data that
    // nobody hand-edits, and pretty-printing it would triple the file.
    let json = serde_json::to_string(&out)?;
    std::fs::write(&path, &json).with_context(|| format!("writing {}", path.display()))?;

    println!("{}", path.display());
    if args.verbose {
        println!("  model      {name}");
        println!("  geoms      {}", out.geom_count);
        if out.body_count > 0 {
            println!("  bodies     {} (deformable)", out.body_count);
        }
        println!("  timestep   {timestep} s ({steps_per_frame} steps/frame)");
        println!(
            "  frames     {} over {:.3} s",
            out.frames.len(),
            out.duration()
        );
        println!("  size       {} KiB", json.len() / 1024);
    }
    Ok(())
}

/// One frame: every geom's world pose in geom-id order, then — when the model
/// has a deformable — every body's world pose in body-id order.
fn sample(
    data: &awsm_renderer_mujoco_sys::Data<'_, '_>,
    ngeom: usize,
    nbody: usize,
    time: f64,
) -> Frame {
    let mut frame = Frame {
        time,
        geom_poses: Vec::with_capacity(ngeom * 7),
        body_poses: Vec::with_capacity(nbody * 7),
    };
    let xpos = data.geom_xpos();
    for g in 0..ngeom {
        let p = &xpos[g * 3..g * 3 + 3];
        let q = data.geom_world_quat(g);
        frame.push_geom(
            [p[0] as f32, p[1] as f32, p[2] as f32],
            [q[0] as f32, q[1] as f32, q[2] as f32, q[3] as f32],
        );
    }
    // `xquat` is already a quaternion in MuJoCo's [w, x, y, z] order — the very
    // order a frame stores — so unlike the geoms there is no matrix to convert.
    let bpos = data.xpos();
    let bquat = data.xquat();
    for b in 0..nbody {
        let p = &bpos[b * 3..b * 3 + 3];
        let q = &bquat[b * 4..b * 4 + 4];
        frame.push_body(
            [p[0] as f32, p[1] as f32, p[2] as f32],
            [q[0] as f32, q[1] as f32, q[2] as f32, q[3] as f32],
        );
    }
    frame
}
