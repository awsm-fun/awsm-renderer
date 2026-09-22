//! `awsm-renderer-isaac-record` — make a robot move with no simulator.
//!
//! Sweeps every movable joint of a USD robot through a smooth, looping,
//! SCRIPTED motion, runs forward kinematics over its UsdPhysics joints, and
//! writes the resulting geom poses as a `<name>.capture.json` — the same
//! capture format (and the same fingerprint) a live Isaac Lab stream records.
//! It is joint animation, not physics: nothing collides, nothing falls, and a
//! floating base (a quadruped's) stays where it was authored.
//!
//! The capture replays through the pose sink exactly like a streamed run (a
//! player loops its frames into `apply_geom_poses`), or bakes into an animation
//! clip in the editor (`ImportMujocoCapture`).

use std::path::PathBuf;

use anyhow::{Context, Result};
use awsm_renderer_isaac_export_cli::kinematics::{record, JointKind, Motion, Sweep};
use awsm_renderer_isaac_export_cli::{export, Options, VariantSelection};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "awsm-renderer-isaac-record",
    about = "Record a scripted joint-motion capture of a USD robot (forward kinematics, no physics)",
    version
)]
struct Args {
    /// Root USD file.
    model: PathBuf,
    /// Output directory; written as `<name>.capture.json`.
    #[arg(short, long, default_value = ".")]
    out_dir: PathBuf,
    /// Base name. Defaults to the stage's default prim.
    #[arg(short, long)]
    name: Option<String>,
    /// Variant selection, as for the exporter. Must match the export the
    /// capture will drive, or the fingerprint and geom count will not.
    #[arg(long = "variant", value_name = "[PRIM:]SET=VALUE")]
    variants: Vec<VariantSelection>,
    /// Length of one loop, in seconds.
    #[arg(long, default_value_t = 8.0)]
    seconds: f64,
    /// Frames per second.
    #[arg(long, default_value_t = 30.0)]
    fps: f64,
    /// Swing per joint, as a fraction of the room it has (≤ 45° / its
    /// travel for a slider) on each side of its rest position.
    #[arg(long, default_value_t = 0.4)]
    amplitude: f64,
    /// Oscillations per loop. Whole numbers loop seamlessly.
    #[arg(long, default_value_t = 2)]
    cycles: u32,
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let out = export(
        &args.model,
        &Options {
            variants: args.variants.clone(),
            ..Options::default()
        },
    )?;
    let doc = &out.sidecar;
    let arm = &out.articulation;
    let name = args
        .name
        .clone()
        .or_else(|| doc.model_name.clone())
        .unwrap_or_else(|| "model".into());

    let motion = Motion {
        seconds: args.seconds,
        fps: args.fps,
        amplitude: args.amplitude,
        cycles: args.cycles,
    };
    let sweep = Sweep::new(arm, motion.amplitude);
    let capture = record(doc, arm, &motion);
    let frames = capture.frames.len();

    std::fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("creating {}", args.out_dir.display()))?;
    let path = args.out_dir.join(format!("{name}.capture.json"));
    std::fs::write(&path, serde_json::to_string(&capture)?)
        .with_context(|| format!("writing {}", path.display()))?;
    println!("{}", path.display());
    if args.verbose {
        println!(
            "  frames     {frames} ({:.1} s at {} fps, looping)",
            args.seconds, args.fps
        );
        println!(
            "  joints     {} moving of {}",
            sweep.moving(),
            arm.joints.len()
        );
        for (j, s) in arm.joints.iter().zip(&sweep.joints) {
            if s.down > 0.0 || s.up > 0.0 {
                let (unit, k) = match j.kind {
                    JointKind::Prismatic { .. } => ("m", 1.0),
                    _ => ("°", 180.0 / std::f64::consts::PI),
                };
                println!(
                    "    {:<24} rest {:>8.3}{unit}  -{:.3} / +{:.3}",
                    j.name,
                    j.rest * k,
                    s.down * k,
                    s.up * k
                );
            }
        }
        for n in &arm.notes {
            println!("  note: {n}");
        }
    }
    Ok(())
}
