//! Raw per-vertex editing primitives (Phase 4 core): soft (falloff) transforms
//! and predicate-based vertex selection. Pure functions over [`MeshData`] —
//! natively unit-tested — so the editor's `SoftTransformVertices` /
//! `select_vertices_where` commands are thin wrappers (resolve mesh bytes → call
//! these → store the sparse diff).

use glam::Vec3;

use crate::mesh_data::MeshData;

/// Comparison for [`select_by_axis`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Greater,
    Less,
}

/// Translate the `selected` vertices by `translation`, dragging nearby vertices
/// with a smooth falloff: a vertex within `falloff` of the nearest selected
/// vertex moves by `translation * w`, where `w` smoothsteps 1→0 over the radius
/// (selected vertices move fully). `falloff <= 0` ⇒ a hard move of exactly the
/// selection. Returns the **full** new position array (the caller diffs it).
pub fn soft_transform_positions(
    mesh: &MeshData,
    selected: &[u32],
    translation: [f32; 3],
    falloff: f32,
) -> Vec<[f32; 3]> {
    let t = Vec3::from_array(translation);
    let sel_positions: Vec<Vec3> = selected
        .iter()
        .filter_map(|&i| mesh.positions.get(i as usize))
        .map(|p| Vec3::from_array(*p))
        .collect();
    let selected_set: std::collections::HashSet<u32> = selected.iter().copied().collect();

    mesh.positions
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let pos = Vec3::from_array(*p);
            let w = if selected_set.contains(&(i as u32)) {
                1.0
            } else if falloff > 0.0 && !sel_positions.is_empty() {
                let d = sel_positions
                    .iter()
                    .map(|s| (pos - *s).length())
                    .fold(f32::INFINITY, f32::min);
                if d < falloff {
                    smoothstep(1.0 - d / falloff)
                } else {
                    0.0
                }
            } else {
                0.0
            };
            (pos + t * w).to_array()
        })
        .collect()
}

/// Smoothstep on `[0,1]` (3t² − 2t³).
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Select vertices whose normal points within `threshold` (dot > threshold) of
/// `dir` — e.g. "the top-facing verts" (`dir = +Y`). Needs normals.
pub fn select_by_normal_dir(mesh: &MeshData, dir: [f32; 3], threshold: f32) -> Vec<u32> {
    let Some(normals) = &mesh.normals else {
        return Vec::new();
    };
    let d = Vec3::from_array(dir).normalize_or_zero();
    normals
        .iter()
        .enumerate()
        .filter(|(_, n)| Vec3::from_array(**n).normalize_or_zero().dot(d) > threshold)
        .map(|(i, _)| i as u32)
        .collect()
}

/// Select vertices on one side of an axis plane: component `axis` `cmp` `value`.
pub fn select_by_axis(mesh: &MeshData, axis: usize, cmp: Cmp, value: f32) -> Vec<u32> {
    if axis > 2 {
        return Vec::new();
    }
    mesh.positions
        .iter()
        .enumerate()
        .filter(|(_, p)| match cmp {
            Cmp::Greater => p[axis] > value,
            Cmp::Less => p[axis] < value,
        })
        .map(|(i, _)| i as u32)
        .collect()
}

/// Select every vertex within the top `percent` (0..1) of the axis **extent**
/// along `axis` — a height band (the count depends on tessellation, not `percent`).
pub fn select_top_percent_axis(mesh: &MeshData, axis: usize, percent: f32) -> Vec<u32> {
    if axis > 2 || mesh.positions.is_empty() {
        return Vec::new();
    }
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for p in &mesh.positions {
        lo = lo.min(p[axis]);
        hi = hi.max(p[axis]);
    }
    let cutoff = hi - (hi - lo) * percent.clamp(0.0, 1.0);
    select_by_axis(mesh, axis, Cmp::Greater, cutoff)
}

