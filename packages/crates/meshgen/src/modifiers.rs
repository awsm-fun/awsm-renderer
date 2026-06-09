//! Modifier-stack **evaluation** — recipe ([`ModifierStack`]) → [`MeshData`].
//!
//! Pure CPU, no scene/GPU access, so it's natively unit-tested (per-modifier
//! bbox / vertex-count / falloff asserts). The base generators handle the
//! self-contained [`MeshBase`] variants (`Primitive` / `Lathe` / `Superquadric`);
//! `Sweep` and `Captured` reference scene state (a curve node / the mesh store),
//! so the editor resolves those to a base `MeshData` and calls
//! [`apply_modifiers`] directly.

use std::collections::HashMap;
use std::f32::consts::{PI, TAU};

use awsm_scene_schema::modifier::{Axis, MeshBase, Modifier, ModifierStack};
use awsm_scene_schema::PrimitiveShape;
use glam::Vec3;

use crate::mesh_data::MeshData;
use crate::primitives::{box_mesh, cone_mesh, cylinder_mesh, plane_mesh, sphere_mesh, torus_mesh};

/// Evaluate a full stack to triangles. `Sweep`/`Captured` bases (which need scene
/// state) produce an empty base here — resolve those editor-side and call
/// [`apply_modifiers`] instead.
pub fn evaluate(stack: &ModifierStack) -> MeshData {
    let base = match &stack.base {
        MeshBase::Primitive(shape) => primitive_mesh(shape),
        MeshBase::Lathe {
            profile,
            segments,
            angle,
        } => lathe(profile, *segments, *angle),
        MeshBase::Superquadric {
            e1,
            e2,
            segments_long,
            segments_lat,
        } => superquadric(*e1, *e2, *segments_long, *segments_lat),
        // Sweep/Captured need scene state (resolved editor-side).
        MeshBase::Sweep(_) | MeshBase::Captured(_) => MeshData::default(),
        // SDF/CSG graph → surface nets.
        MeshBase::Sdf { node, resolution } => crate::sdf_mesh::surface_nets_mesh(node, *resolution),
    };
    apply_modifiers(base, &stack.modifiers)
}

/// Map a scene `PrimitiveShape` to its generated mesh (mirrors the renderer
/// bridge's `primitive_to_mesh`, kept here so eval is self-contained).
pub fn primitive_mesh(shape: &PrimitiveShape) -> MeshData {
    match shape {
        PrimitiveShape::Plane {
            width,
            depth,
            segments_x,
            segments_z,
        } => plane_mesh(*width, *depth, *segments_x, *segments_z),
        PrimitiveShape::Box { dims } => box_mesh(Vec3::from_array(*dims)),
        PrimitiveShape::Sphere {
            radius,
            segments_long,
            segments_lat,
        } => sphere_mesh(*radius, *segments_long, *segments_lat),
        PrimitiveShape::Cylinder {
            radius,
            height,
            radial_segments,
        } => cylinder_mesh(*radius, *height, *radial_segments),
        PrimitiveShape::Cone {
            radius,
            height,
            radial_segments,
        } => cone_mesh(*radius, *height, *radial_segments),
        PrimitiveShape::Torus {
            radius,
            thickness,
            segments_major,
            segments_minor,
        } => torus_mesh(*radius, *thickness, *segments_major, *segments_minor),
    }
}

/// Apply an ordered modifier list to a base mesh, recomputing normals after each
/// step (so normal-dependent deformers like `Inflate` see a current basis).
pub fn apply_modifiers(mut mesh: MeshData, modifiers: &[Modifier]) -> MeshData {
    if mesh.normals.is_none() {
        mesh.compute_vertex_normals();
    }
    for m in modifiers {
        apply_one(&mut mesh, m);
        mesh.compute_vertex_normals();
    }
    mesh
}

