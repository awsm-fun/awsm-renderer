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
    /// Convex hull over an arbitrary point cloud, collider-local.
    ///
    /// Points, not faces: the physics host recomputes the hull from the
    /// same cloud (Box3D `b3CreateHull`), and the editor wireframe
    /// re-solves it for edges — one source of truth, no face table to
    /// drift. Authored by fitting to a source node's meshes (the editor's
    /// "fit hull" flow); 4..=254 points (physics hull indices are 8-bit),
    /// though fitted hulls aim far lower for solver health.
    ConvexHull {
        points: Vec<[f32; 3]>,
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

    /// A unit-ish tetrahedron — the smallest legal hull, as the blank-slate
    /// default (real hulls come from the fit-to-mesh flow).
    pub fn default_convex_hull() -> Self {
        Self::ConvexHull {
            points: vec![
                [0.5, -0.35, 0.5],
                [-0.5, -0.35, 0.5],
                [0.0, -0.35, -0.5],
                [0.0, 0.65, 0.0],
            ],
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The hull's point list must survive project.toml/scene.toml verbatim —
    /// it IS the collision geometry (same pattern as the instancer's
    /// transform list round-trip in tree.rs).
    #[test]
    fn convex_hull_toml_round_trips() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Doc {
            shape: ColliderShape,
        }
        let doc = Doc {
            shape: ColliderShape::ConvexHull {
                points: vec![
                    [0.125, -1.5, 3.75],
                    [-0.25, 0.0, 0.5],
                    [1.0, 2.0, -3.0],
                    [0.0, 0.75, 0.0],
                    [0.5, -0.5, 0.5],
                ],
            },
        };
        let text = toml::to_string(&doc).expect("serialize");
        let back: Doc = toml::from_str(&text).expect("deserialize");
        assert_eq!(back, doc);
    }
}

// ─────────────────────────── physics parameters ────────────────────────────

/// Contact parameters for a collider: a **universal core** every engine
/// understands, plus an optional per-engine extension block.
///
/// Two-tier on purpose. The core is what an author actually reasons about
/// ("slippery", "bouncy", "what does this hit"), and it means the same thing
/// everywhere. The extension blocks hold the knobs that only exist in one
/// engine's contact model and have no honest cross-engine equivalent — putting
/// them in the core would imply a portability that isn't there.
///
/// **Portability caveat**: engines combine *pairwise* friction differently —
/// MuJoCo takes the element-wise max (modulo geom priority), Rapier averages,
/// Box2D takes the geometric mean. Identical authored values therefore do NOT
/// produce identical contact behaviour across engines, and no amount of schema
/// design fixes that; it is a property of the solvers.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct PhysicsParams {
    /// Sliding (tangential) friction coefficient. 0 = frictionless.
    #[serde(default = "default_friction")]
    pub friction: f32,
    /// Bounciness, 0..1.
    ///
    /// **MuJoCo has no restitution parameter at all** — bounce and softness live
    /// in its soft-constraint model (`solref`/`solimp`), so a harness maps this
    /// only approximately. It is in the core anyway because every other engine
    /// does have it and authors expect it.
    #[serde(default)]
    pub restitution: f32,
    /// Which collision layers this collider belongs to (bitmask).
    /// A MuJoCo harness encodes this as `contype`.
    #[serde(default = "default_mask")]
    pub layer: u32,
    /// Which layers this collider collides WITH (bitmask).
    /// A MuJoCo harness encodes this as `conaffinity`.
    #[serde(default = "default_mask")]
    pub mask: u32,
    /// kg/m³. Reserved for future dynamic bodies — static colliders ignore it.
    /// `None` leaves the engine's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<f32>,
    /// MuJoCo-specific contact parameters. Absent ⇒ MuJoCo's own defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mujoco: Option<MujocoPhysics>,
}

fn default_friction() -> f32 {
    1.0
}

/// Layer/mask default: in every layer, colliding with every layer — matching
/// MuJoCo's own `contype`/`conaffinity` default of 1 broadened to "everything",
/// so an author who never touches layers gets collisions rather than silence.
fn default_mask() -> u32 {
    u32::MAX
}

impl Default for PhysicsParams {
    fn default() -> Self {
        Self {
            friction: default_friction(),
            restitution: 0.0,
            layer: default_mask(),
            mask: default_mask(),
            density: None,
            mujoco: None,
        }
    }
}

