//! MikkTSpace tangent generation over plain geometry arrays — baked into the
//! exported / canonical glb so the population path never has to generate them
//! (and the pure-data conversion proptests cover tangent generation).
//!
//! This mirrors the adapter in `renderer/src/raw_mesh.rs` (and `renderer-gltf`'s
//! `ensure_tangents`), but operates on `[f32; N]` slices with no renderer deps.
//! Consolidating the three into one shared home is a follow-on (see the plan
//! doc); for now each lives where it's used.

/// Per-vertex MikkTSpace tangents (`vec4`: xyz + handedness `w`), one per vertex,
/// or `None` when inputs are unusable (mismatched lengths, < 1 triangle, or
/// MikkTSpace fails). Generated whenever positions+normals+uvs+indices are
/// present — at export/convert time we don't know whether a normal map will be
/// bound, and a baked tangent on a non-normal-mapped mesh is harmless.
pub(crate) fn generate_tangents(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    uvs: &[[f32; 2]],
    indices: &[u32],
) -> Option<Vec<[f32; 4]>> {
    let vcount = positions.len();
    if vcount == 0
        || normals.len() != vcount
        || uvs.len() != vcount
        || indices.len() < 3
        || indices.iter().any(|&i| i as usize >= vcount)
    {
        return None;
    }
    let triangles: Vec<[usize; 3]> = indices
        .chunks_exact(3)
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    if triangles.is_empty() {
        return None;
    }
    let mut geo = TangentGeometry::new(positions, normals, uvs, &triangles);
    if !bevy_mikktspace::generate_tangents(&mut geo) {
        return None;
    }
    Some(geo.finalize())
}

struct TangentGeometry<'a> {
    positions: &'a [[f32; 3]],
    normals: &'a [[f32; 3]],
    uvs: &'a [[f32; 2]],
    triangles: &'a [[usize; 3]],
    tangent_sum: Vec<[f32; 3]>,
    tangent_sign_sum: Vec<f32>,
    sign_pos: Vec<u32>,
    sign_neg: Vec<u32>,
    count: Vec<u32>,
}

impl<'a> TangentGeometry<'a> {
    fn new(
        positions: &'a [[f32; 3]],
        normals: &'a [[f32; 3]],
        uvs: &'a [[f32; 2]],
        triangles: &'a [[usize; 3]],
    ) -> Self {
        let n = positions.len();
        Self {
            positions,
            normals,
            uvs,
            triangles,
            tangent_sum: vec![[0.0; 3]; n],
            tangent_sign_sum: vec![0.0; n],
            sign_pos: vec![0; n],
            sign_neg: vec![0; n],
            count: vec![0; n],
        }
    }

    fn finalize(&self) -> Vec<[f32; 4]> {
        let mut out = Vec::with_capacity(self.tangent_sum.len());
        for v in 0..self.tangent_sum.len() {
            if self.count[v] == 0 {
                out.push([1.0, 0.0, 0.0, 1.0]);
                continue;
            }
            let mut tangent = normalize_or_fallback(self.tangent_sum[v], self.normals[v]);
            if !tangent.iter().all(|x| x.is_finite()) {
                tangent = [1.0, 0.0, 0.0];
            }
            let sign_sum = self.tangent_sign_sum[v];
            const SIGN_EPSILON: f32 = 1e-4;
            let sign = if !sign_sum.is_finite() {
                1.0
            } else if sign_sum.abs() >= SIGN_EPSILON {
                if sign_sum > 0.0 {
                    1.0
                } else {
                    -1.0
                }
            } else if self.sign_pos[v] >= self.sign_neg[v] {
                1.0
            } else {
                -1.0
            };
            out.push([tangent[0], tangent[1], tangent[2], sign]);
        }
        out
    }
}

impl bevy_mikktspace::Geometry for TangentGeometry<'_> {
    fn num_faces(&self) -> usize {
        self.triangles.len()
    }
    fn num_vertices_of_face(&self, _face: usize) -> usize {
        3
    }
    fn position(&self, face: usize, vert: usize) -> [f32; 3] {
        self.positions[self.triangles[face][vert]]
    }
    fn normal(&self, face: usize, vert: usize) -> [f32; 3] {
        self.normals[self.triangles[face][vert]]
    }
    fn tex_coord(&self, face: usize, vert: usize) -> [f32; 2] {
        self.uvs[self.triangles[face][vert]]
    }
    fn set_tangent_encoded(&mut self, tangent: [f32; 4], face: usize, vert: usize) {
        let v = self.triangles[face][vert];
        self.tangent_sum[v][0] += tangent[0];
        self.tangent_sum[v][1] += tangent[1];
        self.tangent_sum[v][2] += tangent[2];
        self.tangent_sign_sum[v] += tangent[3];
        if tangent[3] > 0.0 {
            self.sign_pos[v] += 1;
        } else if tangent[3] < 0.0 {
            self.sign_neg[v] += 1;
        }
        self.count[v] += 1;
    }
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len_sq = dot3(v, v);
    if len_sq > 1e-20 {
        let inv = len_sq.sqrt().recip();
        [v[0] * inv, v[1] * inv, v[2] * inv]
    } else {
        [0.0, 0.0, 0.0]
    }
}
fn canonical_tangent_from_normal(normal: [f32; 3]) -> [f32; 3] {
    let n = normalize3(normal);
    let axis = if n[1].abs() < 0.999 {
        [0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let t = normalize3(cross3(axis, n));
    if dot3(t, t) > 0.0 {
        t
    } else {
        [1.0, 0.0, 0.0]
    }
}
fn normalize_or_fallback(v: [f32; 3], normal: [f32; 3]) -> [f32; 3] {
    let n = normalize3(normal);
    let proj = dot3(v, n);
    let v_ortho = [v[0] - n[0] * proj, v[1] - n[1] * proj, v[2] - n[2] * proj];
    let t = normalize3(v_ortho);
    if dot3(t, t) > 0.0 {
        t
    } else {
        canonical_tangent_from_normal(normal)
    }
}

#[cfg(test)]
mod tests {
    use crate::{write_glb, ExportNode, GlbScene, MeshData};

    /// A mesh with normals + uvs round-trips through write_glb carrying a TANGENT
    /// accessor (vec4 f32, one per vertex) — so population skips generation.
    #[test]
    fn write_glb_bakes_tangent_accessor() {
        // A single triangle with normals + uvs (enough for MikkTSpace).
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
        let prim = doc
            .meshes()
            .next()
            .unwrap()
            .primitives()
            .next()
            .unwrap();
        let reader = prim.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
        let tangents: Vec<[f32; 4]> = reader.read_tangents().expect("TANGENT present").collect();
        assert_eq!(tangents.len(), 3, "one tangent per vertex");
        // Handedness must be ±1, xyz roughly unit.
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
