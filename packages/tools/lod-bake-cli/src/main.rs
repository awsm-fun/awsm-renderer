//! `awsm-lod-bake` — offline nanite/LOD pre-processor.
//!
//! Reads a glTF/GLB mesh and writes the same pre-baked assets the editor's
//! export-time bake produces ([`controller::lod_bake`] in the editor crate),
//! but offline and native — so a heavy mesh can be converted ONCE on the
//! command line and then imported into the editor pre-baked, instead of the
//! editor exploding/baking it in-browser.
//!
//! Per mesh node it emits, into `--out`:
//! - `<id>.glb`          — the base (level-0) geometry as a clean single-mesh glb.
//! - `<id>.lod{N}.glb`   — discrete simplified levels (≥ `--lod-min` tris).
//! - `<id>.lod.toml`     — the discrete-LOD manifest.
//! - `<id>.clusters.bin` — the cluster-LOD DAG, JSON (≥ `--cluster-min` tris).
//!
//! The bake itself is pure Rust (no GPU): it reuses `awsm-renderer-lod-bake`
//! (`build_cluster_dag`, `ClusterMesh::from_dag`, `plan_lod_levels`) and
//! `awsm-renderer-glb-export` (`extract_node_mesh_from_bytes`, `write_glb`),
//! the exact crates the editor uses, so output is identical to an in-editor bake.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use awsm_renderer_glb_export::{
    extract_node_mesh_from_bytes, write_glb, ExportNode, GlbScene, MeshData,
};
use awsm_renderer_lod_bake::{
    build_cluster_dag, cluster_mesh_filename, lod_level_filename, lod_manifest_filename,
    plan_lod_levels, ClusterMesh, DagOptions,
};
use clap::Parser;

/// Default discrete-LOD level ratios (level 0 is the base mesh). Mirrors the
/// editor's `LOD_RATIOS`.
const LOD_RATIOS: &[f32] = &[0.5, 0.25, 0.125];

#[derive(Parser, Debug)]
#[command(
    name = "awsm-lod-bake",
    about = "Pre-bake a glTF/GLB mesh into nanite-ready cluster-LOD + discrete-LOD assets (offline)."
)]
struct Args {
    /// Input glTF/GLB file (self-contained: `.glb`, or `.gltf` with embedded /
    /// data-URI buffers — no external `.bin` side-files).
    input: PathBuf,

    /// Output directory for the baked assets (created if missing). Defaults to
    /// `<input-dir>/<input-stem>.nanite/`.
    #[arg(short, long)]
    out: Option<PathBuf>,

    /// Asset id prefix for the emitted filenames. Defaults to the input file
    /// stem. With multiple mesh nodes, each node gets `<id>_node<index>`.
    #[arg(long)]
    id: Option<String>,

    /// Skip the cluster-LOD (nanite) bake.
    #[arg(long)]
    no_clusters: bool,

    /// Skip the discrete-LOD chain bake.
    #[arg(long)]
    no_discrete: bool,

    /// Minimum triangle count to bake a cluster-LOD DAG (mirrors the editor's
    /// `CLUSTER_MIN_TRIANGLES`).
    #[arg(long, default_value_t = 4096)]
    cluster_min: usize,

    /// Minimum triangle count to bake the discrete chain (mirrors the editor's
    /// `LOD_MIN_TRIANGLES`).
    #[arg(long, default_value_t = 512)]
    lod_min: usize,

    /// Bake even when below the floors (forces both `--cluster-min`/`--lod-min`
    /// to 0). Useful for testing small meshes.
    #[arg(long)]
    force: bool,

    /// Write the cluster bake even when it looks DEGENERATE (pathological topology
    /// that didn't cluster well — see the guard in [`bake_one`]). Off by default so
    /// a bad source never ships a useless, oversized `.clusters.bin`.
    #[arg(long)]
    allow_degenerate_clusters: bool,
}

