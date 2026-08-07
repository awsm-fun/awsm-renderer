//! Exports a real mesh-bearing model and reads the GLB **back** with the same
//! glTF reader the renderer imports with. A writer that emits bytes nobody can
//! load is the failure this exists to catch.

use std::path::PathBuf;

use awsm_renderer_mujoco_export_cli::{mesh, sidecar};
use awsm_renderer_mujoco_format::sidecar::Sidecar;

struct Exported {
    doc: Sidecar,
    glb: Vec<u8>,
}

fn export(rel: &str) -> Option<Exported> {
    let base = std::env::var_os("MUJOCO_MENAGERIE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            let home =
                PathBuf::from(std::env::var_os("HOME")?).join(".local/share/mujoco_menagerie");
            home.is_dir().then_some(home)
        })?;
    let path = base.join(rel);
    if !path.exists() {
        eprintln!("SKIP: no model at {}", path.display());
        return None;
    }
    let lib = match awsm_renderer_mujoco_sys::Library::load() {
        Ok(lib) => lib,
        Err(e) => {
            assert!(
                std::env::var_os("MUJOCO_DIR").is_none(),
                "MUJOCO_DIR is set, so this must work: {e}"
            );
            eprintln!("SKIP: {e}");
            return None;
        }
    };
    let model = lib.load_model(&path).expect("model should compile");
    let source = sidecar::fingerprint(&path, lib.version_string()).unwrap();
    let doc = sidecar::build(&model, source).unwrap();
    let names: Vec<_> = doc.meshes.iter().map(|m| m.name.clone()).collect();
    let scene = mesh::build(&model, &names)
        .unwrap()
        .expect("go2 has meshes");
    let glb = awsm_renderer_glb_export::write_glb(&scene);
    Some(Exported { doc, glb })
}

#[test]
fn the_glb_reads_back_and_matches_the_sidecar() {
    let Some(Exported { doc, glb }) = export("unitree_go2/go2.xml") else {
        return;
    };

    let (document, buffers, _images) = gltf::import_slice(&glb).expect("emitted a loadable glTF");

    // One root node per MuJoCo mesh, in mesh order, and the sidecar's `node`
    // field names each one — that correspondence IS the binding.
    let nodes: Vec<_> = document.nodes().collect();
    assert_eq!(nodes.len(), doc.meshes.len(), "one node per mesh");
    for (i, m) in doc.meshes.iter().enumerate() {
        assert_eq!(
            nodes[i].name(),
            m.node.as_deref(),
            "sidecar mesh {i} names a node the GLB does not have under that name"
        );
        assert!(nodes[i].mesh().is_some(), "node {i} carries no mesh");
    }

    let mut total_tris = 0usize;
    let mut min = [f32::MAX; 3];
    let mut max = [f32::MIN; 3];

    for node in &nodes {
        let m = node.mesh().unwrap();
        let prims: Vec<_> = m.primitives().collect();
        assert_eq!(prims.len(), 1, "geometry-only library: one primitive each");
        let prim = &prims[0];

        // Materials live in the sidecar and are minted at import, so the GLB must
        // carry none — otherwise a re-import would silently inherit glTF ones.
        assert!(
            prim.material().index().is_none(),
            "GLB must be geometry-only"
        );

        let reader = prim.reader(|b| Some(&buffers[b.index()]));
        let positions: Vec<_> = reader.read_positions().expect("POSITION").collect();
        let normals: Vec<_> = reader.read_normals().expect("NORMAL").collect();
        let indices: Vec<_> = reader.read_indices().expect("indices").into_u32().collect();

        assert_eq!(normals.len(), positions.len());
        assert_eq!(indices.len() % 3, 0);
        total_tris += indices.len() / 3;
        assert!(
            indices.iter().all(|i| (*i as usize) < positions.len()),
            "index out of range after de-indexing"
        );

        // Normals must be unit — MuJoCo's are, and de-indexing must not have
        // paired a position with the wrong normal slot.
        for n in &normals {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-3, "non-unit normal {n:?}");
        }

        for p in &positions {
            assert!(p.iter().all(|v| v.is_finite()));
            for a in 0..3 {
                min[a] = min[a].min(p[a]);
                max[a] = max[a].max(p[a]);
            }
        }
    }

    // Sanity on scale rather than exact counts: MuJoCo's mesh parts are recentred
    // per mesh, so every part sits near its own origin, and the widest span is a
    // Go2 body panel — decimetres, not millimetres or metres.
    let span = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    assert!(
        span.iter().all(|s| *s > 0.01 && *s < 2.0),
        "implausible model extent {span:?} — units are probably wrong"
    );
    assert!(
        total_tris > 1000,
        "only {total_tris} triangles for a Go2 — geometry is missing"
    );
}

