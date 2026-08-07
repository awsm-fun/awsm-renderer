//! Turning `mjModel`'s mesh pools into a geometry-only GLB.
//!
//! The GLB is a flat **library**: one root node per MuJoCo mesh, in mesh order,
//! identity transform, no materials. It carries geometry and nothing else — every
//! bit of MuJoCo meaning (which geom uses which mesh, at what offset, with what
//! material, in which visibility group) lives in the sidecar. That split is what
//! lets the editor mint OUR materials at import instead of inheriting glTF ones.
//!
//! Vertices come from `mjModel` post-compile, never from the source OBJ/STL: the
//! compiler recentres mesh vertices and folds the difference into the geom's
//! pos/quat, so mixing the two sources would put every visual geom in the wrong
//! place.

use std::collections::HashMap;

use anyhow::{ensure, Result};
use awsm_renderer_glb_export::{ExportNode, GlbScene, MeshData, Trs};
use awsm_renderer_mujoco_sys::{Data, Model};

/// Build the geometry library. Returns `None` when the model has no meshes at all
/// (all-primitive models like the DeepMind humanoid) — there is no point writing
/// an empty GLB, and the importer keys off the sidecar's mesh list anyway.
pub fn build(
    model: &Model<'_>,
    data: &Data<'_, '_>,
    names: &[Option<String>],
) -> Result<Option<GlbScene>> {
    if model.nmesh() == 0 && model.nhfield() == 0 && model.nflex() == 0 {
        return Ok(None);
    }
    let mut scene = GlbScene::default();
    for m in 0..model.nmesh() {
        let mesh = extract(model, m)?;
        scene.nodes.push(ExportNode {
            name: node_name(m, names.get(m).and_then(|n| n.as_deref())),
            transform: Trs::IDENTITY,
            mesh: Some(mesh),
            // No material on purpose — see the module docs.
            material: None,
            ..Default::default()
        });
    }
    // Heightfields are baked to meshes and APPENDED after the real meshes, so a
    // heightfield geom downstream is just a mesh geom. Their sidecar entries are
    // appended in the same order (see the exporter's sidecar builder).
    for h in 0..model.nhfield() {
        let nrow = model.hfield_nrow()[h] as usize;
        let ncol = model.hfield_ncol()[h] as usize;
        let adr = model.hfield_adr()[h] as usize;
        let data = &model.hfield_data()[adr..adr + nrow * ncol];
        let size = [
            model.hfield_size()[h * 4],
            model.hfield_size()[h * 4 + 1],
            model.hfield_size()[h * 4 + 2],
            model.hfield_size()[h * 4 + 3],
        ];
        let index = model.nmesh() + h;
        scene.nodes.push(ExportNode {
            name: node_name(index, names.get(index).and_then(|n| n.as_deref())),
            transform: Trs::IDENTITY,
            mesh: Some(heightfield_mesh(size, nrow, ncol, data)),
            material: None,
            ..Default::default()
        });
    }
    // Flexes are baked at their INITIAL POSE and appended after the
    // heightfields, so a deformable's surface downstream is just another mesh
    // asset. Its vertices are world-space (a flex has no rigid frame of its
    // own), so the node transform stays identity and the importer places it at
    // the origin under the instance root.
    for f in 0..model.nflex() {
        let index = model.nmesh() + model.nhfield() + f;
        scene.nodes.push(ExportNode {
            name: node_name(index, names.get(index).and_then(|n| n.as_deref())),
            transform: Trs::IDENTITY,
            mesh: Some(flex_mesh(model, data, f)),
            material: None,
            ..Default::default()
        });
    }
    Ok(Some(scene))
}