fn main() -> Result<()> {
    let mut args = Args::parse();
    if args.force {
        args.cluster_min = 0;
        args.lod_min = 0;
    }

    let bytes = std::fs::read(&args.input)
        .with_context(|| format!("reading input {}", args.input.display()))?;

    let stem = args
        .id
        .clone()
        .or_else(|| {
            args.input
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .context("could not derive an asset id from the input path; pass --id")?;

    let out_dir = args.out.clone().unwrap_or_else(|| {
        let parent = args.input.parent().unwrap_or_else(|| Path::new("."));
        parent.join(format!("{stem}.nanite"))
    });
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("creating output dir {}", out_dir.display()))?;

    // Enumerate mesh-bearing nodes (one bake per node, like the editor's
    // per-mesh-asset bake).
    let (doc, _buffers, _images) =
        gltf::import_slice(&bytes).context("parsing the input as glTF/GLB")?;
    let mesh_nodes: Vec<u32> = doc
        .nodes()
        .filter(|n| n.mesh().is_some())
        .map(|n| n.index() as u32)
        .collect();
    if mesh_nodes.is_empty() {
        bail!("no mesh-bearing nodes found in {}", args.input.display());
    }
    let multi = mesh_nodes.len() > 1;
    eprintln!(
        "awsm-lod-bake: {} → {} ({} mesh node{})",
        args.input.display(),
        out_dir.display(),
        mesh_nodes.len(),
        if multi { "s" } else { "" }
    );

    let mut total_files = 0usize;
    for node_index in mesh_nodes {
        let asset_id = if multi {
            format!("{stem}_node{node_index}")
        } else {
            stem.clone()
        };
        let Some(mesh) = extract_node_mesh_from_bytes(&bytes, node_index, None) else {
            eprintln!("  node {node_index}: no extractable geometry — skipped");
            continue;
        };
        total_files += bake_one(&out_dir, &asset_id, &mesh, &args)?;
    }

    eprintln!(
        "awsm-lod-bake: wrote {total_files} file(s) to {}",
        out_dir.display()
    );
    Ok(())
}

/// Bake one mesh node's full asset set into `out_dir`. Returns the file count.
fn bake_one(out_dir: &Path, asset_id: &str, mesh: &MeshData, args: &Args) -> Result<usize> {
    let tris = mesh.indices.len() / 3;
    let mut written = 0usize;

    // Base (level-0) geometry as a clean single-mesh glb — the runtime / editor
    // loads `<id>.glb` as level 0 (the discrete manifest references it implicitly).
    let base_glb = write_glb(&GlbScene {
        nodes: vec![ExportNode::new("mesh").with_mesh(mesh.clone())],
        ..Default::default()
    });
    written += write_file(out_dir, &format!("{asset_id}.glb"), &base_glb)?;

    // Cluster-LOD (nanite) DAG.
    if !args.no_clusters && tris >= args.cluster_min {
        let dag = build_cluster_dag(&mesh.positions, &mesh.indices, &DagOptions::default());
        let cm = ClusterMesh::from_dag(
            &dag,
            mesh.positions.clone(),
            mesh.normals.clone().unwrap_or_default(),
            mesh.uvs.first().cloned().unwrap_or_default(),
            mesh.colors.clone().unwrap_or_default(),
        );
        // Degeneracy guard. A healthy DAG averages dozens of tris/cluster and ~2×
        // the source in total (each coarser level ~halves). Pathological source
        // topology (non-manifold / unweldable) can defeat clustering even after the
        // weld-for-adjacency pass → ~1 tri/cluster and a DAG that balloons many× the
        // source, i.e. a huge, useless `.clusters.bin`. Skip writing it (the mesh
        // still gets the discrete chain) unless explicitly allowed.
        let cluster_count = cm.clusters.len();
        let dag_tris = cm.indices.len() / 3;
        let avg_tpc = dag_tris as f32 / cluster_count.max(1) as f32;
        let dag_ratio = dag_tris as f32 / tris.max(1) as f32;
        let degenerate = avg_tpc < 8.0 || dag_ratio > 6.0;
        if degenerate && !args.allow_degenerate_clusters {
            eprintln!(
                "  {asset_id}: ⚠ DEGENERATE clustering ({cluster_count} clusters, \
                 {avg_tpc:.1} tris/cluster, DAG {dag_ratio:.1}× source) — SKIPPING cluster bake \
                 (discrete LOD still emitted). The source topology didn't cluster well \
                 (non-manifold / unweldable?). Re-run with --allow-degenerate-clusters to force."
            );
        } else {
            let bytes = serde_json::to_vec(&cm).context("serializing ClusterMesh")?;
            written += write_file(out_dir, &cluster_mesh_filename(asset_id), &bytes)?;
            eprintln!(
                "  {asset_id}: {tris} tris → cluster DAG: {cluster_count} clusters \
                 ({dag_tris} DAG tris, {avg_tpc:.1} tris/cluster){}",
                if degenerate {
                    " [forced — degenerate]"
                } else {
                    ""
                }
            );
        }
    } else if !args.no_clusters {
        eprintln!(
            "  {asset_id}: {tris} tris < cluster-min {} — no cluster bake",
            args.cluster_min
        );
    }

    // Discrete-LOD chain.
    if !args.no_discrete && tris >= args.lod_min {
        let plan = plan_lod_levels(
            &mesh.positions,
            &mesh.indices,
            LOD_RATIOS,
            args.lod_min.max(1),
        );
        if plan.levels.is_empty() {
            eprintln!("  {asset_id}: no discrete level reduced the triangle count — skipped");
        } else {
            for lvl in &plan.levels {
                let sm = &lvl.mesh;
                let level_mesh = MeshData {
                    positions: sm.gather(&mesh.positions),
                    normals: mesh.normals.as_ref().map(|n| sm.gather(n)),
                    uvs: mesh.uvs.iter().map(|set| sm.gather(set)).collect(),
                    colors: mesh.colors.as_ref().map(|c| sm.gather(c)),
                    indices: sm.indices.clone(),
                };
                let glb = write_glb(&GlbScene {
                    nodes: vec![ExportNode::new("mesh").with_mesh(level_mesh)],
                    ..Default::default()
                });
                written += write_file(out_dir, &lod_level_filename(asset_id, lvl.index), &glb)?;
            }
            let manifest_toml =
                toml::to_string(&plan.manifest()).context("serializing LOD manifest")?;
            written += write_file(
                out_dir,
                &lod_manifest_filename(asset_id),
                manifest_toml.as_bytes(),
            )?;
            eprintln!(
                "  {asset_id}: discrete chain: {} level(s) [{}]",
                plan.levels.len(),
                plan.levels
                    .iter()
                    .map(|l| format!("L{}={}t", l.index, l.mesh.indices.len() / 3))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    } else if !args.no_discrete {
        eprintln!(
            "  {asset_id}: {tris} tris < lod-min {} — no discrete bake",
            args.lod_min
        );
    }

    Ok(written)
}

fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> Result<usize> {
    let path = dir.join(name);
    std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(1)
}
