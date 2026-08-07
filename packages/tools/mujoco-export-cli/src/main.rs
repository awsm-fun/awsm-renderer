//! `awsm-renderer-mujoco-export` — compile a MuJoCo model with MuJoCo's own
//! compiler and export what the editor needs to import it.
//!
//! We never parse MJCF/URDF/`.mjb`. `libmujoco` does all of it (defaults
//! inheritance, procedural textures, mesh fitting) and we read the compiled
//! `mjModel`; see `docs/plans/mujoco.md`.
//!
//! Needs a local MuJoCo install — see `packages/crates/mujoco-sys/README.md`.

use awsm_renderer_mujoco_export_cli::{mesh, sidecar};

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "awsm-renderer-mujoco-export",
    about = "Export a MuJoCo model (MJCF/URDF/mjb) as an editor-importable sidecar (+ GLB)",
    version
)]
struct Args {
    /// Model to compile: `.xml` (MJCF), `.urdf`, or a version-locked `.mjb`.
    model: PathBuf,

    /// Output directory. Written as `<name>.mujoco.json` (+ `<name>.glb` once the
    /// mesh path lands).
    #[arg(short, long, default_value = ".")]
    out_dir: PathBuf,

    /// Base name for the outputs. Defaults to the compiled model's own name,
    /// falling back to the input file stem.
    #[arg(short, long)]
    name: Option<String>,

    /// Print a human-readable summary of what was exported.
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let lib = awsm_renderer_mujoco_sys::Library::load()?;
    let model = lib.load_model(&args.model)?;

    let source = sidecar::fingerprint(&args.model, lib.version_string())?;
    let mut doc = sidecar::build(&model, source)?;

    let name = args
        .name
        .or_else(|| doc.model_name.clone())
        .or_else(|| {
            args.model
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "model".to_string());

    std::fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("creating {}", args.out_dir.display()))?;

    // Geometry first: an all-primitive model produces no GLB at all, and the
    // sidecar's mesh list is what says so.
    let mesh_names: Vec<_> = doc.meshes.iter().map(|m| m.name.clone()).collect();
    let glb_path = match mesh::build(&model, &mesh_names)? {
        Some(scene) => {
            let bytes = awsm_renderer_glb_export::write_glb(&scene);
            let path = args.out_dir.join(format!("{name}.glb"));
            std::fs::write(&path, &bytes).with_context(|| format!("writing {}", path.display()))?;
            doc.glb = Some(format!("{name}.glb"));
            Some((path, bytes.len()))
        }
        None => None,
    };

    let out = args.out_dir.join(format!("{name}.mujoco.json"));
    // Pretty-printed on purpose: the sidecar is a documented interchange format
    // people are expected to read, diff and hand-edit.
    let json = serde_json::to_string_pretty(&doc)?;
    std::fs::write(&out, json).with_context(|| format!("writing {}", out.display()))?;

    if args.verbose {
        let visible = doc
            .geoms
            .iter()
            .filter(|g| (0..=2).contains(&g.group))
            .count();
        println!("{}", out.display());
        println!(
            "  model      {}",
            doc.model_name.as_deref().unwrap_or("(unnamed)")
        );
        println!(
            "  source     {} ({})",
            doc.source.filename,
            &doc.source.sha256[..16]
        );
        println!("  mujoco     {}", doc.source.mujoco_version);
        println!("  bodies     {}", doc.bodies.len());
        println!(
            "  geoms      {} ({visible} in visible groups 0-2)",
            doc.geoms.len()
        );
        println!("  materials  {}", doc.materials.len());
        println!("  meshes     {}", doc.meshes.len());
        match &glb_path {
            Some((p, len)) => println!("  glb        {} ({} KiB)", p.display(), len / 1024),
            None => println!("  glb        (none — model has no meshes)"),
        }
    } else {
        println!("{}", out.display());
        if let Some((p, _)) = &glb_path {
            println!("{}", p.display());
        }
    }

    Ok(())
}
