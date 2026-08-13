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
use awsm_renderer_glb_export::{
    CageInfluences, ExportNode, ExportSkin, GlbScene, MeshData, SkinInfluenceSet, Trs,
};
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
        let mesh = flex_mesh(model, data, f);
        let vertnum = mesh.positions.len();

        // A flex deforms by linear blend skinning EXACTLY — verified to the
        // nanometre against MuJoCo's own `flexvert_xpos` (tests/flex_skin.rs) —
        // so it ships as a skinned mesh and the renderer needs no per-frame
        // vertex traffic at all.
        let (joint_bodies, influences) = flex_influences(model, f, vertnum);
        let node_index = scene.nodes.len();
        let joint_base = node_index + 1;

        let mesh_node_name = node_name(index, names.get(index).and_then(|n| n.as_deref()));
        let mut node = ExportNode {
            name: mesh_node_name.clone(),
            transform: Trs::IDENTITY,
            mesh: Some(mesh),
            material: None,
            ..Default::default()
        };

        if !joint_bodies.is_empty() && vertnum > 0 {
            node.skin = Some(scene.skins.len());
            match influences {
                // Interpolated flexes ship the COMPACT cage form: 3 coords a
                // vertex instead of 8/27 expanded (joint, weight) pairs. The
                // consumers expand through the one shared `CageInfluences`
                // implementation, so the GLB is transport only.
                FlexInfluences::Cage(cage) => node.cage_influences = Some(cage),
                FlexInfluences::Direct(sets) => {
                    node.joints = Some(sets.first().map(|s| s.joints.clone()).unwrap_or_default());
                    node.weights =
                        Some(sets.first().map(|s| s.weights.clone()).unwrap_or_default());
                    node.extra_influence_sets = sets.into_iter().skip(1).collect();
                }
            }
        }
        scene.nodes.push(node);

        if joint_bodies.is_empty() || vertnum == 0 {
            continue;
        }

        // One glTF node per joint, placed at its body's REST world pose. The
        // inverse of that pose is the inverse-bind matrix, so at the bind pose
        // every joint contributes the identity and the mesh sits exactly where
        // its baked vertices already are.
        let mut ibms = Vec::with_capacity(joint_bodies.len());
        for (j, body) in joint_bodies.iter().enumerate() {
            let (t, r) = body_rest_pose(data, *body);
            scene.nodes.push(ExportNode {
                // Derived from the MESH node's name, which is the only identity
                // the sidecar and the GLB share — the importer finds joints by
                // exactly this rule.
                name: format!("{mesh_node_name}_joint_{j}"),
                transform: Trs {
                    translation: t,
                    rotation: r,
                    scale: [1.0; 3],
                },
                ..Default::default()
            });
            ibms.push(
                glam::Mat4::from_rotation_translation(
                    glam::Quat::from_array(r),
                    glam::Vec3::from(t),
                )
                .inverse()
                .to_cols_array(),
            );
        }
        scene.skins.push(ExportSkin {
            joints: (joint_base..joint_base + joint_bodies.len()).collect(),
            inverse_bind_matrices: ibms,
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

/// A body's rest world pose, as `(translation, rotation)`.
fn body_rest_pose(data: &Data<'_, '_>, body: usize) -> ([f32; 3], [f32; 4]) {
    let xpos = data.xpos();
    let q = &data.xquat()[body * 4..body * 4 + 4];
    (
        [
            xpos[body * 3] as f32,
            xpos[body * 3 + 1] as f32,
            xpos[body * 3 + 2] as f32,
        ],
        // MuJoCo quaternions are [w, x, y, z]; glTF and glam want [x, y, z, w].
        [q[1] as f32, q[2] as f32, q[3] as f32, q[0] as f32],
    )
}

/// A flex's per-vertex skin binding, in whichever shape it ships.
enum FlexInfluences {
    /// Expanded four-influence sets — the body-attached case (one influence at
    /// weight 1 per vertex; the joint list IS the vertex list).
    Direct(Vec<SkinInfluenceSet>),
    /// Compact cage form — the interpolated case. See
    /// [`awsm_renderer_glb_export::CageInfluences`]: 3 normalized coordinates
    /// per vertex reconstruct all 8/27 weights exactly, so the GLB carries 12
    /// bytes a vertex instead of up to 172.
    Cage(CageInfluences),
}

/// The flex's joint bodies and the per-vertex influences that bind to them.
///
/// Two shapes, one mechanism:
///
/// - **body-attached**: each vertex rides its own body, so it has ONE influence
///   at weight 1 and the joint list is the vertex list.
/// - **interpolated**: every vertex is a fixed blend of a regular cage lattice —
///   `2×2×2` corners (trilinear) or `3×3×3` nodes (triquadratic). MuJoCo already
///   stores each vertex's position inside its cell as normalized coordinates in
///   `flex_vert0` — those three numbers ARE the shipped encoding; the tensor
///   product of the 1D Lagrange basis (implemented once, in
///   `glb_export::CageInfluences`) reconstructs every weight exactly at load.
///
/// The lattice index convention is shared between the weight basis and the node
/// lookup ([`cage_lattice`]). That sharing is load-bearing — a convention that
/// disagreed between the two would look right at rest and deform wrongly, which
/// is precisely what `tests/flex_skin.rs` measures.
fn flex_influences(model: &Model<'_>, f: usize, vertnum: usize) -> (Vec<usize>, FlexInfluences) {
    let vertadr = model.flex_vertadr()[f] as usize;

    if model.flex_interp()[f] == 0 {
        let bodies: Vec<usize> = model.flex_vertbodyid()[vertadr..vertadr + vertnum]
            .iter()
            .map(|b| (*b).max(0) as usize)
            .collect();
        let set = SkinInfluenceSet {
            joints: (0..vertnum).map(|v| [v as u16, 0, 0, 0]).collect(),
            weights: vec![[1.0, 0.0, 0.0, 0.0]; vertnum],
        };
        return (bodies, FlexInfluences::Direct(vec![set]));
    }

    let nadr = model.flex_nodeadr()[f] as usize;
    let nnum = model.flex_nodenum()[f] as usize;
    let bodies: Vec<usize> = model.flex_nodebodyid()[nadr..nadr + nnum]
        .iter()
        .map(|b| (*b).max(0) as usize)
        .collect();

    // 8 nodes ⇒ trilinear, 27 ⇒ triquadratic. Anything else is not a regular
    // lattice and has no fixed-weight blend to bake, so it is refused loudly
    // rather than approximated into something that looks nearly right and moves
    // wrongly.
    let order = match nnum {
        8 => 2usize,
        27 => 3,
        _ => {
            eprintln!(
                "warning: flex {f} is interpolated with {nnum} cage nodes, which is neither a \
                 trilinear (8) nor a quadratic (27) lattice, so this flex ships un-deformable"
            );
            return (Vec::new(), FlexInfluences::Direct(Vec::new()));
        }
    };

    let Some(lattice) = cage_lattice(model, f, order) else {
        eprintln!(
            "warning: flex {f}'s {nnum}-node cage is not a regular {order}×{order}×{order} \
             lattice (it is degenerate along at least one axis), so this flex ships \
             un-deformable"
        );
        return (Vec::new(), FlexInfluences::Direct(Vec::new()));
    };

    // `flex_vert0` is the vertex's normalized position inside its cell — in
    // [0,1]³, already computed by MuJoCo. The f64 → f32 narrowing here is the
    // only precision the encoding gives up, and it lands far below the f32
    // GLB's own tolerance (`the_exported_glb_deforms_like_mujoco` pins it).
    let vert0 = model.flex_vert0();
    let coords: Vec<[f32; 3]> = (0..vertnum)
        .map(|v| {
            let o = (vertadr + v) * 3;
            [vert0[o] as f32, vert0[o + 1] as f32, vert0[o + 2] as f32]
        })
        .collect();
    let cage = CageInfluences {
        order,
        joints: lattice.iter().map(|n| *n as u16).collect(),
        coords,
    };

    // Which form SHIPS is a measured size call, not a principle (both load
    // everywhere; the runtime buffers are identical):
    //
    // - **quadratic** ships the cage: its Lagrange weights go negative, so the
    //   expanded form can never quantize and costs 112 f32 bytes a vertex —
    //   bunny_quadratic shipped 259.5 KB expanded vs ~51 KB as a cage.
    // - **trilinear** ships EXPANDED: its weights sit in [0,1], so the bundle
    //   compressor packs them to unorm8 + u8 joints (16 low-entropy bytes a
    //   vertex that meshopt loves), which measured SMALLER than 12 bytes of
    //   high-entropy f32 coords — bunny 41.1 KB expanded vs 50.7 KB as a cage.
    //   (Quantizing the coords instead would sail too close to the 1e-5 m
    //   deformation oracle to trust.)
    //
    // Both expansions are the same shared `CageInfluences` math, so the two
    // forms cannot drift apart.
    if order == 3 {
        (bodies, FlexInfluences::Cage(cage))
    } else {
        let (joints, weights, extra) = cage.expand_sets();
        let mut sets = vec![SkinInfluenceSet { joints, weights }];
        sets.extend(extra);
        (bodies, FlexInfluences::Direct(sets))
    }
}

/// Which cage node sits at each lattice position, indexed `(i*order + j)*order + k`
/// where `i`/`j`/`k` are the bins along axes x/y/z — the same order the
/// `CageInfluences` weight basis emits.
///
/// Derived from the rest positions rather than assumed: MuJoCo promises no node
/// ordering, and an assumed one produces a mesh that looks right at rest and
/// turns inside out the moment it moves.
///
/// Returns `None` if the nodes do not land on a full lattice — a cage flattened
/// along an axis collapses two bins into one and leaves holes, and a silently
/// wrong node map is exactly the failure mode worth refusing.
fn cage_lattice(model: &Model<'_>, f: usize, order: usize) -> Option<Vec<usize>> {
    let adr = model.flex_nodeadr()[f] as usize;
    let num = order.pow(3);
    let node0 = model.flex_node0();
    let at = |n: usize| {
        let o = (adr + n) * 3;
        [node0[o], node0[o + 1], node0[o + 2]]
    };
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for n in 0..num {
        let p = at(n);
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let mut lattice = vec![usize::MAX; num];
    for n in 0..num {
        let p = at(n);
        let mut idx = 0usize;
        for k in 0..3 {
            let span = hi[k] - lo[k];
            if span <= 0.0 {
                return None;
            }
            // Nodes sit on an even grid, so the normalized coordinate lands on
            // a bin centre and rounding names it.
            let bin = (((p[k] - lo[k]) / span) * (order - 1) as f64).round() as usize;
            idx = idx * order + bin.min(order - 1);
        }
        lattice[idx] = n;
    }
    lattice.iter().all(|n| *n != usize::MAX).then_some(lattice)
}
