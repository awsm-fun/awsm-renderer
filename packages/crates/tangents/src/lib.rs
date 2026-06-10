//! MikkTSpace tangent generation over plain geometry arrays.
//!
//! One pure-CPU implementation shared by every caller that bakes tangents:
//! - the renderer's raw-mesh upload path (`renderer::raw_mesh`), and
//! - the glb exporter/converter (`glb-export::write_glb`, via `gltf-convert`).
//!
//! (The `renderer-gltf` populate path has its own byte-buffer variant tuned to
//! its attribute-map representation; folding it in here is a follow-on.)
//!
//! Produces per-vertex `vec4` tangents (xyz direction + handedness `w`), matching
//! the glTF `TANGENT` convention. Living in a tiny dependency-light crate lets
//! the wasm-only renderer reuse it AND get native test coverage of the algorithm.

/// Per-vertex MikkTSpace tangents (`vec4`: xyz + handedness `w`), one per vertex,
/// or `None` when inputs are unusable (mismatched lengths, fewer than one
/// triangle, an out-of-range index, or MikkTSpace failing).
pub fn generate_tangents(
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

/// MikkTSpace [`bevy_mikktspace::Geometry`] adapter over plain attribute slices.
/// Accumulates the per-corner tangents MikkTSpace emits into per-vertex sums (UV
/// charts that meet at a shared vertex can emit differing tangents) and resolves
/// a deterministic per-vertex basis in [`TangentGeometry::finalize`].
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
    use super::*;

    #[test]
    fn generates_tangent_for_a_triangle() {
        // A flat triangle in the XY plane (+Z normal) with a standard UV layout —
        // the tangent should run along +X with finite, ~unit length and ±1 sign.
        let positions = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let normals = [[0.0, 0.0, 1.0]; 3];
        let uvs = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        let indices = [0u32, 1, 2];
        let t = generate_tangents(&positions, &normals, &uvs, &indices).expect("tangents");
        assert_eq!(t.len(), 3);
        for v in &t {
            assert!((v[3].abs() - 1.0).abs() < 1e-3, "w is ±1");
            let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-2, "xyz ~ unit");
            // +X-ish tangent for this UV layout.
            assert!(v[0] > 0.5, "tangent runs along +X");
        }
    }

    #[test]
    fn rejects_bad_inputs() {
        assert!(generate_tangents(&[], &[], &[], &[]).is_none());
        // Mismatched normals length.
        assert!(generate_tangents(&[[0.0; 3]], &[], &[[0.0; 2]], &[0, 0, 0]).is_none());
        // Out-of-range index.
        let p = [[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let n = [[0.0, 0.0, 1.0]; 3];
        let u = [[0.0; 2]; 3];
        assert!(generate_tangents(&p, &n, &u, &[0, 1, 9]).is_none());
    }
}
