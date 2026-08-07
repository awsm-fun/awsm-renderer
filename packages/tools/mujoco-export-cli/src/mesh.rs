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
use awsm_renderer_mujoco_sys::Model;

/// Build the geometry library. Returns `None` when the model has no meshes at all
/// (all-primitive models like the DeepMind humanoid) — there is no point writing
/// an empty GLB, and the importer keys off the sidecar's mesh list anyway.
pub fn build(model: &Model<'_>, names: &[Option<String>]) -> Result<Option<GlbScene>> {
    if model.nmesh() == 0 {
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
    Ok(Some(scene))
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
