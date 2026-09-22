//! Export an Isaac Sim / Isaac Lab robot (a USD stage) into the **same seam** the
//! MuJoCo exporter produces: a `<name>.mujoco.json` sidecar plus a geometry-only
//! `<name>.glb`. The editor import, the player bundle, capture baking and the
//! pose sink then work unchanged — see `docs/isaac.md`.
//!
//! ## The mapping
//!
//! | USD | sidecar |
//! |---|---|
//! | prim with `PhysicsRigidBodyAPI` | a **body** (body 0 is a synthetic `world`) |
//! | `Mesh` × material `GeomSubset` | a `mesh` **geom**, its vertices baked into its body's frame |
//! | `Cube` / `Sphere` / `Cylinder` / `Capsule` / `Cone` / `Plane` | a primitive **geom**, with a body-relative offset |
//! | purpose `guide` / `proxy`, or `invisible` | geom group 3 (hidden, like MuJoCo collision geoms) |
//! | bound `OmniPBR` / `OmniSurface` / `UsdPreviewSurface` | a **material** |
//!
//! Every geom's world pose is therefore `body world pose ∘ (geom.pos, geom.quat)`,
//! and for mesh geoms that offset is the identity — so a frame of Isaac Lab
//! **body** poses drives the geoms by a scatter through `geoms[i].body`.
//!
//! Nothing here reaches a runtime crate: this is a native tool, and `openusd`
//! is its dependency alone.

pub mod geometry;
pub mod material;
pub mod primitive;
pub mod stage;

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;

use anyhow::{bail, Context, Result};
use awsm_renderer_glb_export::{ExportNode, GlbScene, MeshData, Trs};
use awsm_renderer_mujoco_format::sidecar::{Body, Geom, GeomKind, Mesh, Sidecar, Source};
use glam::{DMat4, DQuat};
use openusd::sdf;
use openusd::usd::{Prim, PrimPredicate, Stage};
use openusd_schemas::geom::{Imageable, Purpose, Visibility};
use sha2::{Digest, Sha256};

pub use stage::VariantSelection;

/// What goes in the sidecar's `source.mujoco_version` for a USD export.
///
/// The field names the compiler that produced the model; a USD robot had none,
/// and saying so is better than inventing a MuJoCo version. Nothing compares
/// this field — instance matching is by `sha256` alone — so the sidecar format
/// stays exactly as it is.
pub const PRODUCER: &str = "none (USD, awsm-renderer-isaac-export)";

/// Visibility group for geometry USD does not draw by default. MuJoCo's
/// convention is that groups 0–2 are visible, so any higher group is hidden by
/// the importer while keeping its slot in the table.
pub const HIDDEN_GROUP: i32 = 3;

/// Prim types that are geometry. Everything else (lights, cameras, render
/// settings, the `OmniverseKit_*` editor cameras, shaders) is ignored.
const PRIMITIVE_TYPES: &[&str] = &["Cube", "Sphere", "Cylinder", "Capsule", "Cone", "Plane"];

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub variants: Vec<VariantSelection>,
    /// Also ship the GEOMETRY of hidden geoms (colliders, guides). Their table
    /// entries are always exported; by default their meshes are not, since the
    /// importer never draws them and they can outweigh the visuals.
    pub include_hidden_geometry: bool,
}

pub struct Export {
    pub sidecar: Sidecar,
    /// `None` when the stage has no mesh geometry at all.
    pub glb: Option<GlbScene>,
    pub report: Report,
}

#[derive(Debug, Default)]
pub struct Report {
    /// Effective variant selections on the default prim.
    pub variants: Vec<(String, String)>,
    pub composition_errors: Vec<String>,
    /// Anything lossy: flattened or dropped textures, approximated shapes,
    /// unsupported shaders.
    pub notes: Vec<String>,
    pub visible_geoms: usize,
}

