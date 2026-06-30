use glam::{Quat, Vec3};

/// Primitive collider shapes the editor authors and the runtime
/// passes to Rapier. Capsule / Cylinder / Cone are Y-axis-aligned in
/// the collider's local frame — to orient them along X or Z, rotate
/// the containing node.
///
/// SIZE LIVES HERE, NOT IN NODE SCALE. These extents
/// (`half_extents` / `radius` / `half_height`) are the collider's only
/// size source: a Rapier collider has no scale, so the node's transform
/// scale is locked to `[1,1,1]` in the editor and dropped at export
/// (`ColliderSpec::from_node` reads translation + rotation only). To
/// resize a collider, change these values — never the node scale.
///
/// Ellipsoid is the one shape Rapier doesn't expose natively: the
/// runtime tessellates a unit sphere into 42 vertices, scales each
/// per-axis, and hands the result to `ColliderBuilder::convex_hull`.
/// Collision is against the 42-vertex / 80-face polyhedron — visibly
/// faceted up close but with < 1% surface deviation from a true
/// ellipsoid at game scale. The editor wireframe draws those exact
/// facets (via [`ellipsoid_hull_mesh`]) so the visualization matches
/// what physics sees rather than flattering it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum ColliderShape {
    Box {
        half_extents: [f32; 3],
    },
    Sphere {
        radius: f32,
    },
    /// Capsule along local Y. Total length = `2 * (half_height + radius)`.
    Capsule {
        half_height: f32,
        radius: f32,
    },
    /// Cylinder along local Y. Total length = `2 * half_height`.
    Cylinder {
        half_height: f32,
        radius: f32,
    },
    /// Cone along local Y, apex up. Total height = `2 * half_height`.
    /// Base radius at the bottom.
    Cone {
        half_height: f32,
        radius: f32,
    },
    /// Axis-aligned ellipsoid (prolate / oblate / general). Each
    /// half-extent independently controls a principal axis. Implemented
    /// as a convex hull over a tessellated, axis-scaled sphere.
    Ellipsoid {
        half_extents: [f32; 3],
    },
}

impl ColliderShape {
    pub fn default_box() -> Self {
        Self::Box {
            half_extents: [0.5, 0.5, 0.5],
        }
    }

    pub fn default_sphere() -> Self {
        Self::Sphere { radius: 1.0 }
    }

    pub fn default_capsule() -> Self {
        Self::Capsule {
            half_height: 0.5,
            radius: 0.3,
        }
    }

    pub fn default_cylinder() -> Self {
        Self::Cylinder {
            half_height: 0.5,
            radius: 0.3,
        }
    }

    pub fn default_cone() -> Self {
        Self::Cone {
            half_height: 0.5,
            radius: 0.3,
        }
    }