fn apply_one(mesh: &mut MeshData, m: &Modifier) {
    match m {
        Modifier::Taper { axis, factor } => taper(mesh, *axis, *factor),
        Modifier::Twist { axis, turns } => twist(mesh, *axis, *turns),
        Modifier::Bend { axis, angle } => bend(mesh, *axis, *angle),
        Modifier::Inflate { amount } => inflate(mesh, *amount),
        Modifier::Spherify { factor } => spherify(mesh, *factor),
        Modifier::Roughen { amount, seed } => roughen(mesh, *amount, *seed),
        Modifier::Subdivide { iterations } => {
            for _ in 0..*iterations {
                subdivide(mesh);
            }
        }
        Modifier::Smooth { iterations, factor } => smooth(mesh, *iterations, *factor),
        Modifier::Mirror { axis } => mirror(mesh, *axis),
        Modifier::Array { count, offset } => array(mesh, *count, *offset),
        Modifier::Displace { expr } => displace(mesh, expr),
    }
}

/// Formula displacement: evaluate `expr` per vertex (over `x,y,z,nx,ny,nz,u,v,i`)
/// and offset the vertex along its normal by the result. A malformed expression
/// (or one referencing an unknown name) is a no-op.
fn displace(mesh: &mut MeshData, expr: &str) {
    let Some(program) = crate::expr::Expr::compile(expr) else {
        return;
    };
    let normals = match &mesh.normals {
        Some(n) if n.len() == mesh.positions.len() => n.clone(),
        _ => return,
    };
    let uvs = mesh.uvs.clone();
    for (i, (p, n)) in mesh.positions.iter_mut().zip(normals.iter()).enumerate() {
        let (uu, vv) = uvs
            .as_ref()
            .and_then(|u| u.get(i))
            .map(|uv| (uv[0], uv[1]))
            .unwrap_or((0.0, 0.0));
        let vars = crate::expr::Vars {
            x: p[0],
            y: p[1],
            z: p[2],
            nx: n[0],
            ny: n[1],
            nz: n[2],
            u: uu,
            v: vv,
            i: i as f32,
        };
        if let Some(d) = program.eval(&vars) {
            p[0] += n[0] * d;
            p[1] += n[1] * d;
            p[2] += n[2] * d;
        }
    }
}

// ───────────────────────── base generators ─────────────────────────

/// Revolve a 2D `(height, radius)` profile around the Y axis. `angle` radians of
/// sweep (`TAU` = closed surface of revolution); `segments` radial divisions.
pub fn lathe(profile: &[[f32; 2]], segments: u32, angle: f32) -> MeshData {
    let segs = segments.max(3) as usize;
    let rows = profile.len();
    if rows < 2 {
        return MeshData::default();
    }
    let closed = (angle - TAU).abs() < 1e-4;
    let cols = if closed { segs } else { segs + 1 };

    let mut positions = Vec::with_capacity(rows * cols);
    let mut uvs = Vec::with_capacity(rows * cols);
    for (ri, p) in profile.iter().enumerate() {
        let (h, r) = (p[0], p[1]);
        for c in 0..cols {
            let t = c as f32 / segs as f32;
            let theta = t * angle;
            positions.push([r * theta.cos(), h, r * theta.sin()]);
            uvs.push([t, ri as f32 / (rows - 1) as f32]);
        }
    }
    let col_step = cols;
    let wrap = |c: usize| if closed { (c + 1) % cols } else { c + 1 };
    let mut indices = Vec::new();
    for ri in 0..rows - 1 {
        for c in 0..segs {
            let c1 = wrap(c);
            let a = ri * col_step + c;
            let b = ri * col_step + c1;
            let d = (ri + 1) * col_step + c;
            let e = (ri + 1) * col_step + c1;
            indices.extend_from_slice(&[a as u32, d as u32, b as u32]);
            indices.extend_from_slice(&[b as u32, d as u32, e as u32]);
        }
    }
    let mut mesh = MeshData {
        positions,
        normals: None,
        uvs: Some(uvs),
        colors: None,
        indices,
    };
    mesh.compute_vertex_normals();
    mesh
}