/// Select the `count` vertices with the GREATEST value along `axis` (a count, not
/// a height band; ties broken by ascending index). Returned in ascending index
/// order, like the other selectors. The count-based companion to
/// [`select_top_percent_axis`].
pub fn select_top_count_axis(mesh: &MeshData, axis: usize, count: u32) -> Vec<u32> {
    if axis > 2 || mesh.positions.is_empty() || count == 0 {
        return Vec::new();
    }
    let mut idx: Vec<u32> = (0..mesh.positions.len() as u32).collect();
    // Highest axis value first; stable by index so ties are deterministic.
    idx.sort_by(|&a, &b| {
        mesh.positions[b as usize][axis]
            .partial_cmp(&mesh.positions[a as usize][axis])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    idx.truncate(count as usize);
    idx.sort_unstable();
    idx
}

/// Select vertices within `radius` of `center`.
pub fn select_within_radius(mesh: &MeshData, center: [f32; 3], radius: f32) -> Vec<u32> {
    let c = Vec3::from_array(center);
    mesh.positions
        .iter()
        .enumerate()
        .filter(|(_, p)| (Vec3::from_array(**p) - c).length() <= radius)
        .map(|(i, _)| i as u32)
        .collect()
}

/// Select vertices inside the axis-aligned box `[min, max]` (inclusive). The box
/// is in the mesh's local space (pair with `get_node_bounds`, transforming a
/// world box into local first, for region selection by area).
pub fn select_within_aabb(mesh: &MeshData, min: [f32; 3], max: [f32; 3]) -> Vec<u32> {
    mesh.positions
        .iter()
        .enumerate()
        .filter(|(_, p)| (0..3).all(|a| p[a] >= min[a] && p[a] <= max[a]))
        .map(|(i, _)| i as u32)
        .collect()
}

/// Heuristic **strip / loop parameterization** for conveyor / tread / road UV
/// authoring. Given the positions of a band of vertices, returns
/// `(axis, coords)` where `axis` is the resolved axle and `coords[i] = [along,
/// across]` for `positions[i]`, normalized so they feed straight into
/// `set_vertex_uvs`:
/// - `along` ∈ `[0,1)` = the vertex's angle **about** `axis` (through the band
///   centroid), mapped from `atan2` — monotonic travel around a belt loop;
/// - `across` ∈ `[0,1]` = the vertex's normalized projection **onto** `axis` —
///   lateral position across the belt width.
///
/// `axis` is the supplied vector (normalized) or, when `None`, the
/// least-variance PCA direction of the band (the axle of a roughly
/// surface-of-revolution belt). This is a HEURISTIC, not a geodesic unwrap: it
/// assumes the band wraps around `axis`. The eigenvector sign is arbitrary, so
/// the `along` winding direction and the `across` polarity may be flipped from a
/// caller's expectation (flip `axis` or `1-coord` to correct).
pub fn strip_parameterize(
    positions: &[[f32; 3]],
    axis: Option<[f32; 3]>,
) -> ([f32; 3], Vec<[f32; 2]>) {
    let n = positions.len();
    if n == 0 {
        return (axis.unwrap_or([0.0, 1.0, 0.0]), Vec::new());
    }
    let mut centroid = Vec3::ZERO;
    for p in positions {
        centroid += Vec3::from_array(*p);
    }
    centroid /= n as f32;

    let axle = match axis {
        Some(a) => {
            let v = Vec3::from_array(a);
            if v.length() > 1.0e-6 {
                v.normalize()
            } else {
                Vec3::Y
            }
        }
        None => pca_smallest_axis(positions, centroid),
    };
    // Orthonormal basis for the plane ⟂ axle.
    let helper = if axle.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
    let e1 = axle.cross(helper).normalize();
    let e2 = axle.cross(e1).normalize();

    let mut coords = Vec::with_capacity(n);
    let (mut amin, mut amax) = (f32::INFINITY, f32::NEG_INFINITY);
    for p in positions {
        let d = Vec3::from_array(*p) - centroid;
        let along = (d.dot(e2).atan2(d.dot(e1)) + std::f32::consts::PI) / std::f32::consts::TAU;
        let across = d.dot(axle);
        amin = amin.min(across);
        amax = amax.max(across);
        coords.push([along, across]);
    }
    let span = (amax - amin).max(1.0e-6);
    for c in &mut coords {
        c[1] = (c[1] - amin) / span;
    }
    (axle.to_array(), coords)
}

/// The least-variance principal axis of a point set (the PCA eigenvector of the
/// smallest covariance eigenvalue) — the "axle" of a roughly planar/tubular
/// band. Classic cyclic Jacobi eigensolver on the symmetric 3×3 covariance
/// (f64 for stability). Used by [`strip_parameterize`] when no axis is given.
fn pca_smallest_axis(positions: &[[f32; 3]], centroid: Vec3) -> Vec3 {
    let mut a = [[0.0f64; 3]; 3];
    for p in positions {
        let d = Vec3::from_array(*p) - centroid;
        let da = [d.x as f64, d.y as f64, d.z as f64];
        for i in 0..3 {
            for j in 0..3 {
                a[i][j] += da[i] * da[j];
            }
        }
    }
    let mut v = [[0.0f64; 3]; 3];
    for (i, row) in v.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    for _sweep in 0..50 {
        let off = a[0][1].abs() + a[0][2].abs() + a[1][2].abs();
        if off < 1.0e-12 {
            break;
        }
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            if a[p][q].abs() < 1.0e-15 {
                continue;
            }
            let theta = 0.5 * (a[q][q] - a[p][p]) / a[p][q];
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            // A ← Jᵀ A J (rotate columns p,q over every row, then rows p,q).
            for row in a.iter_mut() {
                let akp = row[p];
                let akq = row[q];
                row[p] = c * akp - s * akq;
                row[q] = s * akp + c * akq;
            }
            let mut rp = a[p];
            let mut rq = a[q];
            for (xp, xq) in rp.iter_mut().zip(rq.iter_mut()) {
                let apk = *xp;
                let aqk = *xq;
                *xp = c * apk - s * aqk;
                *xq = s * apk + c * aqk;
            }
            a[p] = rp;
            a[q] = rq;
            // V ← V J.
            for row in v.iter_mut() {
                let vp = row[p];
                let vq = row[q];
                row[p] = c * vp - s * vq;
                row[q] = s * vp + c * vq;
            }
        }
    }
    let eig = [a[0][0], a[1][1], a[2][2]];
    let mut min_i = 0;
    for i in 1..3 {
        if eig[i] < eig[min_i] {
            min_i = i;
        }
    }
    let col = Vec3::new(v[0][min_i] as f32, v[1][min_i] as f32, v[2][min_i] as f32);
    if col.length() > 1.0e-6 {
        col.normalize()
    } else {
        Vec3::Y
    }
}

/// Position-welded connected-component id per ORIGINAL vertex index, plus the
/// component count. Vertices at the same position (within a quantization epsilon)
/// are **welded** before union-find over triangle edges — so a UV/normal seam
/// (which duplicates a vertex per face) doesn't fragment one solid piece into
/// many components. The backbone of [`connected_component_of`] /
/// [`connected_components`].
/// Union-find core: given a per-vertex **weld representative** array (vertices
/// sharing a rep are pre-merged) and a triangle index buffer, return a dense
/// component id per ORIGINAL vertex + the component count. Shared by
/// position-welded ([`weld_components`]) and UV-welded ([`uv_islands`]) island
/// detection — only the weld key differs.
fn components_from_weld(weld: &[u32], indices: &[u32]) -> (Vec<u32>, u32) {
    use std::collections::HashMap;
    let n = weld.len();
    fn find(parent: &mut [u32], x: u32) -> u32 {
        let mut r = x;
        while parent[r as usize] != r {
            r = parent[r as usize];
        }
        let mut c = x;
        while parent[c as usize] != r {
            let next = parent[c as usize];
            parent[c as usize] = r;
            c = next;
        }
        r
    }
    fn union(parent: &mut [u32], a: u32, b: u32) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent[ra as usize] = rb;
        }
    }
    let mut parent: Vec<u32> = (0..n as u32).collect();
    for tri in indices.chunks_exact(3) {
        let a = weld[tri[0] as usize];
        let b = weld[tri[1] as usize];
        let c = weld[tri[2] as usize];
        union(&mut parent, a, b);
        union(&mut parent, b, c);
    }
    let mut comp_ids: HashMap<u32, u32> = HashMap::new();
    let mut next = 0u32;
    let mut comp_of = vec![0u32; n];
    for (i, slot) in comp_of.iter_mut().enumerate() {
        let root = find(&mut parent, weld[i]);
        *slot = *comp_ids.entry(root).or_insert_with(|| {
            let id = next;
            next += 1;
            id
        });
    }
    (comp_of, next)
}

