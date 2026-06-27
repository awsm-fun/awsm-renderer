//! Boundary-locked half-edge QEM simplification.
//!
//! The simplifier collapses each chosen edge onto **one of its two existing
//! endpoints** — it never synthesises a new vertex position. Consequently the
//! surviving vertices are always a *subset* of the input vertices, identified by
//! their original indices ([`SimplifiedMesh::surviving`]). That subset property
//! is the whole point: a caller can carry *any* per-vertex attribute through a
//! level verbatim — positions, normals, UVs, colours, **skin JOINTS/WEIGHTS, and
//! morph-target deltas** — with [`SimplifiedMesh::gather`], no interpolation and
//! no chance of corrupting a blend shape or skin binding.
//!
//! Boundary and attribute-seam vertices are *locked* (never removed). In the
//! index topology a UV/material seam appears as a one-sided ("boundary") edge,
//! so the single rule "lock any vertex on a boundary edge" preserves both the
//! open-mesh silhouette and every attribute seam, keeping levels stable.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use glam::DVec3;

use crate::quadric::{triangle_quadric, Quadric};

/// Tuning knobs for [`simplify`].
#[derive(Clone, Copy, Debug)]
pub struct SimplifyOptions {
    /// Stop collapsing once the live triangle count reaches this. Clamped to at
    /// least 1.
    pub target_triangles: usize,
    /// Reject a collapse if it would rotate any surviving incident triangle's
    /// normal so that `dot(old, new) < this`. `1.0` forbids any rotation; `-1.0`
    /// allows full flips. Default `0.2` (≈ keep within ~78°), which blocks the
    /// fold-overs that produce visible artefacts while still allowing aggressive
    /// simplification of flat-ish regions.
    pub flip_threshold: f64,
    /// Fully lock **every** boundary vertex (never remove or slide it), instead
    /// of letting straight-seam ("Boundary") vertices slide along the seam. The
    /// **cluster DAG** needs this: it simplifies each group in isolation, and two
    /// adjacent groups must lock their shared boundary *identically* or the cut
    /// cracks between them. Sliding lets them disagree ⇒ holes. Because the DAG
    /// path position-welds first (attribute seams become interior edges, no longer
    /// one-sided), locking the *remaining* true boundaries no longer re-triggers
    /// the seam-plateau that motivated sliding. The discrete LOD chain leaves this
    /// `false` (whole-mesh, no cross-group seams) to keep its simplification rate.
    pub lock_boundaries: bool,
}

impl SimplifyOptions {
    pub fn with_target(target_triangles: usize) -> Self {
        Self {
            target_triangles: target_triangles.max(1),
            flip_threshold: 0.2,
            lock_boundaries: false,
        }
    }

    /// Like [`Self::with_target`] but fully locks boundary vertices — the
    /// crack-free mode used by the cluster-DAG group simplify.
    pub fn with_target_locked(target_triangles: usize) -> Self {
        Self {
            lock_boundaries: true,
            ..Self::with_target(target_triangles)
        }
    }
}

/// Result of one simplification pass.
#[derive(Clone, Debug)]
pub struct SimplifiedMesh {
    /// Original-vertex indices that survive, in ascending order. The new vertex
    /// buffer is exactly these vertices; [`Self::indices`] addresses into it.
    pub surviving: Vec<u32>,
    /// Triangle list (triplets) indexing into the *compacted* vertex buffer,
    /// i.e. into `[0, surviving.len())`.
    pub indices: Vec<u32>,
    /// Object-space geometric error estimate: the square root of the largest
    /// QEM cost actually paid by an accepted collapse (0 if nothing collapsed).
    pub error: f32,
}

impl SimplifiedMesh {
    /// Triangle count of the simplified mesh.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Gather a per-original-vertex attribute array down to this level's
    /// surviving vertices, in compacted order. `attr` must be indexed by
    /// original vertex id (length == original vertex count).
    pub fn gather<T: Copy>(&self, attr: &[T]) -> Vec<T> {
        self.surviving.iter().map(|&i| attr[i as usize]).collect()
    }

    /// An identity ("no simplification") result over `vertex_count` vertices and
    /// the given index buffer — used when a mesh is below the simplify floor.
    pub fn identity(vertex_count: usize, indices: &[u32]) -> Self {
        Self {
            surviving: (0..vertex_count as u32).collect(),
            indices: indices.to_vec(),
            error: 0.0,
        }
    }
}