/// Bake one flex's visible surface at the model's initial pose.
///
/// MuJoCo carries no normals for a flex — its own visualizer derives them every
/// frame — so they are computed here. At the bind pose that is exactly right;
/// once the surface deforms they go stale, which is a problem for whatever ends
/// up streaming it, not for this file.
fn flex_mesh(model: &Model<'_>, data: &Data<'_, '_>, f: usize) -> MeshData {
    let dim = model.flex_dim()[f];
    let vertadr = model.flex_vertadr()[f] as usize;
    let vertnum = model.flex_vertnum()[f] as usize;

    // A 2D flex's ELEMENTS are already triangles; a 3D flex's elements are
    // tetrahedra whose visible boundary is the shell. A 1D flex (a rope) has no
    // surface at all.
    let (adr, count, pool) = match dim {
        2 => (
            model.flex_elemdataadr()[f] as usize,
            model.flex_elemnum()[f] as usize,
            model.flex_elem(),
        ),
        3 => (
            model.flex_shelldataadr()[f] as usize,
            model.flex_shellnum()[f] as usize,
            model.flex_shell(),
        ),
        _ => (0, 0, model.flex_elem()),
    };

    // World positions from `mjData`, not `flex_vert`: the latter is each vertex
    // in ITS OWN body's frame, which is only the shape once the bodies are
    // placed. Same reason geoms record world poses.
    let xpos = data.flexvert_xpos();
    let mut mesh = MeshData {
        positions: (0..vertnum)
            .map(|v| {
                let o = (vertadr + v) * 3;
                [xpos[o] as f32, xpos[o + 1] as f32, xpos[o + 2] as f32]
            })
            .collect(),
        indices: (0..count * 3).map(|i| pool[adr + i] as u32).collect(),
        ..Default::default()
    };
    mesh.compute_vertex_normals();

    if model.flex_texcoordadr()[f] >= 0 {
        let tcadr = model.flex_texcoordadr()[f] as usize;
        let tc = model.flex_texcoord();
        mesh.uvs = vec![(0..vertnum)
            .map(|v| {
                let o = (tcadr + v) * 2;
                // MuJoCo's V runs opposite ours, the same flip `extract` applies.
                [tc[o], 1.0 - tc[o + 1]]
            })
            .collect()];
    }
    mesh
}

/// The GLB node name for mesh `i`. Recorded in the sidecar too, so the importer
/// matches by name and never has to reproduce this rule.
pub fn node_name(index: usize, name: Option<&str>) -> String {
    match name {
        // Prefixed with the index because MuJoCo does not guarantee unique mesh
        // names (and glTF node names are not required to be unique either, so a
        // collision would silently bind the wrong geometry).
        Some(n) if !n.is_empty() => format!("mesh_{index}_{n}"),
        _ => format!("mesh_{index}"),
    }
}

/// De-index one MuJoCo mesh into a glTF-shaped mesh.
///
/// MuJoCo stores meshes OBJ-style: separate index arrays for positions, normals
/// and texcoords, so one triangle corner can pull attribute values from three
/// different slots. glTF has a single index per vertex, so each distinct
/// `(position, normal, texcoord)` triple becomes one vertex. Sharing is preserved
/// wherever the triples repeat, which is the common case.
fn extract(model: &Model<'_>, m: usize) -> Result<MeshData> {
    let vert_base = model.mesh_vertadr()[m] as usize;
    let vert_num = model.mesh_vertnum()[m] as usize;
    let face_base = model.mesh_faceadr()[m] as usize;
    let face_num = model.mesh_facenum()[m] as usize;
    let normal_base = model.mesh_normaladr()[m] as usize;
    let normal_num = model.mesh_normalnum()[m] as usize;
    let texcoord_adr = model.mesh_texcoordadr()[m];
    let texcoord_num = model.mesh_texcoordnum()[m] as usize;
    let has_uv = texcoord_adr >= 0 && texcoord_num > 0;
    let texcoord_base = texcoord_adr.max(0) as usize;

    let verts = model.mesh_vert();
    let normals = model.mesh_normal();
    let texcoords = model.mesh_texcoord();
    let faces = model.mesh_face();
    let face_normals = model.mesh_facenormal();
    let face_texcoords = model.mesh_facetexcoord();

    let mut out = MeshData::default();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    // (position, normal, texcoord) triple → emitted vertex index.
    let mut seen: HashMap<(i32, i32, i32), u32> = HashMap::new();

    for f in face_base..face_base + face_num {
        for k in 0..3 {
            let pi = faces[f * 3 + k];
            let ni = face_normals[f * 3 + k];
            let ti = if has_uv {
                face_texcoords[f * 3 + k]
            } else {
                -1
            };

            // Bounds are checked here rather than trusted, because MuJoCo's face
            // indices being mesh-RELATIVE (not absolute into the shared pool) is a
            // convention we read off the library, not a guarantee in the header.
            // If it ever flipped, silently reading a neighbouring mesh's vertices
            // would produce geometry that looks almost right.
            ensure!(
                pi >= 0 && (pi as usize) < vert_num,
                "mesh {m}: face vertex index {pi} out of range for {vert_num} vertices"
            );
            ensure!(
                ni >= 0 && (ni as usize) < normal_num,
                "mesh {m}: face normal index {ni} out of range for {normal_num} normals"
            );
            ensure!(
                !has_uv || (ti >= 0 && (ti as usize) < texcoord_num),
                "mesh {m}: face texcoord index {ti} out of range for {texcoord_num} texcoords"
            );

            let next = out.positions.len() as u32;
            let index = *seen.entry((pi, ni, ti)).or_insert_with(|| {
                let p = (vert_base + pi as usize) * 3;
                out.positions.push([verts[p], verts[p + 1], verts[p + 2]]);
                let n = (normal_base + ni as usize) * 3;
                out.normals.get_or_insert_with(Vec::new).push([
                    normals[n],
                    normals[n + 1],
                    normals[n + 2],
                ]);
                if has_uv {
                    let t = (texcoord_base + ti as usize) * 2;
                    // MuJoCo's V runs bottom-up (OBJ convention); glTF's runs
                    // top-down. Flipping here rather than at import keeps the
                    // GLB a plain, correct glTF that any viewer opens right.
                    uvs.push([texcoords[t], 1.0 - texcoords[t + 1]]);
                }
                next
            });
            out.indices.push(index);
        }
    }

    if has_uv {
        out.uvs.push(uvs);
    }
    Ok(out)
}

