//! Reading a compiled `mjModel` out into the frozen sidecar schema.
//!
//! Kept apart from `main.rs` so it can be tested against real models without going
//! through the CLI surface.

use anyhow::{Context, Result};
use awsm_renderer_mujoco_format::sidecar::{Body, Geom, GeomKind, Material, Mesh, Sidecar, Source};
use awsm_renderer_mujoco_sys::{mjtGeom, mjtObj, Model};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Fingerprint a source model file: filename (never a path — see [`Source`]) plus
/// the SHA-256 of its bytes.
pub fn fingerprint(path: &Path, mujoco_version: String) -> Result<Source> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    Ok(Source {
        filename,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        mujoco_version,
    })
}

/// Read everything the sidecar describes out of a compiled model.
///
/// Nothing is filtered here — invisible groups, collision geoms and unnamed
/// entries all come through with their real indices, because the index *is* the
/// MuJoCo geom id the pose stream addresses. Deciding what to render is the
/// importer's job, and it needs the whole table to do it.
pub fn build(model: &Model<'_>, source: Source) -> Result<Sidecar> {
    let mut out = Sidecar::new(source);
    out.model_name = model.model_name().map(str::to_string);

    for b in 0..model.nbody() {
        out.bodies.push(Body {
            name: model.name(mjtObj::mjOBJ_BODY, b).map(str::to_string),
            parent: model.body_parentid()[b] as usize,
            pos: read3(model.body_pos(), b),
            quat: read4(model.body_quat(), b),
        });
    }

    for m in 0..model.nmat() {
        out.materials.push(Material {
            name: model.name(mjtObj::mjOBJ_MATERIAL, m).map(str::to_string),
            rgba: read4f(model.mat_rgba(), m),
            specular: model.mat_specular()[m],
            shininess: model.mat_shininess()[m],
            reflectance: model.mat_reflectance()[m],
            emission: model.mat_emission()[m],
        });
    }

    for m in 0..model.nmesh() {
        let name = model.name(mjtObj::mjOBJ_MESH, m).map(str::to_string);
        out.meshes.push(Mesh {
            node: Some(crate::mesh::node_name(m, name.as_deref())),
            name,
        });
    }

    for g in 0..model.ngeom() {
        let kind = geom_kind(model, g)?;
        // geom_dataid indexes meshes for mesh geoms and heightfields for hfield
        // geoms — the same field, two meanings, so it is only a mesh reference
        // when the type says so.
        let mesh = (kind == GeomKind::Mesh)
            .then(|| model.geom_dataid()[g])
            .filter(|id| *id >= 0)
            .map(|id| id as usize);
        let material = Some(model.geom_matid()[g])
            .filter(|id| *id >= 0)
            .map(|id| id as usize);

        out.geoms.push(Geom {
            name: model.name(mjtObj::mjOBJ_GEOM, g).map(str::to_string),
            body: model.geom_bodyid()[g] as usize,
            group: model.geom_group()[g],
            kind,
            size: read3(model.geom_size(), g),
            pos: read3(model.geom_pos(), g),
            quat: read4(model.geom_quat(), g),
            mesh,
            material,
            rgba: read4f(model.geom_rgba(), g),
        });
    }

    out.validate()
        .map_err(|e| anyhow::anyhow!("built an invalid sidecar: {e}"))?;
    Ok(out)
}

fn geom_kind(model: &Model<'_>, g: usize) -> Result<GeomKind> {
    let raw = model.geom_type()[g];
    let kind = model
        .geom_kind(g)
        .with_context(|| format!("geom {g} has unknown mjtGeom {raw}"))?;
    Ok(match kind {
        mjtGeom::mjGEOM_PLANE => GeomKind::Plane,
        mjtGeom::mjGEOM_HFIELD => GeomKind::Hfield,
        mjtGeom::mjGEOM_SPHERE => GeomKind::Sphere,
        mjtGeom::mjGEOM_CAPSULE => GeomKind::Capsule,
        mjtGeom::mjGEOM_ELLIPSOID => GeomKind::Ellipsoid,
        mjtGeom::mjGEOM_CYLINDER => GeomKind::Cylinder,
        mjtGeom::mjGEOM_BOX => GeomKind::Box,
        mjtGeom::mjGEOM_MESH => GeomKind::Mesh,
        mjtGeom::mjGEOM_SDF => GeomKind::Sdf,
        // Everything past mjGEOM_SDF is a *visualization* primitive (arrows,
        // labels), never a model geom, so reaching here means mjModel contained
        // something we have no business rendering.
        other => anyhow::bail!("geom {g} has non-model type {other:?}"),
    })
}

fn read3(src: &[f64], i: usize) -> [f64; 3] {
    [src[i * 3], src[i * 3 + 1], src[i * 3 + 2]]
}

fn read4(src: &[f64], i: usize) -> [f64; 4] {
    [src[i * 4], src[i * 4 + 1], src[i * 4 + 2], src[i * 4 + 3]]
}

fn read4f(src: &[f32], i: usize) -> [f32; 4] {
    [src[i * 4], src[i * 4 + 1], src[i * 4 + 2], src[i * 4 + 3]]
}