/// Min-heap entry for a candidate collapse "remove `from`, keep `to`".
struct Candidate {
    cost: f64,
    from: u32,
    to: u32,
    from_ver: u64,
    to_ver: u64,
}
impl PartialEq for Candidate {
    fn eq(&self, o: &Self) -> bool {
        self.cost == o.cost
    }
}
impl Eq for Candidate {}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Candidate {
    fn cmp(&self, o: &Self) -> Ordering {
        // Reverse so BinaryHeap (a max-heap) yields the *cheapest* collapse.
        // Break ties by endpoints so the whole simplification is deterministic
        // (no HashMap-iteration-order dependence) — required for reproducible
        // bakes / content-hash caching.
        o.cost
            .partial_cmp(&self.cost)
            .unwrap_or(Ordering::Equal)
            .then_with(|| o.from.cmp(&self.from))
            .then_with(|| o.to.cmp(&self.to))
    }
}

/// Simplify `(positions, indices)` toward `opts.target_triangles`, returning a
/// [`SimplifiedMesh`]. `positions` is indexed by vertex id; `indices` is a
/// triangle list. Non-triangle tails of `indices` are ignored.
pub fn simplify(positions: &[[f32; 3]], indices: &[u32], opts: SimplifyOptions) -> SimplifiedMesh {
    let vert_count = positions.len();
    let tri_count = indices.len() / 3;
    if tri_count == 0 || vert_count == 0 {
        return SimplifiedMesh::identity(vert_count, indices);
    }
    let target = opts.target_triangles.max(1);
    if tri_count <= target {
        return SimplifiedMesh::identity(vert_count, indices);
    }

    let pos: Vec<DVec3> = positions
        .iter()
        .map(|p| DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64))
        .collect();

    // Weld coincident positions for TOPOLOGY. Index-based adjacency would
    // otherwise treat a UV/attribute seam (two coincident-but-distinct columns)
    // or a pole fan (many coincident vertices) as an *open boundary*: the two
    // sides then classify as boundary, simplify independently, and tear holes on
    // non-watertight / subdivided meshes (e.g. a UV sphere). Mapping every vertex
    // to one canonical id per location makes a geometric seam two-sided
    // (interior) so it collapses consistently, while genuine open edges stay
    // one-sided. Survivors are canonical ids — still a subset of the input
    // vertices (coincident twins share a position, so gathering an attribute from
    // the canonical one is exact for skin/morph; only a duplicated attribute
    // *seam value*, e.g. a UV, is dropped at the coarser level). Dup-free
    // watertight meshes weld to identity ⇒ their bake output is unchanged.
    let weld = weld_coincident(&pos);

    // Mutable triangle table (canonical vertex ids) + removal flags.
    let mut tris: Vec<[u32; 3]> = (0..tri_count)
        .map(|t| {
            [
                weld[indices[t * 3] as usize],
                weld[indices[t * 3 + 1] as usize],
                weld[indices[t * 3 + 2] as usize],
            ]
        })
        .collect();
    let mut tri_dead = vec![false; tri_count];

    // Vertex → incident triangles (may hold stale entries after edits; we
    // re-validate that the triangle still references the vertex on use).
    let mut vert_tris: Vec<Vec<u32>> = vec![Vec::new(); vert_count];
    // Undirected edge → incident triangle count, for boundary detection.
    let mut edge_tris: HashMap<(u32, u32), u32> = HashMap::new();
    // Per-vertex accumulated quadric.
    let mut quad: Vec<Quadric> = vec![Quadric::default(); vert_count];

    for (t, tri) in tris.iter().enumerate() {
        let [i0, i1, i2] = *tri;
        if i0 == i1 || i1 == i2 || i0 == i2 {
            tri_dead[t] = true;
            continue;
        }
        if let Some((q, _)) = triangle_quadric(pos[i0 as usize], pos[i1 as usize], pos[i2 as usize])
        {
            quad[i0 as usize].add_assign(&q);
            quad[i1 as usize].add_assign(&q);
            quad[i2 as usize].add_assign(&q);
        }
        for &v in tri {
            vert_tris[v as usize].push(t as u32);
        }
        for (a, b) in [(i0, i1), (i1, i2), (i2, i0)] {
            *edge_tris.entry(undirected(a, b)).or_insert(0) += 1;
        }
    }

    // Classify vertices by their relation to mesh boundary / attribute-seam
    // edges (edges used by exactly one triangle). The class decides how a vertex
    // may move:
    //   - Interior: free to collapse onto any neighbour.
    //   - Boundary: a smooth point along a seam — may collapse only onto another
    //     *non-interior* vertex (slides along the seam; never pulled inward), so
    //     seams thin out but stay put.
    //   - Corner: a seam junction (≠2 boundary edges) or a sharp boundary turn —
    //     locked, so silhouette/seam corners are preserved exactly.
    // This replaces the old "lock every boundary vertex" rule, which over-locked
    // seam-heavy meshes (they plateaued ~65% of base instead of reaching target).
    let mut boundary_nbrs: Vec<Vec<u32>> = vec![Vec::new(); vert_count];
    for ((a, b), &count) in &edge_tris {
        if count == 1 {
            boundary_nbrs[*a as usize].push(*b);
            boundary_nbrs[*b as usize].push(*a);
        }
    }
    let mut class = classify_vertices(&boundary_nbrs, &pos);
    // Lock the endpoints of every non-manifold edge (used by 3+ triangles). Such an
    // edge has no well-defined two-sided collapse: merging across it folds disjoint
    // sheets together and, in cluster mode, cracks the per-cluster cut where the
    // sheets meet. count==1 is the open boundary (handled above); count==2 is the
    // manifold interior; count>=3 is the pathological case this guards. Manifold
    // meshes have no such edges, so this is a no-op for them.
    let nonmanifold = nonmanifold_locked(&edge_tris, vert_count);
    for (v, &nm) in nonmanifold.iter().enumerate() {
        if nm {
            class[v] = VertClass::Corner;
        }
    }
    if opts.lock_boundaries {
        // Crack-free cluster mode: a group's external boundary must be preserved
        // exactly so the adjacent group's matching boundary stays welded to it.
        // Promote every sliding "Boundary" vertex to a locked "Corner".
        for c in class.iter_mut() {
            if *c == VertClass::Boundary {
                *c = VertClass::Corner;
            }
        }
    }

    // Union-find over vertices: parent[v] == v while alive; otherwise points at
    // the vertex it was collapsed into.
    let mut parent: Vec<u32> = (0..vert_count as u32).collect();
    let mut version: Vec<u64> = vec![0; vert_count];

    let mut live_tris = tri_dead.iter().filter(|d| !**d).count();
    let mut max_cost = 0.0_f64;

    // Seed the heap with every undirected edge's best collapse direction, in a
    // deterministic order (HashMap iteration order is randomised) so the whole
    // collapse sequence is reproducible — required for content-hash-cached bakes.
    let mut heap: BinaryHeap<Candidate> = BinaryHeap::new();
    let mut seed_edges: Vec<(u32, u32)> = edge_tris.keys().copied().collect();
    seed_edges.sort_unstable();
    for (a, b) in seed_edges {
        if let Some(c) = candidate(a, b, &pos, &quad, &class, &version) {
            heap.push(c);
        }
    }

    while live_tris > target {
        let Some(cand) = heap.pop() else { break };
        let (from, to) = (cand.from, cand.to);
        // Skip stale entries: either endpoint already collapsed, or its
        // neighbourhood changed since this candidate was queued.
        if parent[from as usize] != from || parent[to as usize] != to {
            continue;
        }
        if version[from as usize] != cand.from_ver || version[to as usize] != cand.to_ver {
            continue;
        }
        if class[from as usize] == VertClass::Corner {
            continue; // never remove a corner
        }

        // Flip guard: collapsing `from` onto `to` rewrites every triangle that
        // uses `from` (and not `to`) to use `to` instead. Reject if any such
        // triangle would fold over.
        if would_flip(
            from,
            to,
            &tris,
            &tri_dead,
            &vert_tris,
            &pos,
            opts.flip_threshold,
        ) {
            continue;
        }

        // Accept. Record error, merge quadric, retopologise.
        max_cost = max_cost.max(cand.cost);
        let q_from = quad[from as usize];
        quad[to as usize].add_assign(&q_from);
        parent[from as usize] = to;

        // Rewrite / kill incident triangles.
        let incident = std::mem::take(&mut vert_tris[from as usize]);
        for t in incident {
            let ti = t as usize;
            if tri_dead[ti] {
                continue;
            }
            let tri = &mut tris[ti];
            if !tri.contains(&from) {
                continue; // stale entry
            }
            for slot in tri.iter_mut() {
                if *slot == from {
                    *slot = to;
                }
            }
            if tri[0] == tri[1] || tri[1] == tri[2] || tri[0] == tri[2] {
                tri_dead[ti] = true;
                live_tris -= 1;
            } else {
                vert_tris[to as usize].push(t);
            }
        }

        // `to`'s neighbourhood (and quadric) changed: invalidate its old
        // candidates and re-seed fresh ones for each current 1-ring neighbour.
        version[to as usize] += 1;
        let neighbours = one_ring(to, &tris, &tri_dead, &vert_tris);
        for w in neighbours {
            if let Some(c) = candidate(to, w, &pos, &quad, &class, &version) {
                heap.push(c);
            }
        }
    }

    finalize(vert_count, &tris, &tri_dead, &mut parent, max_cost)
}