#[test]
fn de_indexing_shares_vertices_instead_of_exploding_them() {
    let Some(Exported { doc: _, glb }) = export("unitree_go2/go2.xml") else {
        return;
    };
    let (document, buffers, _) = gltf::import_slice(&glb).unwrap();

    let mut verts = 0usize;
    let mut tris = 0usize;
    for node in document.nodes() {
        let prim = node.mesh().unwrap().primitives().next().unwrap();
        let reader = prim.reader(|b| Some(&buffers[b.index()]));
        verts += reader.read_positions().unwrap().count();
        tris += reader.read_indices().unwrap().into_u32().count() / 3;
    }
    // The naive de-index (three fresh vertices per triangle) would give exactly
    // 3 x tris. Real shared geometry lands well under that; anything at or near
    // the ceiling means the (pos, normal, uv) dedup key is not matching.
    assert!(
        verts < tris * 2,
        "de-indexing shared nothing: {verts} vertices for {tris} triangles"
    );
}

/// Pins down how MuJoCo frames mesh assets — the fact the whole geom-placement
/// path rests on, and the reason the GLB's vertices must come from `mjModel` and
/// never from the source OBJ.
///
/// The compiler recentres each mesh on its own frame and folds the difference
/// into the geom's `pos`/`quat`. So every mesh in the library sits at its own
/// origin (which is why they all pile up when rendered untransformed), and the
/// geoms carry decimetre-scale offsets that put them back where the robot is.
/// Mixing the two sources — original vertices with compiled geom poses, or the
/// reverse — puts every visual part in the wrong place.
#[test]
fn meshes_are_recentred_and_geoms_carry_the_offset() {
    let Some(Exported { doc, glb }) = export("unitree_go2/go2.xml") else {
        return;
    };
    let (document, buffers, _) = gltf::import_slice(&glb).unwrap();
    let mut total_verts = 0usize;
    let mut total_tris = 0usize;
    let mut worst_centroid = 0.0f64;
    let mut biggest_geom_offset = 0.0f64;

    for (i, node) in document.nodes().enumerate() {
        let prim = node.mesh().unwrap().primitives().next().unwrap();
        let reader = prim.reader(|b| Some(&buffers[b.index()]));
        let ps: Vec<_> = reader.read_positions().unwrap().collect();
        let mut c = [0.0f64; 3];
        for p in &ps {
            for a in 0..3 {
                c[a] += p[a] as f64;
            }
        }
        for v in &mut c {
            *v /= ps.len() as f64;
        }
        let users: Vec<_> = doc
            .geoms
            .iter()
            .filter(|g| g.mesh == Some(i))
            .map(|g| g.pos)
            .collect();
        eprintln!(
            "mesh {i:2} {:24} verts {:6} centroid [{:+.4} {:+.4} {:+.4}]  geom_pos {:?}",
            doc.meshes[i].name.as_deref().unwrap_or("-"),
            ps.len(),
            c[0],
            c[1],
            c[2],
            users.first()
        );
        total_verts += ps.len();
        total_tris += reader.read_indices().unwrap().into_u32().count() / 3;
        worst_centroid = worst_centroid.max(c.iter().fold(0.0f64, |m, v| m.max(v.abs())));
        for p in &users {
            biggest_geom_offset =
                biggest_geom_offset.max(p.iter().fold(0.0f64, |m, v| m.max(v.abs())));
        }
    }
    eprintln!(
        "TOTAL {total_verts} verts, {total_tris} tris across {} meshes",
        doc.meshes.len()
    );

    assert!(
        worst_centroid < 0.10,
        "a mesh is {worst_centroid:.3}m off its own origin — vertices do not look recentred"
    );
    assert!(
        biggest_geom_offset > 0.10,
        "no geom offset exceeds {biggest_geom_offset:.3}m — the compensating transform is missing, \
         so these vertices are probably NOT the compiled ones"
    );
    // Not a decimated proxy: the full Go2 visual set.
    assert!(total_tris > 100_000, "only {total_tris} triangles");
}