/// Position-welded connected-component id per ORIGINAL vertex index, plus the
/// component count. Vertices at the same position (within a quantization epsilon)
/// are **welded** before union-find over triangle edges — so a UV/normal seam
/// (which duplicates a vertex per face) doesn't fragment one solid piece into
/// many components. The backbone of [`connected_component_of`] /
/// [`connected_components`].
fn weld_components(mesh: &MeshData) -> (Vec<u32>, u32) {
    use std::collections::HashMap;
    let q = 1.0e4_f32;
    let mut pos_rep: HashMap<(i64, i64, i64), u32> = HashMap::new();
    let mut weld = vec![0u32; mesh.positions.len()];
    for (i, p) in mesh.positions.iter().enumerate() {
        let cell = (
            (p[0] * q).round() as i64,
            (p[1] * q).round() as i64,
            (p[2] * q).round() as i64,
        );
        weld[i] = *pos_rep.entry(cell).or_insert(i as u32);
    }
    components_from_weld(&weld, &mesh.indices)
}

/// UV-space connected components (**islands**): vertices whose UV coincides
/// (within a quantization epsilon) are welded, then union-find runs over the
/// triangle edges. Returns `(island_id per vertex, island_count)`. This is what
/// distinguishes a contiguous **strip** UV (one island spanning ~[0,1]) from a
/// packed **atlas** (many small islands) — the UV-layout overlay's core. `uvs`
/// is one UV set, vertex-aligned with the index buffer.
pub fn uv_islands(uvs: &[[f32; 2]], indices: &[u32]) -> (Vec<u32>, u32) {
    use std::collections::HashMap;
    let q = 1.0e4_f32;
    let mut uv_rep: HashMap<(i64, i64), u32> = HashMap::new();
    let mut weld = vec![0u32; uvs.len()];
    for (i, uv) in uvs.iter().enumerate() {
        let cell = ((uv[0] * q).round() as i64, (uv[1] * q).round() as i64);
        weld[i] = *uv_rep.entry(cell).or_insert(i as u32);
    }
    components_from_weld(&weld, indices)
}