/// Build several LOD levels from one base mesh. `ratios` are target
/// triangle-count fractions of the base (e.g. `[0.5, 0.25, 0.125]`), each
/// simplified *independently from the original* for the tightest error bound.
/// Ratios are clamped to `(0, 1]` and produce one [`SimplifiedMesh`] each.
pub fn build_lod_chain(
    positions: &[[f32; 3]],
    indices: &[u32],
    ratios: &[f32],
) -> Vec<SimplifiedMesh> {
    let base_tris = indices.len() / 3;
    ratios
        .iter()
        .map(|&r| {
            let r = r.clamp(f32::EPSILON, 1.0);
            let target = ((base_tris as f32 * r).round() as usize).max(1);
            simplify(positions, indices, SimplifyOptions::with_target(target))
        })
        .collect()
}

fn undirected(a: u32, b: u32) -> (u32, u32) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Map each vertex to a canonical id shared by all vertices at the same location
/// (within an extent-relative epsilon). The canonical id is the lowest input id
/// at that location, so the result is deterministic (required for content-hash
/// caching). Vertices with a unique position map to themselves ⇒ a mesh with no
/// coincident duplicates welds to the identity.
fn weld_coincident(pos: &[DVec3]) -> Vec<u32> {
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for p in pos {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    // Quantise to a grid fine enough to separate genuinely-distinct vertices
    // (real meshes' min spacing ≫ extent·1e-5) yet coarse enough to unify
    // coincident duplicates that differ only by float round-off (e.g. a sphere
    // seam at phi=0 vs phi=2π).
    let eps = ((hi - lo).length() * 1e-5).max(1e-12);
    let inv = 1.0 / eps;
    let mut map: HashMap<(i64, i64, i64), u32> = HashMap::new();
    pos.iter()
        .enumerate()
        .map(|(i, p)| {
            let key = (
                (p.x * inv).round() as i64,
                (p.y * inv).round() as i64,
                (p.z * inv).round() as i64,
            );
            *map.entry(key).or_insert(i as u32)
        })
        .collect()
}

/// Mark every vertex that touches a non-manifold edge — one shared by 3 or more
/// triangles. The simplifier collapses an edge by merging its endpoints; doing
/// that across a non-manifold edge welds otherwise-disjoint surface sheets and
/// tears the mesh (and cracks the cluster cut where the sheets meet). Callers lock
/// the marked vertices. `edge_tris` maps each undirected edge to its incident
/// triangle count; manifold meshes (all counts 1 or 2) yield an all-`false` mask.
fn nonmanifold_locked(edge_tris: &HashMap<(u32, u32), u32>, vert_count: usize) -> Vec<bool> {
    let mut nm = vec![false; vert_count];
    for ((a, b), &count) in edge_tris {
        if count >= 3 {
            nm[*a as usize] = true;
            nm[*b as usize] = true;
        }
    }
    nm
}

/// How a vertex is allowed to move during collapse.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum VertClass {
    /// Off any boundary — free to collapse onto any neighbour.
    Interior,
    /// A smooth point along a boundary/seam — may only collapse onto another
    /// non-interior vertex (slides along the seam, never inward).
    Boundary,
    /// A seam junction or sharp boundary turn — locked.
    Corner,
}

