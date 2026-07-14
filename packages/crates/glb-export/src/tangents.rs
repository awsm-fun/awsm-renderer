//! Tangent policy for the glb writer. `write_glb` carries the source's AUTHORED
//! tangents verbatim (they can't be reproduced), but NEVER bakes
//! MikkTSpace-derived ones: the runtime population path (renderer `raw_mesh`)
//! generates those at load via the SAME `awsm-renderer-tangents` crate, gated on
//! whether a bound material samples a normal map — so baking derived tangents
//! here would be byte-identical redundant data.

#[cfg(test)]
mod tests {
    use crate::{write_glb, ExportNode, GlbScene, MeshData};

    /// A mesh with normals + uvs but NO authored tangents carries NO TANGENT
    /// accessor — derived tangents are the runtime's job (computed at load from
    /// the geometry the player actually renders, gated on normal-map usage), so
    /// baking them would be redundant. Only authored tangents ship.
    #[test]
    fn derived_tangents_not_baked() {
        let mesh = MeshData {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: Some(vec![[0.0, 0.0, 1.0]; 3]),
            uvs: vec![vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]],
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
        assert!(
            reader.read_tangents().is_none(),
            "derived tangents must NOT be baked; the runtime generates them at load"
        );
    }

    /// AUTHORED tangents on the node are emitted VERBATIM — even though
    /// normals+uvs are present. Pins the save→reload fix: the clean rig glb
    /// preserves the exact tangent basis a normal map was baked against. The
    /// sentinel values below are deliberately NOT what MikkTSpace would produce.
    #[test]
    fn authored_tangents_emitted_verbatim() {
        let mesh = MeshData {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: Some(vec![[0.0, 0.0, 1.0]; 3]),
            uvs: vec![vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]],
            colors: None,
            indices: vec![0, 1, 2],
        };
        let authored = vec![
            [0.0, 1.0, 0.0, -1.0],
            [0.0, 1.0, 0.0, -1.0],
            [0.0, 1.0, 0.0, -1.0],
        ];
        let mut node = ExportNode::new("t").with_mesh(mesh);
        node.tangents = Some(authored.clone());
        let glb = write_glb(&GlbScene {
            nodes: vec![node],
            ..Default::default()
        });
        let (doc, buffers, _) = gltf::import_slice(&glb).expect("parse");
        let buffers: Vec<Vec<u8>> = buffers.into_iter().map(|b| b.0).collect();
        let prim = doc.meshes().next().unwrap().primitives().next().unwrap();
        let reader = prim.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
        let got: Vec<[f32; 4]> = reader.read_tangents().expect("TANGENT present").collect();
        assert_eq!(
            got, authored,
            "authored tangents must round-trip unmodified"
        );
    }

    /// A mesh with no uvs gets no TANGENT (MikkTSpace needs uvs).
    #[test]
    fn no_uvs_no_tangent() {
        let mesh = MeshData {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: Some(vec![[0.0, 0.0, 1.0]; 3]),
            uvs: vec![],
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