/// All vertex indices in the connected component(s) containing `seeds` (the
/// "select this solid piece" selector). Position-welded, so a UV/normal-split
/// seam doesn't break a piece apart. Out-of-range seeds are ignored; an empty /
/// all-invalid seed list returns empty.
pub fn connected_component_of(mesh: &MeshData, seeds: &[u32]) -> Vec<u32> {
    if mesh.positions.is_empty() {
        return Vec::new();
    }
    let (comp_of, _) = weld_components(mesh);
    let seed_comps: std::collections::HashSet<u32> = seeds
        .iter()
        .filter_map(|&s| comp_of.get(s as usize).copied())
        .collect();
    if seed_comps.is_empty() {
        return Vec::new();
    }
    (0..comp_of.len() as u32)
        .filter(|&i| seed_comps.contains(&comp_of[i as usize]))
        .collect()
}

/// Every connected component (island) as a list of vertex indices, position-welded.
/// Order is by first-seen vertex. The discovery counterpart to
/// [`connected_component_of`] (e.g. "how many separate pieces does this mesh have").
pub fn connected_components(mesh: &MeshData) -> Vec<Vec<u32>> {
    let (comp_of, count) = weld_components(mesh);
    let mut out = vec![Vec::new(); count as usize];
    for (i, &c) in comp_of.iter().enumerate() {
        out[c as usize].push(i as u32);
    }
    out
}