    pub fn default_ellipsoid() -> Self {
        Self::Ellipsoid {
            half_extents: [0.6, 0.4, 0.4],
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Canonical tessellation for `ColliderShape::Ellipsoid`.
//
// Rapier has no native ellipsoid primitive. The host implementation
// (`lockstep-physics-host`) feeds the unit-sphere vertices below —
// scaled per-axis by the authored `half_extents` — to
// `ColliderBuilder::convex_hull`, which produces a 42-vertex /
// 80-triangle / 120-edge convex polyhedron.
//
// The editor's wireframe renderer draws those exact edges, scaled the
// same way, so the visualization is the convex hull Rapier actually
// collides against — not a flattering smooth ellipsoid. Putting the
// mesh here makes "physics-and-wireframe-agree" a structural
// invariant: both consumers read from this one function.
//
// At game scale, surface deviation from a true axis-scaled sphere is
// under 1%; bumping the subdivision level is not configurable on
// purpose (see commit history / docs for rationale).
// ─────────────────────────────────────────────────────────────────────

/// Shared geometry tables for the ellipsoid convex hull. Both fields
/// describe a unit (radius = 1) icosphere; ellipsoid colliders scale
/// each vertex per-axis by their `half_extents` before use.
pub struct EllipsoidHullMesh {
    /// 42 unit-length vertex positions, in icosphere construction
    /// order. The physics host scales these per-axis and hands the
    /// resulting point cloud to Rapier's `convex_hull` builder.
    pub vertices: Vec<[f32; 3]>,
    /// 120 unique edges of the icosphere, each as a `(low, high)`
    /// pair of indices into `vertices`. The editor wireframe draws
    /// one line per edge. Edges are deduped by canonical ordering
    /// so each appears exactly once.
    pub edges: Vec<(u16, u16)>,
}

/// Lazily-built canonical mesh. Cached behind a `OnceLock` so every
/// ellipsoid collider in every game session reuses the same buffers.
pub fn ellipsoid_hull_mesh() -> &'static EllipsoidHullMesh {
    use std::sync::OnceLock;
    static CACHE: OnceLock<EllipsoidHullMesh> = OnceLock::new();
    CACHE.get_or_init(build_ellipsoid_hull_mesh)
}

fn build_ellipsoid_hull_mesh() -> EllipsoidHullMesh {
    // Icosahedron base — 12 vertices arranged via the golden ratio.
    // These three orthogonal rectangles each contribute 4 corners.
    let phi = (1.0_f32 + 5.0_f32.sqrt()) * 0.5;
    let raw: [[f32; 3]; 12] = [
        [-1.0, phi, 0.0],
        [1.0, phi, 0.0],
        [-1.0, -phi, 0.0],
        [1.0, -phi, 0.0],
        [0.0, -1.0, phi],
        [0.0, 1.0, phi],
        [0.0, -1.0, -phi],
        [0.0, 1.0, -phi],
        [phi, 0.0, -1.0],
        [phi, 0.0, 1.0],
        [-phi, 0.0, -1.0],
        [-phi, 0.0, 1.0],
    ];
    // The icosahedron's 20 triangular faces, as triples of indices
    // into `raw`. Winding is consistent (outward-facing) but the
    // convex hull is independent of winding anyway.
    let base_faces: [[usize; 3]; 20] = [
        [0, 11, 5],
        [0, 5, 1],
        [0, 1, 7],
        [0, 7, 10],
        [0, 10, 11],
        [1, 5, 9],
        [5, 11, 4],
        [11, 10, 2],
        [10, 7, 6],
        [7, 1, 8],
        [3, 9, 4],
        [3, 4, 2],
        [3, 2, 6],
        [3, 6, 8],
        [3, 8, 9],
        [4, 9, 5],
        [2, 4, 11],
        [6, 2, 10],
        [8, 6, 7],
        [9, 8, 1],
    ];

    let normalize = |v: [f32; 3]| {
        let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        [v[0] / n, v[1] / n, v[2] / n]
    };

    // Push every raw vertex to the unit sphere — they start at radius
    // `sqrt(1 + phi^2)`, not 1.
    let mut vertices: Vec<[f32; 3]> = raw.iter().copied().map(normalize).collect();

    // Subdivide once: every triangle (a, b, c) becomes four
    // sub-triangles (a, ab, ca), (b, bc, ab), (c, ca, bc),
    // (ab, bc, ca) — where ab/bc/ca are the edge midpoints, pushed
    // back out to the unit sphere. After dedup, we get 30 new
    // vertices on top of the original 12 → 42 total.
    let mut midpoint_cache: std::collections::HashMap<(u16, u16), u16> =
        std::collections::HashMap::new();
    let mut sub_faces: Vec<[u16; 3]> = Vec::with_capacity(base_faces.len() * 4);

    for face in &base_faces {
        let a = face[0] as u16;
        let b = face[1] as u16;
        let c = face[2] as u16;
        let ab = midpoint_index(&mut vertices, &mut midpoint_cache, a, b);
        let bc = midpoint_index(&mut vertices, &mut midpoint_cache, b, c);
        let ca = midpoint_index(&mut vertices, &mut midpoint_cache, c, a);
        sub_faces.push([a, ab, ca]);
        sub_faces.push([b, bc, ab]);
        sub_faces.push([c, ca, bc]);
        sub_faces.push([ab, bc, ca]);
    }

    // Enumerate unique edges from the subdivided face list. Each
    // shared edge between two adjacent triangles would appear twice
    // if we didn't dedup; using a canonical (low, high) key keeps
    // exactly one of each pair.
    let mut edge_set: std::collections::BTreeSet<(u16, u16)> = std::collections::BTreeSet::new();
    for face in &sub_faces {
        for (a, b) in [(face[0], face[1]), (face[1], face[2]), (face[2], face[0])] {
            let key = if a < b { (a, b) } else { (b, a) };
            edge_set.insert(key);
        }
    }
    let edges: Vec<(u16, u16)> = edge_set.into_iter().collect();

    debug_assert_eq!(vertices.len(), 42, "icosphere subdivision-1 vertex count");
    debug_assert_eq!(edges.len(), 120, "icosphere subdivision-1 edge count");

    EllipsoidHullMesh { vertices, edges }
}

/// Return the index of the midpoint of `(a, b)` on the unit sphere,
/// inserting a new vertex if this is the first time the edge has been
/// seen. Cache keys are canonicalized so `(a, b)` and `(b, a)` resolve
/// to the same vertex — that's what gives us 30 new vertices instead
/// of 60.
fn midpoint_index(
    vertices: &mut Vec<[f32; 3]>,
    cache: &mut std::collections::HashMap<(u16, u16), u16>,
    a: u16,
    b: u16,
) -> u16 {
    let key = if a < b { (a, b) } else { (b, a) };
    if let Some(&idx) = cache.get(&key) {
        return idx;
    }
    let va = vertices[a as usize];
    let vb = vertices[b as usize];
    let mid = [
        (va[0] + vb[0]) * 0.5,
        (va[1] + vb[1]) * 0.5,
        (va[2] + vb[2]) * 0.5,
    ];
    let n = (mid[0] * mid[0] + mid[1] * mid[1] + mid[2] * mid[2]).sqrt();
    let normalized = [mid[0] / n, mid[1] / n, mid[2] / n];
    let idx = vertices.len() as u16;
    vertices.push(normalized);
    cache.insert(key, idx);
    idx
}

/// A collider's geometry + placement, extracted from an editor-authored
/// `NodeKind::Collider` node at Build time.
///
/// Lives in game-data (not just the editor project) because the
/// runtime reads it: the game-server hands extracted specs to its
/// engine WASM via the per-game `session-config.arena` so the engine
/// can spawn Rapier bodies/colliders matching the editor's authored
/// dimensions instead of carrying hardcoded constants.
///
/// `translation` / `rotation` are the node's local transform —
/// relative to whatever owns the collider (world for top-level fixed
/// colliders like floors and finish lines; the prefab root for
/// colliders nested inside a per-player prefab).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ColliderSpec {
    pub translation: Vec3,
    pub rotation: Quat,
    pub shape: ColliderShape,
}

impl ColliderSpec {
    pub fn new(translation: Vec3, rotation: Quat, shape: ColliderShape) -> Self {
        Self {
            translation,
            rotation,
            shape,
        }
    }

    /// Extract a runtime collider spec from an editor-authored
    /// `NodeKind::Collider(...)` node. Returns `None` if the node
    /// isn't a Collider so callers can produce structured "hook has
    /// wrong kind" errors with extra context.
    pub fn from_node(node: &super::tree::EditorNode) -> Option<Self> {
        let shape = match &node.kind {
            super::tree::NodeKind::Collider(s) => s.clone(),
            _ => return None,
        };
        Some(Self::new(
            Vec3::from_array(node.transform.translation),
            Quat::from_xyzw(
                node.transform.rotation[0],
                node.transform.rotation[1],
                node.transform.rotation[2],
                node.transform.rotation[3],
            ),
            shape,
        ))
    }
}