/// Export the stage rooted at `root`.
pub fn export(root: &Path, options: &Options) -> Result<Export> {
    let stage = stage::open(root, &options.variants)?;
    let mut report = Report {
        composition_errors: stage
            .composition_errors()
            .iter()
            .map(|e| e.to_string())
            .collect(),
        ..Report::default()
    };

    // Isaac Sim and Isaac Lab are Z-up metres, exactly MuJoCo's convention —
    // which is why the MuJoCo seam fits unchanged. A stage in any other
    // convention would need its sim's pose stream converted too, so refuse it
    // rather than quietly baking half a conversion.
    let (up_axis, meters_per_unit) = stage::stage_units(&stage);
    if up_axis != "Z" || (meters_per_unit - 1.0).abs() > 1e-9 {
        bail!(
            "{}: upAxis={up_axis}, metersPerUnit={meters_per_unit}; only Z-up metre stages \
             (the Isaac Sim convention) are supported",
            root.display()
        );
    }

    let default_prim = stage.default_prim().map(|t| t.as_str().to_string());
    if let Some(d) = &default_prim {
        report.variants = stage
            .prim(format!("/{d}").as_str())?
            .variant_sets()
            .get_all_variant_selections()?;
    }

    // Every composed prim, INCLUDING instance proxies: Isaac assets put every
    // link's visuals under an `instanceable` prim, and a default traversal
    // stops at instances — it would find no meshes at all.
    let mut paths = Vec::new();
    stage.traverse(PrimPredicate::DEFAULT_PROXIES, |p| paths.push(p.clone()))?;

    let mut xforms = stage::WorldXforms::default();
    let mut materials = material::Materials::new(root);
    let mut doc = Sidecar::new(fingerprint(root)?);
    doc.model_name = default_prim.clone();

    // ── bodies ──────────────────────────────────────────────────────────────
    doc.bodies.push(Body {
        name: Some("world".into()),
        parent: 0,
        pos: [0.0; 3],
        quat: [1.0, 0.0, 0.0, 0.0],
    });
    let mut body_of_path: HashMap<sdf::Path, usize> = HashMap::new();
    // Rigid world frame per body (scale dropped: a sim pose has none).
    let mut body_frames: Vec<DMat4> = vec![DMat4::IDENTITY];
    for path in &paths {
        let prim = stage.prim(path.clone())?;
        if !prim.has_api_schema("PhysicsRigidBodyAPI")? {
            continue;
        }
        let frame = rigid(xforms.get(&stage, path)?);
        let parent = nearest_body(path, &body_of_path, false);
        let (_, rot, pos) = (body_frames[parent].inverse() * frame).to_scale_rotation_translation();
        body_of_path.insert(path.clone(), doc.bodies.len());
        body_frames.push(frame);
        doc.bodies.push(Body {
            // The prim NAME, not its path: it is what Isaac Lab reports in
            // `Articulation.data.body_names`, so a harness maps its body order
            // onto this table by name with no path juggling.
            name: path.name().map(str::to_string),
            parent,
            pos: pos.to_array(),
            quat: wxyz(rot),
        });
    }

    // ── geoms ───────────────────────────────────────────────────────────────
    let names = geom_names(&stage, &paths)?;
    let mut glb = GlbScene::default();
    let mut mesh_by_hash: HashMap<u64, usize> = HashMap::new();
    for path in &paths {
        let prim = stage.prim(path.clone())?;
        let Some(ty) = prim.type_name()? else {
            continue;
        };
        let is_mesh = ty.as_str() == "Mesh";
        if !is_mesh && !PRIMITIVE_TYPES.contains(&ty.as_str()) {
            continue;
        }
        let body = nearest_body(path, &body_of_path, true);
        let body_frame = body_frames[body];
        let local = body_frame.inverse() * xforms.get(&stage, path)?;
        let group = group_of(&prim)?;
        let hidden = group == HIDDEN_GROUP;
        let base_name = names.get(path).cloned().unwrap_or_else(|| "geom".into());
        let display_rgba = display_color(&prim);

        if is_mesh {
            if hidden && !options.include_hidden_geometry {
                // Keep the slot (a sim may still address it), ship no geometry.
                doc.geoms.push(Geom {
                    name: Some(base_name),
                    body,
                    group,
                    kind: GeomKind::Mesh,
                    size: [0.0; 3],
                    pos: [0.0; 3],
                    quat: [1.0, 0.0, 0.0, 0.0],
                    world_pos: body_frame.w_axis.truncate().to_array(),
                    world_quat: wxyz(DQuat::from_mat4(&body_frame)),
                    mesh: None,
                    material: None,
                    rgba: display_rgba,
                });
                continue;
            }
            let pieces = geometry::pieces(&stage, &prim, local)
                .with_context(|| format!("reading mesh {path}"))?;
            let split = pieces.len() > 1;
            for piece in pieces {
                let material = materials.bound(&stage, &piece.binding_prim);
                let rgba = material
                    .map(|m| materials.table[m].rgba)
                    .unwrap_or(display_rgba);
                let label = match (
                    split,
                    material.and_then(|m| materials.table[m].name.clone()),
                    &piece.subset,
                ) {
                    (false, _, _) => base_name.clone(),
                    (true, Some(mat), _) => format!("{base_name} ({mat})"),
                    (true, None, Some(sub)) => format!("{base_name} ({sub})"),
                    (true, None, None) => format!("{base_name} (rest)"),
                };
                let mesh = intern_mesh(&mut doc, &mut glb, &mut mesh_by_hash, &label, piece.mesh);
                doc.geoms.push(Geom {
                    name: Some(label),
                    body,
                    group,
                    kind: GeomKind::Mesh,
                    size: [0.0; 3],
                    // Baked: the vertices are already in the body's frame.
                    pos: [0.0; 3],
                    quat: [1.0, 0.0, 0.0, 0.0],
                    world_pos: body_frame.w_axis.truncate().to_array(),
                    world_quat: wxyz(DQuat::from_mat4(&body_frame)),
                    mesh: Some(mesh),
                    material,
                    rgba,
                });
            }
        } else {
            let (scale, rot, pos) = local.to_scale_rotation_translation();
            let Some(shape) = primitive::from_prim(&prim, scale.abs()) else {
                continue;
            };
            if shape.approximate {
                report
                    .notes
                    .push(format!("{path}: {ty} approximated as a cylinder"));
            }
            let quat = rot * shape.axis_rotation;
            let world = body_frame * DMat4::from_rotation_translation(quat, pos);
            let material = materials.bound(&stage, &prim);
            let rgba = material
                .map(|m| materials.table[m].rgba)
                .unwrap_or(display_rgba);
            doc.geoms.push(Geom {
                name: Some(base_name),
                body,
                group,
                kind: shape.kind,
                size: shape.size,
                pos: pos.to_array(),
                quat: wxyz(quat),
                world_pos: world.w_axis.truncate().to_array(),
                world_quat: wxyz(DQuat::from_mat4(&world)),
                mesh: None,
                material,
                rgba,
            });
        }
    }
    doc.materials = std::mem::take(&mut materials.table);
    report.notes.append(&mut materials.notes);
    report.visible_geoms = doc.geoms.iter().filter(|g| g.group < HIDDEN_GROUP).count();

    if doc.geoms.is_empty() {
        bail!(
            "{}: no geometry found (is the payload loaded, and is this a robot asset?)",
            root.display()
        );
    }
    doc.validate()
        .map_err(|e| anyhow::anyhow!("exported sidecar failed validation: {e}"))?;

    let glb = (!glb.nodes.is_empty()).then_some(glb);
    Ok(Export {
        sidecar: doc,
        glb,
        report,
    })
}