/// Classify each vertex from its boundary-edge neighbours. A vertex with no
/// boundary edge is `Interior`; with exactly two collinear-ish boundary edges
/// it's `Boundary` (a smooth seam point); otherwise (a junction with ≠2
/// boundary edges, or a sharp turn) it's a `Corner`.
fn classify_vertices(boundary_nbrs: &[Vec<u32>], pos: &[DVec3]) -> Vec<VertClass> {
    // A boundary turning by more than ~45° is treated as a corner. For a smooth
    // seam the two edge directions are nearly opposite (dot ≈ -1); the turn
    // exceeds 45° once dot(d1, d2) ≥ -cos(45°).
    const STRAIGHT_DOT: f64 = -std::f64::consts::FRAC_1_SQRT_2;
    boundary_nbrs
        .iter()
        .enumerate()
        .map(|(v, nb)| {
            if nb.is_empty() {
                VertClass::Interior
            } else if nb.len() != 2 {
                VertClass::Corner
            } else {
                let d1 = (pos[nb[0] as usize] - pos[v]).normalize_or_zero();
                let d2 = (pos[nb[1] as usize] - pos[v]).normalize_or_zero();
                if d1.dot(d2) >= STRAIGHT_DOT {
                    VertClass::Corner
                } else {
                    VertClass::Boundary
                }
            }
        })
        .collect()
}