/// Builds one compacted sub-mesh (only the vertices its pushed triangles use,
/// indices remapped, attributes carried through). Backs [`extract_faces`].
struct SubMeshBuilder<'a> {
    src: &'a MeshData,
    remap: std::collections::HashMap<u32, u32>,
    positions: Vec<[f32; 3]>,
    normals: Option<Vec<[f32; 3]>>,
    uvs: Vec<Vec<[f32; 2]>>,
    colors: Option<Vec<[f32; 4]>>,
    indices: Vec<u32>,
}

impl<'a> SubMeshBuilder<'a> {
    fn new(src: &'a MeshData) -> Self {
        Self {
            src,
            remap: std::collections::HashMap::new(),
            positions: Vec::new(),
            normals: src.normals.as_ref().map(|_| Vec::new()),
            uvs: src.uvs.iter().map(|_| Vec::new()).collect(),
            colors: src.colors.as_ref().map(|_| Vec::new()),
            indices: Vec::new(),
        }
    }
    /// Map an original vertex index to this sub-mesh's compacted index, copying
    /// its attributes on first use.
    fn map(&mut self, i: u32) -> u32 {
        if let Some(&n) = self.remap.get(&i) {
            return n;
        }
        let n = self.positions.len() as u32;
        self.remap.insert(i, n);
        let iu = i as usize;
        self.positions.push(self.src.positions[iu]);
        if let (Some(dst), Some(s)) = (self.normals.as_mut(), self.src.normals.as_ref()) {
            dst.push(s[iu]);
        }
        for (k, dst) in self.uvs.iter_mut().enumerate() {
            dst.push(self.src.uvs[k][iu]);
        }
        if let (Some(dst), Some(s)) = (self.colors.as_mut(), self.src.colors.as_ref()) {
            dst.push(s[iu]);
        }
        n
    }
    fn push_tri(&mut self, a: u32, b: u32, c: u32) {
        let a = self.map(a);
        let b = self.map(b);
        let c = self.map(c);
        self.indices.extend([a, b, c]);
    }
    fn finish(self) -> MeshData {
        MeshData {
            positions: self.positions,
            normals: self.normals,
            uvs: self.uvs,
            colors: self.colors,
            indices: self.indices,
        }
    }
}

