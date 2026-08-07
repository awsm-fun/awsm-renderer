//! Reading a compiled `mjModel` out into the frozen sidecar schema.
//!
//! Kept apart from `main.rs` so it can be tested against real models without going
//! through the CLI surface.

use anyhow::{Context, Result};
use awsm_renderer_mujoco_format::sidecar::{
    Body, Geom, GeomKind, Material, Mesh, Sidecar, Site, Source, Tendon,
};
use awsm_renderer_mujoco_sys::{mjtGeom, mjtObj, mjtWrap, Model};
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
    // Evaluate the model's initial configuration once, so each geom can record
    // where it actually starts rather than where it was authored relative to its
    // body. See `Geom::world_pos`.
    let data = model.forward_at_initial_pose()?;
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

    // One synthetic mesh entry per heightfield, appended after the real meshes.
    // The grid is static after compile, so baking it here means nothing
    // downstream — editor, scene format, pose sink — ever learns what a
    // heightfield is.
    for h in 0..model.nhfield() {
        let name = model
            .name(mjtObj::mjOBJ_HFIELD, h)
            .map(str::to_string)
            .or_else(|| Some(format!("hfield {h}")));
        let index = model.nmesh() + h;
        out.meshes.push(Mesh {
            node: Some(crate::mesh::node_name(index, name.as_deref())),
            name,
        });
    }

    for g in 0..model.ngeom() {
        let kind = geom_kind(model, g)?;
        // geom_dataid indexes meshes for mesh geoms and heightfields for hfield
        // geoms — the same field, two meanings, so it is only a mesh reference
        // when the type says so.
        let mesh = match kind {
            GeomKind::Mesh => Some(model.geom_dataid()[g])
                .filter(|id| *id >= 0)
                .map(|id| id as usize),
            // A heightfield geom points at its BAKED mesh. `kind` stays `hfield`
            // so the sidecar remains a faithful record of the compiled model;
            // consumers that only know how to draw meshes just follow `mesh`.
            GeomKind::Hfield => Some(model.geom_dataid()[g])
                .filter(|id| *id >= 0)
                .map(|id| model.nmesh() + id as usize),
            _ => None,
        };
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
            world_pos: read3(data.geom_xpos(), g),
            world_quat: data.geom_world_quat(g),
            mesh,
            material,
            rgba: read4f(model.geom_rgba(), g),
        });
    }

    for st in 0..model.nsite() {
        out.sites.push(Site {
            name: model.name(mjtObj::mjOBJ_SITE, st).map(str::to_string),
            body: model.site_bodyid()[st] as usize,
            group: model.site_group()[st],
            kind: site_kind(model, st)?,
            size: read3(model.site_size(), st),
            pos: read3(model.site_pos(), st),
            quat: read4(model.site_quat(), st),
            world_pos: read3(data.site_xpos(), st),
            world_quat: data.site_world_quat(st),
            material: Some(model.site_matid()[st])
                .filter(|id| *id >= 0)
                .map(|id| id as usize),
            rgba: read4f(model.site_rgba(), st),
        });
    }

    for t in 0..model.ntendon() {
        let adr0 = model.tendon_adr()[t] as usize;
        let num0 = model.tendon_num()[t] as usize;
        // A FIXED tendon (joint coupling) has no path through space, so there is
        // nothing to draw. It still gets an entry, because this vec's index is
        // the MuJoCo tendon id and a stream binds against that; a zero pool is
        // how "not drawable" is spelled.
        let spatial = model.wrap_type()[adr0..adr0 + num0]
            .iter()
            .any(|w| *w != mjtWrap::mjWRAP_JOINT as i32);
        // Two points per wrap object is MuJoCo's own allocation rule for
        // `wrap_xpos`, so it is the true ceiling on this tendon's waypoints.
        let max_waypoints = if spatial { (num0 as u32) * 2 } else { 0 };
        let adr = data.ten_wrapadr()[t] as usize;
        let num = if spatial {
            data.ten_wrapnum()[t] as usize
        } else {
            0
        };
        let xpos = data.wrap_xpos();
        let world_waypoints = (0..num)
            .map(|i| {
                let o = (adr + i) * 3;
                [xpos[o], xpos[o + 1], xpos[o + 2]]
            })
            .collect();
        out.tendons.push(Tendon {
            name: model.name(mjtObj::mjOBJ_TENDON, t).map(str::to_string),
            group: model.tendon_group()[t],
            width: model.tendon_width()[t],
            material: Some(model.tendon_matid()[t])
                .filter(|id| *id >= 0)
                .map(|id| id as usize),
            rgba: read4f(model.tendon_rgba(), t),
            max_waypoints,
            world_waypoints,
        });
    }

    out.validate()
        .map_err(|e| anyhow::anyhow!("built an invalid sidecar: {e}"))?;
    Ok(out)
}

/// A site's draw shape. `site_type` uses the same `mjtGeom` numbering as geoms,
/// but only the primitive kinds are meaningful — a site never has a mesh.
fn site_kind(model: &Model<'_>, s: usize) -> Result<GeomKind> {
    Ok(match model.site_type()[s] {
        0 => GeomKind::Plane,
        2 => GeomKind::Sphere,
        3 => GeomKind::Capsule,
        4 => GeomKind::Ellipsoid,
        5 => GeomKind::Cylinder,
        6 => GeomKind::Box,
        // Sites are drawn, never collided; anything else means MuJoCo grew a
        // shape we do not know how to place, and guessing would misdraw it.
        other => anyhow::bail!("site {s} has unsupported draw type {other}"),
    })
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
