//! `awsm-renderer-isaac-export` — read an Isaac Sim / Isaac Lab robot (USD) and
//! export what the editor needs to import it: the same `mujoco.json` sidecar +
//! geometry GLB the MuJoCo exporter writes. See `docs/isaac.md`.

use awsm_renderer_isaac_export_cli::{export, Options, VariantSelection};

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "awsm-renderer-isaac-export",
    about = "Export an Isaac Sim / Isaac Lab USD robot as an editor-importable sidecar (+ GLB)",
    version
)]
struct Args {
    /// Root USD file (`.usd`, `.usda`, `.usdc`, `.usdz`). Everything it
    /// references must be reachable on disk, relative to it.
    model: PathBuf,

    /// Output directory. Written as `<name>.mujoco.json` + `<name>.glb`.
    #[arg(short, long, default_value = ".")]
    out_dir: PathBuf,

    /// Base name for the outputs. Defaults to the stage's default prim, falling
    /// back to the input file stem.
    #[arg(short, long)]
    name: Option<String>,

    /// Select a variant: `SET=VALUE` on the default prim, or
    /// `/Prim/Path:SET=VALUE`. Repeatable (e.g. `--variant Mesh=Quality`).
    #[arg(long = "variant", value_name = "[PRIM:]SET=VALUE")]
    variants: Vec<VariantSelection>,

    /// Also ship the geometry of hidden geoms (colliders, guides). Their table
    /// entries are always exported.
    #[arg(long)]
    include_hidden_geometry: bool,

    /// Print a human-readable summary of what was exported.
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let out = export(
        &args.model,
        &Options {
            variants: args.variants.clone(),
            include_hidden_geometry: args.include_hidden_geometry,
        },
    )?;
    let mut doc = out.sidecar;

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

    let glb_path = match &out.glb {
        Some(scene) => {
            let bytes = awsm_renderer_glb_export::write_glb(scene);
            let path = args.out_dir.join(format!("{name}.glb"));
            std::fs::write(&path, &bytes).with_context(|| format!("writing {}", path.display()))?;
            doc.glb = Some(format!("{name}.glb"));
            Some((path, bytes.len()))
        }
        None => None,
    };

    let json_path = args.out_dir.join(format!("{name}.mujoco.json"));
    // Pretty-printed: the sidecar is a documented format people read and diff.
    let json = serde_json::to_string_pretty(&doc)?;
    std::fs::write(&json_path, json).with_context(|| format!("writing {}", json_path.display()))?;

    let report = &out.report;
    if args.verbose {
        println!("{}", json_path.display());
        println!(
            "  model      {}",
            doc.model_name.as_deref().unwrap_or("(unnamed)")
        );
        println!(
            "  source     {} ({})",
            doc.source.filename,
            &doc.source.sha256[..16]
        );
        println!("  variants   {:?}", report.variants);
        println!("  bodies     {} (incl. world)", doc.bodies.len());
        println!(
            "  geoms      {} ({} visible)",
            doc.geoms.len(),
            report.visible_geoms
        );
        println!("  materials  {}", doc.materials.len());
        println!("  meshes     {}", doc.meshes.len());
        match &glb_path {
            Some((p, len)) => println!("  glb        {} ({} KiB)", p.display(), len / 1024),
            None => println!("  glb        (none — no mesh geometry)"),
        }
        for e in &report.composition_errors {
            println!("  composition warning: {e}");
        }
        for n in &report.notes {
            println!("  note: {n}");
        }
    } else {
        println!("{}", json_path.display());
        if let Some((p, _)) = &glb_path {
            println!("{}", p.display());
        }
        if !report.composition_errors.is_empty() || !report.notes.is_empty() {
            eprintln!(
                "{} composition warning(s), {} note(s) — rerun with -v to list them",
                report.composition_errors.len(),
                report.notes.len()
            );
        }
    }
    Ok(())
}