/// Split a mesh into `(extracted, remainder)` by a vertex selection: a triangle
/// goes to `extracted` when **all three** of its vertices are selected, else to
/// `remainder`. Each side is a compacted sub-mesh (only the vertices its
/// triangles reference, indices remapped, normals/uvs/colors carried through).
/// The backbone of the editor's `SeparateMesh` command — detach a region into a
/// new node. Either side may be empty (selection covered no complete face, or
/// covered everything).
pub fn extract_faces(mesh: &MeshData, selected: &[u32]) -> (MeshData, MeshData) {
    let sel: std::collections::HashSet<u32> = selected.iter().copied().collect();
    let mut ext = SubMeshBuilder::new(mesh);
    let mut rem = SubMeshBuilder::new(mesh);
    for tri in mesh.indices.chunks_exact(3) {
        let target = if tri.iter().all(|i| sel.contains(i)) {
            &mut ext
        } else {
            &mut rem
        };
        target.push_tri(tri[0], tri[1], tri[2]);
    }
    (ext.finish(), rem.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modifiers::lathe;
    use crate::primitives::box_mesh;
    use std::f32::consts::TAU;

    #[test]
    fn strip_parameterize_cylinder_band_about_y() {
        // A band on a unit-radius cylinder about Y: 16 angles × 2 heights (±0.2).
        // Loop (X/Z) variance ≫ height (Y) variance ⇒ PCA axle = Y.
        let mut pts = Vec::new();
        for i in 0..16 {
            let th = i as f32 / 16.0 * TAU;
            for &h in &[-0.2f32, 0.2] {
                pts.push([th.cos(), h, th.sin()]);
            }
        }
        let (axis, coords) = strip_parameterize(&pts, None);
        // Resolved axle ≈ ±Y.
        assert!(
            axis[1].abs() > 0.99 && axis[0].abs() < 0.05 && axis[2].abs() < 0.05,
            "axle should be ~Y, got {axis:?}"
        );
        assert_eq!(coords.len(), pts.len());
        // along spans a wide range around the loop; across hits both extremes.
        let along_min = coords.iter().map(|c| c[0]).fold(f32::INFINITY, f32::min);
        let along_max = coords
            .iter()
            .map(|c| c[0])
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(along_max - along_min > 0.8, "along should span the loop");
        let across_min = coords.iter().map(|c| c[1]).fold(f32::INFINITY, f32::min);
        let across_max = coords
            .iter()
            .map(|c| c[1])
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            across_min < 0.01 && across_max > 0.99,
            "across should span [0,1]"
        );
        // The two heights form two tight across-clusters (lateral position).
        for (p, c) in pts.iter().zip(&coords) {
            let near0 = c[1] < 0.01;
            let near1 = c[1] > 0.99;
            assert!(near0 || near1, "across should be ~0 or ~1, got {}", c[1]);
            // same height ⇒ same cluster (sign may flip which height is which).
            let _ = p;
        }
    }

    #[test]
    fn strip_parameterize_respects_supplied_axis() {
        let pts = [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [-1.0, 0.0, 0.0]];
        let (axis, coords) = strip_parameterize(&pts, Some([0.0, 2.0, 0.0]));
        // Supplied axis is normalized.
        assert!((axis[1] - 1.0).abs() < 1.0e-6 && axis[0].abs() < 1.0e-6);
        assert_eq!(coords.len(), 3);
        // all `along` in [0,1).
        assert!(coords.iter().all(|c| c[0] >= 0.0 && c[0] < 1.0));
    }

    /// Merge two meshes into one buffer with disjoint vertex ranges (second's
    /// indices offset) — a single MeshData holding two unconnected islands.
    fn merge(a: &MeshData, b: &MeshData) -> MeshData {
        let off = a.positions.len() as u32;
        let mut positions = a.positions.clone();
        positions.extend_from_slice(&b.positions);
        let mut indices = a.indices.clone();
        indices.extend(b.indices.iter().map(|&i| i + off));
        MeshData {
            positions,
            normals: None,
            uvs: Vec::new(),
            colors: None,
            indices,
        }
    }

    #[test]
    fn connected_components_finds_two_disjoint_boxes() {
        let a = box_mesh(Vec3::splat(1.0));
        let mut b = box_mesh(Vec3::splat(1.0));
        // Shift b far away so no positions weld between the two.
        for p in &mut b.positions {
            p[0] += 100.0;
        }
        let na = a.positions.len() as u32;
        let merged = merge(&a, &b);

        let comps = connected_components(&merged);
        assert_eq!(comps.len(), 2, "two disjoint boxes → two islands");

        // A seed in box A selects exactly box A's vertex range; none of box B.
        let sel_a = connected_component_of(&merged, &[0]);
        assert!(sel_a.iter().all(|&i| i < na), "box A seed leaked into B");
        assert!(sel_a.contains(&0));
        // A seed in box B selects only the B range.
        let sel_b = connected_component_of(&merged, &[na]);
        assert!(sel_b.iter().all(|&i| i >= na), "box B seed leaked into A");
        // Together they cover everything, disjointly.
        assert_eq!(sel_a.len() + sel_b.len(), merged.positions.len());
    }

    #[test]
    fn connected_component_welds_split_seam_vertices() {
        // A single box has split (per-face) corner vertices at shared positions;
        // position-welding must still report it as ONE connected component.
        let m = box_mesh(Vec3::splat(2.0));
        let comps = connected_components(&m);
        assert_eq!(
            comps.len(),
            1,
            "a box is one welded piece, got {}",
            comps.len()
        );
        // A seed at any vertex selects the whole mesh.
        let all = connected_component_of(&m, &[0]);
        assert_eq!(all.len(), m.positions.len());
        // Empty / out-of-range seeds → empty.
        assert!(connected_component_of(&m, &[]).is_empty());
        assert!(connected_component_of(&m, &[9999]).is_empty());
    }

    #[test]
    fn uv_islands_distinguishes_contiguous_vs_split() {
        // One quad with a contiguous UV square → one island.
        let uvs = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let indices = vec![0, 1, 2, 0, 2, 3];
        let (_, count) = uv_islands(&uvs, &indices);
        assert_eq!(count, 1, "a single contiguous quad is one UV island");

        // Two quads whose UVs don't touch → two islands.
        let uvs2 = vec![
            [0.0, 0.0],
            [0.4, 0.0],
            [0.4, 0.4],
            [0.0, 0.4], // island A
            [0.6, 0.6],
            [1.0, 0.6],
            [1.0, 1.0],
            [0.6, 1.0], // island B
        ];
        let indices2 = vec![0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7];
        let (island_of, count2) = uv_islands(&uvs2, &indices2);
        assert_eq!(count2, 2, "two disjoint UV quads are two islands");
        assert_ne!(island_of[0], island_of[4], "the two quads differ");
        assert_eq!(island_of[0], island_of[3], "quad A is one island");
    }

    #[test]
    fn extract_faces_splits_two_boxes() {
        // Two disjoint boxes in one buffer; selecting box A's vertex range must
        // extract exactly box A and leave box B as the remainder.
        let a = box_mesh(Vec3::splat(1.0));
        let mut b = box_mesh(Vec3::splat(1.0));
        for p in &mut b.positions {
            p[0] += 100.0;
        }
        let na = a.positions.len() as u32;
        let tris_a = a.indices.len();
        let tris_b = b.indices.len();
        let merged = merge(&a, &b);

        let sel: Vec<u32> = (0..na).collect(); // all of box A
        let (ext, rem) = extract_faces(&merged, &sel);

        // Extracted = box A; remainder = box B. Triangle counts preserved.
        assert_eq!(ext.indices.len(), tris_a);
        assert_eq!(rem.indices.len(), tris_b);
        assert_eq!(ext.positions.len(), a.positions.len());
        assert_eq!(rem.positions.len(), b.positions.len());
        // No dangling indices on either side.
        assert!(ext
            .indices
            .iter()
            .all(|&i| (i as usize) < ext.positions.len()));
        assert!(rem
            .indices
            .iter()
            .all(|&i| (i as usize) < rem.positions.len()));
        // The remainder's geometry is box B (shifted +100 in x).
        assert!(rem.positions.iter().all(|p| p[0] > 50.0));
        assert!(ext.positions.iter().all(|p| p[0] < 50.0));
    }

    #[test]
    fn extract_faces_partial_face_stays_in_remainder() {
        // A face is extracted only when ALL its verts are selected. Selecting a
        // strict subset of a single box's verts extracts nothing whole.
        let m = box_mesh(Vec3::splat(2.0));
        let (ext, rem) = extract_faces(&m, &[0]); // one vertex only
        assert!(ext.indices.is_empty(), "no complete face selected");
        assert_eq!(rem.indices.len(), m.indices.len());
    }

    #[test]
    fn within_aabb_selects_the_box_region() {
        // A 2x2x2 box centered at origin → 8 corners at ±1. A box covering only
        // the +x half (x in [0,2]) selects exactly the 4 +x corners.
        let m = box_mesh(Vec3::splat(2.0));
        let sel = select_within_aabb(&m, [0.0, -2.0, -2.0], [2.0, 2.0, 2.0]);
        assert!(!sel.is_empty());
        assert!(sel.iter().all(|&i| m.positions[i as usize][0] >= 0.0));
        // Every +x vertex is selected; no -x vertex is.
        for (i, p) in m.positions.iter().enumerate() {
            assert_eq!(sel.contains(&(i as u32)), p[0] >= 0.0);
        }
    }

    #[test]
    fn hard_transform_moves_only_selection() {
        let m = box_mesh(Vec3::splat(2.0));
        let out = soft_transform_positions(&m, &[0], [10.0, 0.0, 0.0], 0.0);
        // Vertex 0 moved by exactly +10x; everything else stayed put.
        assert!((out[0][0] - (m.positions[0][0] + 10.0)).abs() < 1e-5);
        for (o, p) in out.iter().zip(&m.positions).skip(1) {
            assert_eq!(o, p);
        }
    }

    #[test]
    fn soft_transform_falls_off_with_distance() {
        // A row of vertices along X; drag vertex 0 with a falloff covering ~half.
        let m = MeshData {
            positions: (0..5).map(|i| [i as f32, 0.0, 0.0]).collect(),
            normals: None,
            uvs: vec![],
            colors: None,
            indices: vec![],
        };
        let out = soft_transform_positions(&m, &[0], [0.0, 1.0, 0.0], 2.5);
        // Selected fully moved; weight strictly decreases with distance; beyond
        // the radius unaffected.
        assert!((out[0][1] - 1.0).abs() < 1e-5);
        assert!(out[1][1] > out[2][1] && out[2][1] > 0.0);
        assert_eq!(out[3][1], 0.0); // distance 3 > radius 2.5
        assert_eq!(out[4][1], 0.0);
    }

    #[test]
    fn select_top_percent_grabs_the_cap() {
        // A lathed cylinder, y in [-2, 2]; top 25% ⇒ only y > 1.
        let rows: Vec<[f32; 2]> = (0..=4).map(|i| [i as f32 - 2.0, 1.0]).collect();
        let m = lathe(&rows, 12, TAU);
        let sel = select_top_percent_axis(&m, 1, 0.25);
        assert!(!sel.is_empty());
        for &i in &sel {
            assert!(m.positions[i as usize][1] > 1.0);
        }
    }

    #[test]
    fn select_top_count_grabs_exactly_n_highest() {
        // 5 verts stacked along Y at y = 0..4. Top 2 by count = the two highest.
        let m = MeshData {
            positions: (0..5).map(|i| [0.0, i as f32, 0.0]).collect(),
            normals: None,
            uvs: vec![],
            colors: None,
            indices: vec![],
        };
        let sel = select_top_count_axis(&m, 1, 2);
        assert_eq!(
            sel,
            vec![3, 4],
            "exactly the 2 highest verts, in index order"
        );
        // count 0 selects nothing; count beyond the vertex count clamps to all.
        assert!(select_top_count_axis(&m, 1, 0).is_empty());
        assert_eq!(select_top_count_axis(&m, 1, 99).len(), 5);
    }

    #[test]
    fn select_within_radius_and_axis() {
        let m = box_mesh(Vec3::splat(2.0)); // corners at ±1
        let near = select_within_radius(&m, [1.0, 1.0, 1.0], 0.1);
        assert!(near.iter().all(|&i| {
            let p = m.positions[i as usize];
            (p[0] - 1.0).abs() < 0.1 && (p[1] - 1.0).abs() < 0.1 && (p[2] - 1.0).abs() < 0.1
        }));
        let pos_x = select_by_axis(&m, 0, Cmp::Greater, 0.0);
        assert!(pos_x.iter().all(|&i| m.positions[i as usize][0] > 0.0));
    }

    #[test]
    fn select_by_normal_dir_top_faces() {
        let m = box_mesh(Vec3::splat(2.0)); // box_mesh sets per-face normals
        let up = select_by_normal_dir(&m, [0.0, 1.0, 0.0], 0.9);
        assert!(!up.is_empty());
        for &i in &up {
            assert!(m.normals.as_ref().unwrap()[i as usize][1] > 0.9);
        }
    }
}