/// A superellipsoid: `e1` controls the latitude profile, `e2` the longitude;
/// `(1,1)` is a sphere, `(0,0)` a box, etc.
pub fn superquadric(e1: f32, e2: f32, segments_long: u32, segments_lat: u32) -> MeshData {
    let lon = segments_long.max(3) as usize;
    let lat = segments_lat.max(2) as usize;
    // Signed power helper: sign(v)*|v|^e.
    let sp = |v: f32, e: f32| v.signum() * v.abs().powf(e);

    let mut positions = Vec::with_capacity((lat + 1) * (lon + 1));
    let mut uvs = Vec::with_capacity((lat + 1) * (lon + 1));
    for i in 0..=lat {
        let eta = -PI / 2.0 + PI * (i as f32 / lat as f32); // [-π/2, π/2]
        for j in 0..=lon {
            let omega = -PI + TAU * (j as f32 / lon as f32); // [-π, π]
            let ce = sp(eta.cos(), e1);
            let x = ce * sp(omega.cos(), e2);
            let y = sp(eta.sin(), e1);
            let z = ce * sp(omega.sin(), e2);
            positions.push([x, y, z]);
            uvs.push([j as f32 / lon as f32, i as f32 / lat as f32]);
        }
    }
    let stride = lon + 1;
    let mut indices = Vec::new();
    for i in 0..lat {
        for j in 0..lon {
            let a = i * stride + j;
            let b = a + 1;
            let c = a + stride;
            let d = c + 1;
            indices.extend_from_slice(&[a as u32, c as u32, b as u32]);
            indices.extend_from_slice(&[b as u32, c as u32, d as u32]);
        }
    }
    let mut mesh = MeshData {
        positions,
        normals: None,
        uvs: Some(uvs),
        colors: None,
        indices,
    };
    mesh.compute_vertex_normals();
    mesh
}

// ───────────────────────── deformers ─────────────────────────

fn axis_index(a: Axis) -> usize {
    match a {
        Axis::X => 0,
        Axis::Y => 1,
        Axis::Z => 2,
    }
}

/// `(min, max)` of one component over all vertices.
fn extent(mesh: &MeshData, comp: usize) -> (f32, f32) {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for p in &mesh.positions {
        lo = lo.min(p[comp]);
        hi = hi.max(p[comp]);
    }
    if mesh.positions.is_empty() {
        (0.0, 0.0)
    } else {
        (lo, hi)
    }
}

fn taper(mesh: &mut MeshData, axis: Axis, factor: f32) {
    let a = axis_index(axis);
    let (lo, hi) = extent(mesh, a);
    let span = (hi - lo).max(1e-6);
    let (u, v) = other_axes(a);
    for p in &mut mesh.positions {
        let t = (p[a] - lo) / span;
        let s = 1.0 + (factor - 1.0) * t;
        p[u] *= s;
        p[v] *= s;
    }
}

fn twist(mesh: &mut MeshData, axis: Axis, turns: f32) {
    let a = axis_index(axis);
    let (lo, hi) = extent(mesh, a);
    let span = (hi - lo).max(1e-6);
    let (u, v) = other_axes(a);
    for p in &mut mesh.positions {
        let t = (p[a] - lo) / span;
        let ang = turns * TAU * t;
        let (s, c) = ang.sin_cos();
        let (pu, pv) = (p[u], p[v]);
        p[u] = pu * c - pv * s;
        p[v] = pu * s + pv * c;
    }
}

/// Simple bend: curve the `axis` extent into an arc of total `angle` (radians),
/// bending the next-cyclic axis as the "height". A near-zero angle is a no-op.
fn bend(mesh: &mut MeshData, axis: Axis, angle: f32) {
    if angle.abs() < 1e-5 {
        return;
    }
    let a = axis_index(axis);
    let p_idx = (a + 1) % 3; // height axis the length bends toward
    let (lo, hi) = extent(mesh, a);
    let center = 0.5 * (lo + hi);
    let length = (hi - lo).max(1e-6);
    let k = angle / length;
    let r = 1.0 / k;
    for p in &mut mesh.positions {
        let a0 = p[a] - center;
        let theta = a0 * k;
        let h = p[p_idx];
        p[a] = center + (r - h) * theta.sin();
        p[p_idx] = r - (r - h) * theta.cos();
    }
}