/// Tessellate a heightfield into a triangle mesh.
///
/// **Heightfields are baked at export, not at import.** MuJoCo's grid is static
/// after compile, so there is nothing dynamic to preserve — and baking here means
/// the editor, the scene format and the pose sink never learn what a heightfield
/// is. An hfield geom arrives downstream as an ordinary mesh geom.
///
/// The grid spans `[-size.x, +size.x] x [-size.y, +size.y]` in the geom's local
/// frame with elevation `data * size.z`, and MuJoCo draws a solid base extending
/// `size.w` below zero. This emits the top surface plus that skirt, so the
/// terrain reads as solid from a grazing angle instead of as a paper sheet.
pub fn heightfield_mesh(size: [f64; 4], nrow: usize, ncol: usize, data: &[f32]) -> MeshData {
    let (half_x, half_y) = (size[0] as f32, size[1] as f32);
    let (z_scale, z_base) = (size[2] as f32, size[3] as f32);
    let mut mesh = MeshData::default();
    if nrow < 2 || ncol < 2 || data.len() < nrow * ncol {
        return mesh;
    }

    // MuJoCo indexes the grid row-major with rows along +Y and columns along +X.
    let at = |r: usize, c: usize| data[r * ncol + c];
    let pos = |r: usize, c: usize| {
        let u = c as f32 / (ncol - 1) as f32;
        let v = r as f32 / (nrow - 1) as f32;
        [
            -half_x + u * 2.0 * half_x,
            -half_y + v * 2.0 * half_y,
            at(r, c) * z_scale,
        ]
    };

    let mut uvs = Vec::with_capacity(nrow * ncol);
    for r in 0..nrow {
        for c in 0..ncol {
            mesh.positions.push(pos(r, c));
            uvs.push([c as f32 / (ncol - 1) as f32, r as f32 / (nrow - 1) as f32]);
        }
    }
    let idx = |r: usize, c: usize| (r * ncol + c) as u32;
    for r in 0..nrow - 1 {
        for c in 0..ncol - 1 {
            mesh.indices.extend_from_slice(&[
                idx(r, c),
                idx(r, c + 1),
                idx(r + 1, c + 1),
                idx(r, c),
                idx(r + 1, c + 1),
                idx(r + 1, c),
            ]);
        }
    }

    // Skirt: drop each boundary edge to the base plane so the terrain is a solid,
    // not a sheet. Walls are their own vertices — sharing them with the top
    // surface would smear its normals over the fold.
    let wall = |a: [f32; 3], b: [f32; 3], mesh: &mut MeshData, uvs: &mut Vec<[f32; 2]>| {
        let base = mesh.positions.len() as u32;
        let bottom = -z_base;
        for p in [a, b, [b[0], b[1], bottom], [a[0], a[1], bottom]] {
            mesh.positions.push(p);
            uvs.push([0.0, 0.0]);
        }
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };
    for c in 0..ncol - 1 {
        wall(pos(0, c + 1), pos(0, c), &mut mesh, &mut uvs);
        wall(pos(nrow - 1, c), pos(nrow - 1, c + 1), &mut mesh, &mut uvs);
    }
    for r in 0..nrow - 1 {
        wall(pos(r, 0), pos(r + 1, 0), &mut mesh, &mut uvs);
        wall(pos(r + 1, ncol - 1), pos(r, ncol - 1), &mut mesh, &mut uvs);
    }

    mesh.uvs.push(uvs);
    // Area-weighted vertex normals: the grid is generated, so there are no
    // authored normals to carry, and the skirt's separate vertices keep the fold
    // at the rim sharp.
    mesh.compute_vertex_normals();
    mesh
}