/// MuJoCo's contact knobs that have no cross-engine equivalent.
///
/// Defaults here are MuJoCo's own, so an authored block that touches one field
/// doesn't silently re-specify the rest.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct MujocoPhysics {
    /// Torsional friction — resists spinning about the contact normal. Only has
    /// an effect at `condim` >= 4.
    #[serde(default = "default_torsional")]
    pub torsional_friction: f32,
    /// Rolling friction — resists rolling. Only has an effect at `condim` 6.
    #[serde(default = "default_rolling")]
    pub rolling_friction: f32,
    /// Contact dimensionality: 1 (frictionless), 3 (sliding), 4 (+ torsional),
    /// 6 (+ rolling). The torsional/rolling coefficients above are inert below
    /// their respective thresholds, which is the usual reason a "sticky" value
    /// appears to do nothing.
    #[serde(default = "default_condim")]
    pub condim: u32,
    /// Constraint solver reference `[timeconst, dampratio]` — where MuJoCo's
    /// bounce and softness actually live (see [`PhysicsParams::restitution`]).
    #[serde(default = "default_solref")]
    pub solref: [f32; 2],
    /// Constraint solver impedance `[dmin, dmax, width, midpoint, power]`.
    #[serde(default = "default_solimp")]
    pub solimp: [f32; 5],
    /// Contact detection margin, metres. Contacts are generated at this
    /// distance; those closer than `gap` are detected but not enforced.
    #[serde(default)]
    pub margin: f32,
    /// Of the `margin` band, how much is detection-only (no force).
    #[serde(default)]
    pub gap: f32,
    /// Geom priority. When two geoms differ in priority, the higher one's
    /// friction wins outright instead of being combined — the escape hatch from
    /// the element-wise-max rule.
    #[serde(default)]
    pub priority: i32,
}

fn default_torsional() -> f32 {
    0.005
}
fn default_rolling() -> f32 {
    0.0001
}
fn default_condim() -> u32 {
    3
}
fn default_solref() -> [f32; 2] {
    [0.02, 1.0]
}
fn default_solimp() -> [f32; 5] {
    [0.9, 0.95, 0.001, 0.5, 2.0]
}

impl Default for MujocoPhysics {
    fn default() -> Self {
        Self {
            torsional_friction: default_torsional(),
            rolling_friction: default_rolling(),
            condim: default_condim(),
            solref: default_solref(),
            solimp: default_solimp(),
            margin: 0.0,
            gap: 0.0,
            priority: 0,
        }
    }
}

#[cfg(test)]
mod physics_tests {
    use super::*;

    #[test]
    fn defaults_are_mujocos_own() {
        let m = MujocoPhysics::default();
        assert_eq!(m.condim, 3);
        assert_eq!(m.solref, [0.02, 1.0]);
        assert_eq!(m.solimp, [0.9, 0.95, 0.001, 0.5, 2.0]);
        // An author who never touches layers must get collisions, not silence.
        let p = PhysicsParams::default();
        assert_eq!(p.layer, u32::MAX);
        assert_eq!(p.mask, u32::MAX);
        assert_eq!(p.friction, 1.0);
    }

    /// A block that sets ONE field must not silently re-specify the rest.
    #[test]
    fn partial_blocks_fill_in_from_mujocos_defaults() {
        let m: MujocoPhysics = serde_json::from_str(r#"{"condim": 6}"#).unwrap();
        assert_eq!(m.condim, 6);
        assert_eq!(m.solref, default_solref());
        assert_eq!(m.rolling_friction, default_rolling());
    }

    #[test]
    fn round_trips_through_toml_and_json() {
        let p = PhysicsParams {
            friction: 0.6,
            restitution: 0.2,
            layer: 0b0101,
            mask: 0b0011,
            density: Some(900.0),
            mujoco: Some(MujocoPhysics {
                condim: 6,
                priority: 2,
                margin: 0.001,
                ..MujocoPhysics::default()
            }),
        };
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<PhysicsParams>(&json).unwrap(), p);
        let t = toml::to_string(&p).unwrap();
        assert_eq!(toml::from_str::<PhysicsParams>(&t).unwrap(), p);
    }

    /// Colliders authored before this feature have no `physics` key at all.
    #[test]
    fn absent_block_is_the_default() {
        let p: PhysicsParams = serde_json::from_str("{}").unwrap();
        assert_eq!(p, PhysicsParams::default());
        assert!(p.mujoco.is_none(), "no mujoco block unless authored");
    }
}