fn inflate(mesh: &mut MeshData, amount: f32) {
    let normals = match &mesh.normals {
        Some(n) if n.len() == mesh.positions.len() => n.clone(),
        _ => return,
    };
    for (p, n) in mesh.positions.iter_mut().zip(normals.iter()) {
        p[0] += n[0] * amount;
        p[1] += n[1] * amount;
        p[2] += n[2] * amount;
    }
}

fn spherify(mesh: &mut MeshData, factor: f32) {
    let center = centroid(mesh);
    let radius = mesh
        .positions
        .iter()
        .map(|p| (Vec3::from_array(*p) - center).length())
        .fold(0.0_f32, f32::max);
    if radius < 1e-6 {
        return;
    }
    for p in &mut mesh.positions {
        let pos = Vec3::from_array(*p);
        let dir = (pos - center).normalize_or_zero();
        let target = center + dir * radius;
        let np = pos.lerp(target, factor);
        *p = np.to_array();
    }
}

fn roughen(mesh: &mut MeshData, amount: f32, seed: u32) {
    let normals = match &mesh.normals {
        Some(n) if n.len() == mesh.positions.len() => n.clone(),
        _ => return,
    };
    // Weld-aware: jitter is keyed by **position** (not vertex index) and applied
    // along a **per-weld-group** averaged normal, so coincident-but-split vertices
    // (UV-sphere seams/poles, hard-edged box corners) move identically and the
    // surface stays closed (index-keyed jitter cracked them open).
    let canon = weld_indices(&mesh.positions);
    let groups = canon.iter().copied().max().map_or(0, |m| m as usize + 1);
    let mut group_normal = vec![Vec3::ZERO; groups];
    for (i, &c) in canon.iter().enumerate() {
        group_normal[c as usize] += Vec3::from_array(normals[i]);
    }
    for v in group_normal.iter_mut() {
        *v = v.normalize_or_zero();
    }
    for (i, p) in mesh.positions.iter_mut().enumerate() {
        // Deterministic jitter in [-amount, amount] from the quantized position +
        // seed (Math.random is unavailable here; determinism matters for replay).
        let d = (hash01(pos_hash(p, seed)) * 2.0 - 1.0) * amount;
        let dir = group_normal[canon[i] as usize];
        p[0] += dir.x * d;
        p[1] += dir.y * d;
        p[2] += dir.z * d;
    }
}

/// One round of midpoint subdivision: every triangle → 4.
fn subdivide(mesh: &mut MeshData) {
    let mut positions = mesh.positions.clone();
    let mut midpoints: HashMap<(u32, u32), u32> = HashMap::new();
    let mut indices = Vec::with_capacity(mesh.indices.len() * 4);

    let mut midpoint = |a: u32, b: u32, positions: &mut Vec<[f32; 3]>| -> u32 {
        let key = if a < b { (a, b) } else { (b, a) };
        if let Some(&m) = midpoints.get(&key) {
            return m;
        }
        let pa = Vec3::from_array(positions[a as usize]);
        let pb = Vec3::from_array(positions[b as usize]);
        let m = positions.len() as u32;
        positions.push(((pa + pb) * 0.5).to_array());
        midpoints.insert(key, m);
        m
    };

    for tri in mesh.indices.chunks_exact(3) {
        let (a, b, c) = (tri[0], tri[1], tri[2]);
        let ab = midpoint(a, b, &mut positions);
        let bc = midpoint(b, c, &mut positions);
        let ca = midpoint(c, a, &mut positions);
        indices.extend_from_slice(&[a, ab, ca]);
        indices.extend_from_slice(&[ab, b, bc]);
        indices.extend_from_slice(&[ca, bc, c]);
        indices.extend_from_slice(&[ab, bc, ca]);
    }
    mesh.positions = positions;
    mesh.indices = indices;
    // Midpoint subdivision invalidates the old per-vertex attrs; drop them
    // (normals are recomputed by the caller; uvs/colors would need interpolation).
    mesh.uvs = None;
    mesh.colors = None;
    mesh.normals = None;
}