/// A readable, unique name per geometry prim: its own name, lengthened with
/// ancestor names only where needed to tell it apart.
///
/// Isaac assets often name every mesh prim the same (ANYmal: 38 prims called
/// `mesh`, under `<link>/visuals/<part>/`), and the name is what the outliner
/// shows, so a bare leaf name would be useless. Prefixing every name with its
/// full path would be unique but unreadable for the common case (the Franka's
/// `panda_link3`).
fn geom_names(stage: &Stage, paths: &[sdf::Path]) -> Result<HashMap<sdf::Path, String>> {
    let mut geometry = Vec::new();
    for path in paths {
        let ty = stage.prim(path.clone())?.type_name()?;
        if ty.is_some_and(|t| t.as_str() == "Mesh" || PRIMITIVE_TYPES.contains(&t.as_str())) {
            // Path segments, leaf last.
            let segments: Vec<String> = path
                .as_str()
                .split('/')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            geometry.push((path.clone(), segments));
        }
    }
    let mut depth = vec![1usize; geometry.len()];
    let label =
        |segments: &[String], d: usize| segments[segments.len().saturating_sub(d)..].join("/");
    loop {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for (i, (_, seg)) in geometry.iter().enumerate() {
            *counts.entry(label(seg, depth[i])).or_default() += 1;
        }
        let mut grew = false;
        for (i, (_, seg)) in geometry.iter().enumerate() {
            if counts[&label(seg, depth[i])] > 1 && depth[i] < seg.len() {
                depth[i] += 1;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    Ok(geometry
        .iter()
        .enumerate()
        .map(|(i, (p, seg))| (p.clone(), label(seg, depth[i])))
        .collect())
}

/// Filename + SHA-256 of the ROOT layer — the file a harness loads, so it can
/// compute the same fingerprint with one hash. Sublayers and referenced files
/// are not folded in: a harness would have to re-implement composition to
/// match, which is exactly the burden the fingerprint exists to avoid.
pub fn fingerprint(root: &Path) -> Result<Source> {
    let bytes = std::fs::read(root).with_context(|| format!("reading {}", root.display()))?;
    Ok(Source {
        filename: root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string(),
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        mujoco_version: PRODUCER.to_string(),
    })
}

/// Name of the GLB node carrying sidecar mesh `index` — same rule as the
/// MuJoCo exporter, index-prefixed because names need not be unique.
pub fn node_name(index: usize, name: &str) -> String {
    format!("mesh_{index}_{name}")
}

/// Register `mesh` in the sidecar + GLB, sharing an identical earlier one.
///
/// Instanced parts baked into their bodies' frames often come out identical
/// (a quadruped's four shins), so byte-equal geometry is stored once.
fn intern_mesh(
    doc: &mut Sidecar,
    glb: &mut GlbScene,
    by_hash: &mut HashMap<u64, usize>,
    label: &str,
    mesh: MeshData,
) -> usize {
    let key = mesh_hash(&mesh);
    if let Some(i) = by_hash.get(&key) {
        return *i;
    }
    let index = doc.meshes.len();
    let node = node_name(index, label);
    doc.meshes.push(Mesh {
        name: Some(label.to_string()),
        node: Some(node.clone()),
    });
    glb.nodes.push(ExportNode {
        name: node,
        transform: Trs::IDENTITY,
        mesh: Some(mesh),
        // Geometry only: materials live in the sidecar, as for MuJoCo.
        material: None,
        ..Default::default()
    });
    by_hash.insert(key, index);
    index
}

fn mesh_hash(m: &MeshData) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in &m.positions {
        p.map(f32::to_bits).hash(&mut h);
    }
    if let Some(n) = &m.normals {
        for v in n {
            v.map(f32::to_bits).hash(&mut h);
        }
    }
    for set in &m.uvs {
        for v in set {
            v.map(f32::to_bits).hash(&mut h);
        }
    }
    m.indices.hash(&mut h);
    h.finish()
}

/// The nearest rigid body at or above `path` (strictly above when `inclusive`
/// is false), or 0 (the world) when none.
fn nearest_body(path: &sdf::Path, bodies: &HashMap<sdf::Path, usize>, inclusive: bool) -> usize {
    let mut p = if inclusive {
        Some(path.clone())
    } else {
        path.parent()
    };
    while let Some(cur) = p {
        if let Some(b) = bodies.get(&cur) {
            return *b;
        }
        if cur.is_abs_root() {
            break;
        }
        p = cur.parent();
    }
    0
}

/// USD's default-drawn purposes are `default` and `render`; `guide` (debug,
/// and Isaac's collision shapes) and `proxy` (a stand-in for `render`) are not.
fn group_of(prim: &Prim) -> Result<i32> {
    let view = stage::AnyPrim(prim.clone());
    let hidden = matches!(view.compute_purpose()?, Purpose::Guide | Purpose::Proxy)
        || matches!(view.compute_visibility()?, Visibility::Invisible);
    Ok(if hidden { HIDDEN_GROUP } else { 0 })
}

fn display_color(prim: &Prim) -> [f32; 4] {
    let rgb = stage::value(&prim.attribute("primvars:displayColor"))
        .and_then(|v| stage::value_vec3(&v))
        .unwrap_or([0.5, 0.5, 0.5]);
    let a = stage::value(&prim.attribute("primvars:displayOpacity"))
        .and_then(|v| match v {
            sdf::Value::FloatVec(f) => f.first().copied().map(f64::from),
            other => stage::value_f64(&other),
        })
        .unwrap_or(1.0);
    [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32, a as f32]
}

/// Drop scale from a world transform: a sim reports rigid poses.
fn rigid(m: DMat4) -> DMat4 {
    let (_, r, t) = m.to_scale_rotation_translation();
    DMat4::from_rotation_translation(r, t)
}

/// glam `[x, y, z, w]` → the sidecar's `[w, x, y, z]`.
fn wxyz(q: DQuat) -> [f64; 4] {
    let q = q.normalize();
    [q.w, q.x, q.y, q.z]
}
