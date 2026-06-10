//! Tangent baking for the glb writer. The MikkTSpace generation lives in the
//! shared `awsm-tangents` crate (also used by the renderer's raw-mesh path);
//! `write_glb` calls it to emit a `TANGENT` accessor from normals+uvs so the
//! exported/canonical glb is self-contained and population is a dumb upload.

pub(crate) use awsm_tangents::generate_tangents;

#[cfg(test)]
mod tests {
    use crate::{write_glb, ExportNode, GlbScene, MeshData};

    /// A mesh with normals + uvs round-trips through write_glb carrying a TANGENT
    /// accessor (vec4 f32, one per vertex) — so population skips generation.
    #[test]
    fn write_glb_bakes_tangent_accessor() {
        let mesh = MeshData {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: Some(vec![[0.0, 0.0, 1.0]; 3]),
            uvs: Some(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            colors: None,
            indices: vec![0, 1, 2],
        };
        let glb = write_glb(&GlbScene {
            nodes: vec![ExportNode::new("t").with_mesh(mesh)],
            ..Default::default()
        });
        let (doc, buffers, _) = gltf::import_slice(&glb).expect("parse");
        let buffers: Vec<Vec<u8>> = buffers.into_iter().map(|b| b.0).collect();
        let prim = doc.meshes().next().unwrap().primitives().next().unwrap();
        let reader = prim.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
        let tangents: Vec<[f32; 4]> = reader.read_tangents().expect("TANGENT present").collect();
        assert_eq!(tangents.len(), 3, "one tangent per vertex");
        for t in &tangents {
            assert!((t[3].abs() - 1.0).abs() < 1e-3, "w is ±1");
            let len = (t[0] * t[0] + t[1] * t[1] + t[2] * t[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-2, "xyz ~ unit");
        }
    }

    /// A mesh with no uvs gets no TANGENT (MikkTSpace needs uvs).
    #[test]
    fn no_uvs_no_tangent() {
        let mesh = MeshData {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: Some(vec![[0.0, 0.0, 1.0]; 3]),
            uvs: None,
            colors: None,
            indices: vec![0, 1, 2],
        };
        let glb = write_glb(&GlbScene {
            nodes: vec![ExportNode::new("t").with_mesh(mesh)],
            ..Default::default()
        });
        let (doc, buffers, _) = gltf::import_slice(&glb).unwrap();
        let buffers: Vec<Vec<u8>> = buffers.into_iter().map(|b| b.0).collect();
        let prim = doc.meshes().next().unwrap().primitives().next().unwrap();
        let reader = prim.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
        assert!(reader.read_tangents().is_none());
    }
}