/// Laplacian smoothing: move each vertex `factor` toward its neighbours' mean.
///
/// Weld-aware: the adjacency graph + the moved positions are computed on
/// **welded** vertices (coincident vertices collapse to one node), so seams stay
/// shut. A per-index graph let split seam/corner vertices drift apart (holes).
fn smooth(mesh: &mut MeshData, iterations: u32, factor: f32) {
    let n = mesh.positions.len();
    if n == 0 {
        return;
    }
    let canon = weld_indices(&mesh.positions);
    let groups = canon.iter().copied().max().map_or(0, |m| m as usize + 1);
    // Neighbour sets on the welded graph.
    let mut neighbours: Vec<Vec<u32>> = vec![Vec::new(); groups];
    for tri in mesh.indices.chunks_exact(3) {
        for k in 0..3 {
            let a = canon[tri[k] as usize];
            let b = canon[tri[(k + 1) % 3] as usize];
            if a != b {
                if !neighbours[a as usize].contains(&b) {
                    neighbours[a as usize].push(b);
                }
                if !neighbours[b as usize].contains(&a) {
                    neighbours[b as usize].push(a);
                }
            }
        }
    }
    for _ in 0..iterations {
        // Current welded positions (all members of a group share one).
        let mut cur = vec![Vec3::ZERO; groups];
        for (i, &c) in canon.iter().enumerate() {
            cur[c as usize] = Vec3::from_array(mesh.positions[i]);
        }
        let mut next = cur.clone();
        for (g, nbrs) in neighbours.iter().enumerate() {
            if nbrs.is_empty() {
                continue;
            }
            let mut avg = Vec3::ZERO;
            for &j in nbrs {
                avg += cur[j as usize];
            }
            avg /= nbrs.len() as f32;
            next[g] = cur[g].lerp(avg, factor);
        }
        // Write the welded result back to every member (keeps seams shut).
        for (i, &c) in canon.iter().enumerate() {
            mesh.positions[i] = next[c as usize].to_array();
        }
    }
}

/// Reflect across the plane through the origin with normal `axis`, keeping both
/// halves (duplicate + mirrored, winding flipped to preserve outward normals).
fn mirror(mesh: &mut MeshData, axis: Axis) {
    let a = axis_index(axis);
    let base = mesh.positions.len() as u32;
    let mut mirrored: Vec<[f32; 3]> = mesh
        .positions
        .iter()
        .map(|p| {
            let mut q = *p;
            q[a] = -q[a];
            q
        })
        .collect();
    mesh.positions.append(&mut mirrored);
    if let Some(uvs) = &mut mesh.uvs {
        let mut dup = uvs.clone();
        uvs.append(&mut dup);
    }
    if let Some(colors) = &mut mesh.colors {
        let mut dup = colors.clone();
        colors.append(&mut dup);
    }
    let extra: Vec<u32> = mesh
        .indices
        .chunks_exact(3)
        .flat_map(|t| [t[0] + base, t[2] + base, t[1] + base]) // flipped winding
        .collect();
    mesh.indices.extend(extra);
    mesh.normals = None;
}

/// Linear array: `count` copies, each offset `offset` from the previous.
fn array(mesh: &mut MeshData, count: u32, offset: [f32; 3]) {
    if count <= 1 {
        return;
    }
    let base_positions = mesh.positions.clone();
    let base_indices = mesh.indices.clone();
    let base_uvs = mesh.uvs.clone();
    let base_colors = mesh.colors.clone();
    let vert_count = base_positions.len() as u32;
    let off = Vec3::from_array(offset);
    for i in 1..count {
        let shift = off * i as f32;
        for p in &base_positions {
            mesh.positions
                .push((Vec3::from_array(*p) + shift).to_array());
        }
        if let (Some(dst), Some(src)) = (&mut mesh.uvs, &base_uvs) {
            dst.extend_from_slice(src);
        }
        if let (Some(dst), Some(src)) = (&mut mesh.colors, &base_colors) {
            dst.extend_from_slice(src);
        }
        let shift_idx = vert_count * i;
        for idx in &base_indices {
            mesh.indices.push(idx + shift_idx);
        }
    }
    mesh.normals = None;
}