/// Best collapse for undirected edge `(a, b)`: pick a valid removable endpoint
/// (the cheaper one if both directions are valid). A `Corner` is never removed;
/// a `Boundary` vertex may only be removed when the kept vertex is also
/// non-interior (so the seam slides along itself, never inward). Returns `None`
/// if neither direction is allowed.
fn candidate(
    a: u32,
    b: u32,
    pos: &[DVec3],
    quad: &[Quadric],
    class: &[VertClass],
    version: &[u64],
) -> Option<Candidate> {
    // Can we remove `x` while keeping `y`?
    let removable = |x: u32, y: u32| match class[x as usize] {
        VertClass::Corner => false,
        VertClass::Interior => true,
        VertClass::Boundary => class[y as usize] != VertClass::Interior,
    };
    let ra_ok = removable(a, b); // remove a, keep b
    let rb_ok = removable(b, a); // remove b, keep a
    if !ra_ok && !rb_ok {
        return None;
    }

    let mut combined = quad[a as usize];
    combined.add_assign(&quad[b as usize]);

    // cost of "remove x, keep y" = combined error evaluated at y's kept position.
    let make = |from: u32, to: u32| Candidate {
        cost: combined.error_at(pos[to as usize]),
        from,
        to,
        from_ver: version[from as usize],
        to_ver: version[to as usize],
    };

    Some(match (ra_ok, rb_ok) {
        (true, false) => make(a, b),
        (false, true) => make(b, a),
        _ => {
            let ra = make(a, b);
            let rb = make(b, a);
            if ra.cost <= rb.cost {
                ra
            } else {
                rb
            }
        }
    })
}

/// Would collapsing `from` onto `to` fold any surviving incident triangle?
fn would_flip(
    from: u32,
    to: u32,
    tris: &[[u32; 3]],
    tri_dead: &[bool],
    vert_tris: &[Vec<u32>],
    pos: &[DVec3],
    threshold: f64,
) -> bool {
    for &t in &vert_tris[from as usize] {
        let ti = t as usize;
        if tri_dead[ti] {
            continue;
        }
        let tri = tris[ti];
        if !tri.contains(&from) || tri.contains(&to) {
            // stale, or a triangle on the collapsing edge (will degenerate away).
            continue;
        }
        let before = face_normal(tri, pos);
        let after_tri = tri.map(|v| if v == from { to } else { v });
        let after = face_normal(after_tri, pos);
        match (before, after) {
            (Some(b), Some(a)) if b.dot(a) < threshold => {
                return true;
            }
            // A triangle that was valid but becomes degenerate (zero area) is a
            // sliver — treat as a flip and reject.
            (Some(_), None) => return true,
            _ => {}
        }
    }
    false
}

fn face_normal(tri: [u32; 3], pos: &[DVec3]) -> Option<DVec3> {
    let n = (pos[tri[1] as usize] - pos[tri[0] as usize])
        .cross(pos[tri[2] as usize] - pos[tri[0] as usize]);
    let len = n.length();
    if len <= f64::EPSILON {
        None
    } else {
        Some(n / len)
    }
}

/// Distinct live 1-ring neighbours of `v`.
fn one_ring(v: u32, tris: &[[u32; 3]], tri_dead: &[bool], vert_tris: &[Vec<u32>]) -> Vec<u32> {
    let mut out = Vec::new();
    for &t in &vert_tris[v as usize] {
        let ti = t as usize;
        if tri_dead[ti] {
            continue;
        }
        let tri = tris[ti];
        if !tri.contains(&v) {
            continue;
        }
        for &w in &tri {
            if w != v && !out.contains(&w) {
                out.push(w);
            }
        }
    }
    out
}

/// Resolve the union-find, drop degenerate triangles, and compact surviving
/// vertices into a fresh, ascending buffer + remapped index list.
fn finalize(
    vert_count: usize,
    tris: &[[u32; 3]],
    tri_dead: &[bool],
    parent: &mut [u32],
    max_cost: f64,
) -> SimplifiedMesh {
    fn find(parent: &mut [u32], mut x: u32) -> u32 {
        while parent[x as usize] != x {
            parent[x as usize] = parent[parent[x as usize] as usize];
            x = parent[x as usize];
        }
        x
    }

    let mut used = vec![false; vert_count];
    let mut resolved: Vec<[u32; 3]> = Vec::new();
    for (t, tri) in tris.iter().enumerate() {
        if tri_dead[t] {
            continue;
        }
        let r = [
            find(parent, tri[0]),
            find(parent, tri[1]),
            find(parent, tri[2]),
        ];
        if r[0] == r[1] || r[1] == r[2] || r[0] == r[2] {
            continue;
        }
        used[r[0] as usize] = true;
        used[r[1] as usize] = true;
        used[r[2] as usize] = true;
        resolved.push(r);
    }

    // Compact: surviving original ids ascending → new compact id.
    let mut surviving: Vec<u32> = Vec::new();
    let mut remap = vec![u32::MAX; vert_count];
    for (v, &is_used) in used.iter().enumerate() {
        if is_used {
            remap[v] = surviving.len() as u32;
            surviving.push(v as u32);
        }
    }
    let mut out_indices: Vec<u32> = Vec::with_capacity(resolved.len() * 3);
    for r in resolved {
        out_indices.push(remap[r[0] as usize]);
        out_indices.push(remap[r[1] as usize]);
        out_indices.push(remap[r[2] as usize]);
    }

    SimplifiedMesh {
        surviving,
        indices: out_indices,
        error: max_cost.max(0.0).sqrt() as f32,
    }
}

#[cfg(test)]
mod nonmanifold_tests {
    use super::*;

    /// The lock mask flags exactly the endpoints of edges used by 3+ triangles;
    /// manifold (count 1/2) edges leave their vertices unflagged.
    #[test]
    fn nonmanifold_locked_flags_only_high_valence_edges() {
        let mut edge_tris: HashMap<(u32, u32), u32> = HashMap::new();
        edge_tris.insert((0, 1), 3); // non-manifold spine → 0,1 locked
        edge_tris.insert((1, 2), 2); // manifold interior
        edge_tris.insert((2, 3), 1); // open boundary (locked elsewhere, not here)
        edge_tris.insert((4, 5), 4); // non-manifold → 4,5 locked
        let nm = nonmanifold_locked(&edge_tris, 6);
        assert_eq!(nm, vec![true, true, false, false, true, true]);
    }

    /// A "Y-prism": three flaps share one spine edge (count==3, non-manifold).
    /// The simplifier must not panic, must emit a valid index buffer, and must
    /// keep the spine endpoints (locked, so the disjoint sheets never fold
    /// together / tear). Guards the whole path on non-manifold input.
    #[test]
    fn nonmanifold_yprism_simplifies_without_tearing() {
        // Spine S0,S1 extruded along z; three flaps at 120°.
        let mut pos = vec![[0.0f32, 0.0, 0.0], [0.0, 0.0, 1.0]];
        let dirs = [
            (1.0f32, 0.0f32),
            (-0.5, 0.8660254),
            (-0.5, -0.8660254),
        ];
        let mut indices = Vec::new();
        for (dx, dy) in dirs {
            let a0 = pos.len() as u32;
            pos.push([dx, dy, 0.0]);
            let a1 = pos.len() as u32;
            pos.push([dx, dy, 1.0]);
            // quad [S0,S1,a1,a0] → (S0,S1,a1),(S0,a1,a0); edge S0-S1 shared by all flaps.
            indices.extend_from_slice(&[0, 1, a1]);
            indices.extend_from_slice(&[0, a1, a0]);
        }
        let sm = simplify(&pos, &indices, SimplifyOptions::with_target_locked(1));
        // Valid, non-empty output.
        assert!(!sm.indices.is_empty());
        assert!(sm.indices.iter().all(|&i| (i as usize) < sm.surviving.len()));
        // The spine endpoints (original ids 0 and 1) survive — non-manifold-locked.
        assert!(sm.surviving.contains(&0), "spine S0 collapsed away");
        assert!(sm.surviving.contains(&1), "spine S1 collapsed away");
    }
}