// ───────────────────────── helpers ─────────────────────────

/// Quantize positions and assign each vertex a **canonical weld index** so that
/// coincident-but-split vertices (UV-sphere seams/poles, per-face box corners)
/// share one node. Used by weld-sensitive deformers (`roughen`, `smooth`) to
/// keep the surface closed.
fn weld_indices(positions: &[[f32; 3]]) -> Vec<u32> {
    const Q: f32 = 1e4;
    let key = |p: &[f32; 3]| {
        (
            (p[0] * Q).round() as i64,
            (p[1] * Q).round() as i64,
            (p[2] * Q).round() as i64,
        )
    };
    let mut map: HashMap<(i64, i64, i64), u32> = HashMap::new();
    positions
        .iter()
        .map(|p| {
            let next = map.len() as u32;
            *map.entry(key(p)).or_insert(next)
        })
        .collect()
}

/// A deterministic hash of a quantized position + seed → `u32`. Coincident
/// positions hash identically (so welded vertices get identical jitter).
fn pos_hash(p: &[f32; 3], seed: u32) -> u32 {
    let q = |v: f32| (v * 1024.0).round() as i32 as u32;
    let mut h = seed.wrapping_mul(0x9E37_79B9).wrapping_add(0x1234_5678);
    for v in [p[0], p[1], p[2]] {
        h ^= q(v);
        h = h.wrapping_mul(0x85EB_CA6B);
        h ^= h >> 13;
    }
    h
}

fn other_axes(a: usize) -> (usize, usize) {
    match a {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    }
}

fn centroid(mesh: &MeshData) -> Vec3 {
    if mesh.positions.is_empty() {
        return Vec3::ZERO;
    }
    let sum: Vec3 = mesh.positions.iter().map(|p| Vec3::from_array(*p)).sum();
    sum / mesh.positions.len() as f32
}

/// A cheap deterministic hash → `[0, 1)` (Wang-style integer hash).
fn hash01(mut x: u32) -> f32 {
    x = (x ^ 61) ^ (x >> 16);
    x = x.wrapping_add(x << 3);
    x ^= x >> 4;
    x = x.wrapping_mul(0x27d4_eb2d);
    x ^= x >> 15;
    (x as f32) / (u32::MAX as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bbox(m: &MeshData) -> ([f32; 3], [f32; 3]) {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for p in &m.positions {
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
        }
        (lo, hi)
    }

    fn cube() -> MeshData {
        box_mesh(Vec3::splat(2.0))
    }

    #[test]
    fn lathe_closed_surface_counts() {
        let m = lathe(&[[0.0, 1.0], [1.0, 1.0]], 8, TAU);
        // 2 rows × 8 cols (closed wraps, no duplicate seam column).
        assert_eq!(m.positions.len(), 16);
        // 8 quads × 2 tris × 3 = 48 indices.
        assert_eq!(m.indices.len(), 48);
        assert!(m.normals.is_some());
    }

    #[test]
    fn superquadric_box_is_boxy() {
        // (e1,e2) → 0 approximates a box: corners reach ~±1 on all axes.
        let m = superquadric(0.1, 0.1, 12, 8);
        let (lo, hi) = bbox(&m);
        for i in 0..3 {
            assert!(hi[i] > 0.8 && lo[i] < -0.8, "axis {i}: {lo:?}..{hi:?}");
        }
    }

    #[test]
    fn twist_preserves_vertex_count_and_changes_shape() {
        let before = cube();
        let mut m = cube();
        twist(&mut m, Axis::Y, 0.25);
        assert_eq!(m.positions.len(), before.positions.len());
        // A quarter turn over the height moves the top corners off their original
        // xz (a full turn would return them, so don't use 1.0 here).
        assert!(m
            .positions
            .iter()
            .zip(&before.positions)
            .any(|(a, b)| (a[0] - b[0]).abs() > 0.1 || (a[2] - b[2]).abs() > 0.1));
    }

    #[test]
    fn taper_shrinks_top() {
        let mut m = cube();
        taper(&mut m, Axis::Y, 0.0); // top collapses to the axis
        let (lo, hi) = bbox(&m);
        // Top ring (y = +1) tapered to ~0 width, bottom (y = -1) unchanged (±1).
        assert!(hi[0] <= 1.01 && lo[0] >= -1.01);
        // Some vertex near the top is now close to the axis.
        assert!(m
            .positions
            .iter()
            .any(|p| p[1] > 0.5 && p[0].abs() < 0.2 && p[2].abs() < 0.2));
    }

    #[test]
    fn inflate_grows_bounds() {
        let mut m = cube();
        m.compute_vertex_normals();
        let (lo0, hi0) = bbox(&m);
        inflate(&mut m, 0.5);
        let (lo1, hi1) = bbox(&m);
        assert!(hi1[0] > hi0[0] && lo1[0] < lo0[0]);
    }

    #[test]
    fn spherify_moves_corners_onto_sphere() {
        let mut m = cube();
        let r = mesh_radius(&m);
        spherify(&mut m, 1.0);
        let c = centroid(&m);
        for p in &m.positions {
            let d = (Vec3::from_array(*p) - c).length();
            assert!((d - r).abs() < 1e-3, "vertex off sphere: {d} vs {r}");
        }
    }

    #[test]
    fn subdivide_quadruples_triangles() {
        let mut m = cube();
        let tris = m.indices.len() / 3;
        subdivide(&mut m);
        assert_eq!(m.indices.len() / 3, tris * 4);
    }

    #[test]
    fn array_multiplies_geometry() {
        let mut m = cube();
        let (v, t) = (m.positions.len(), m.indices.len());
        array(&mut m, 3, [4.0, 0.0, 0.0]);
        assert_eq!(m.positions.len(), v * 3);
        assert_eq!(m.indices.len(), t * 3);
        // The third copy sits ~8 units along +X.
        let (_, hi) = bbox(&m);
        assert!(hi[0] > 8.0);
    }

    #[test]
    fn mirror_doubles_and_reflects() {
        let mut m = cube();
        let v = m.positions.len();
        mirror(&mut m, Axis::X);
        assert_eq!(m.positions.len(), v * 2);
        assert_eq!(m.indices.len() % 3, 0);
    }

    #[test]
    fn displace_constant_matches_inflate() {
        // A constant formula displaces every vertex one unit along its normal —
        // equivalent to Inflate(1.0); a bad formula is a no-op.
        let with_normals = || {
            let mut m = cube();
            m.compute_vertex_normals();
            m
        };
        let mut a = with_normals();
        let mut b = with_normals();
        displace(&mut a, "1");
        inflate(&mut b, 1.0);
        for (pa, pb) in a.positions.iter().zip(&b.positions) {
            for k in 0..3 {
                assert!((pa[k] - pb[k]).abs() < 1e-5);
            }
        }
        let before = with_normals();
        let mut bad = with_normals();
        displace(&mut bad, "1 +"); // malformed → no-op
        assert_eq!(bad.positions, before.positions);
    }

    #[test]
    fn roughen_keeps_the_surface_welded() {
        // Regression: index-keyed jitter split coincident corner vertices →
        // holes. A box stays watertight (welded) after roughen, and it moved.
        use crate::stats::mesh_stats;
        let mut m = cube();
        m.compute_vertex_normals();
        let before = m.positions.clone();
        roughen(&mut m, 0.1, 7);
        assert!(
            mesh_stats(&m).watertight,
            "roughen split coincident vertices (holes)"
        );
        assert!(m.positions != before, "roughen did nothing");
    }

    #[test]
    fn smooth_keeps_the_surface_welded() {
        use crate::stats::mesh_stats;
        let mut m = cube();
        smooth(&mut m, 3, 0.5);
        assert!(
            mesh_stats(&m).watertight,
            "smooth split coincident vertices (holes)"
        );
    }

    #[test]
    fn roughen_is_weld_consistent_on_a_sphere_seam() {
        // Coincident vertices (sphere seam/poles) must receive identical offsets.
        use crate::primitives::sphere_mesh;
        let mut m = sphere_mesh(1.0, 24, 16);
        m.compute_vertex_normals();
        let canon = weld_indices(&m.positions);
        roughen(&mut m, 0.08, 3);
        // After roughen, vertices that were coincident are still coincident.
        let mut group_pos: std::collections::HashMap<u32, [f32; 3]> =
            std::collections::HashMap::new();
        for (i, &c) in canon.iter().enumerate() {
            let p = m.positions[i];
            if let Some(prev) = group_pos.get(&c) {
                for k in 0..3 {
                    assert!(
                        (prev[k] - p[k]).abs() < 1e-4,
                        "coincident vertices drifted apart under roughen"
                    );
                }
            } else {
                group_pos.insert(c, p);
            }
        }
    }

    #[test]
    fn degenerate_inputs_dont_panic() {
        // Every modifier on an empty mesh — must not panic.
        let all = [
            Modifier::Taper {
                axis: Axis::Y,
                factor: 0.0,
            },
            Modifier::Twist {
                axis: Axis::X,
                turns: 2.0,
            },
            Modifier::Bend {
                axis: Axis::Z,
                angle: 3.0,
            },
            Modifier::Inflate { amount: 1.0 },
            Modifier::Spherify { factor: 1.0 },
            Modifier::Roughen {
                amount: 0.1,
                seed: 0,
            },
            Modifier::Subdivide { iterations: 2 },
            Modifier::Smooth {
                iterations: 3,
                factor: 0.5,
            },
            Modifier::Mirror { axis: Axis::X },
            Modifier::Array {
                count: 0,
                offset: [1.0, 0.0, 0.0],
            },
            Modifier::Displace {
                expr: "1 + ".into(),
            }, // malformed
        ];
        let out = apply_modifiers(MeshData::default(), &all);
        assert!(out.positions.is_empty());

        // Lathe needs ≥2 profile rows; fewer → empty (no panic / no OOB).
        assert!(lathe(&[], 8, TAU).positions.is_empty());
        assert!(lathe(&[[0.0, 1.0]], 8, TAU).positions.is_empty());
        // Superquadric clamps tiny segment counts instead of producing garbage.
        assert!(!superquadric(1.0, 1.0, 1, 1).positions.is_empty());
        // Array count 0/1 is a no-op (not a crash / empty).
        let mut b = cube();
        let v0 = b.positions.len();
        array(&mut b, 1, [1.0, 0.0, 0.0]);
        assert_eq!(b.positions.len(), v0);
        // Modifiers needing normals on a mesh without them: no-op, no panic.
        let mut nonorm = cube();
        nonorm.normals = None;
        inflate(&mut nonorm, 0.5); // returns early (no normals)
    }

    #[test]
    fn evaluate_runs_a_full_stack() {
        let stack = ModifierStack {
            base: MeshBase::Superquadric {
                e1: 1.0,
                e2: 1.0,
                segments_long: 16,
                segments_lat: 10,
            },
            modifiers: vec![
                Modifier::Twist {
                    axis: Axis::Y,
                    turns: 0.5,
                },
                Modifier::Inflate { amount: 0.1 },
                Modifier::Array {
                    count: 2,
                    offset: [3.0, 0.0, 0.0],
                },
            ],
        };
        let m = evaluate(&stack);
        assert!(!m.positions.is_empty());
        assert!(m.normals.as_ref().unwrap().len() == m.positions.len());
        assert_eq!(m.indices.len() % 3, 0);
    }

    #[test]
    fn sweep_and_captured_bases_are_empty_pending_editor() {
        // These need scene state; evaluate yields an empty base (editor resolves).
        let stack = ModifierStack {
            base: MeshBase::Captured(awsm_scene_schema::MeshRef(awsm_scene_schema::AssetId::new())),
            modifiers: vec![Modifier::Inflate { amount: 1.0 }],
        };
        assert!(evaluate(&stack).positions.is_empty());
    }

    fn mesh_radius(m: &MeshData) -> f32 {
        let c = centroid(m);
        m.positions
            .iter()
            .map(|p| (Vec3::from_array(*p) - c).length())
            .fold(0.0_f32, f32::max)
    }
}
