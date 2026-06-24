//! Procedural **modifier stack** — the editable mesh *recipe* (Phase 3).
//!
//! A modifier stack is a non-destructive recipe: a [`MeshBase`] generator plus an
//! ordered list of [`Modifier`] deformers. It is tiny, infinitely re-editable
//! project data; the baked `.mesh.bin` triangle buffer is a regenerable cache.
//! Evaluation (recipe → `MeshData`) lives in `awsm-renderer-meshgen` (`modifiers::evaluate`),
//! which is pure-CPU and natively unit-tested.
//!
//! Stored on [`MeshDef::stack`](super::material::MeshDef::stack) — every
//! `MeshDef` carries one. A purely-captured / raw-edited / collapsed mesh is a
//! stack whose `base` is [`MeshBase::Captured`] with no modifiers.

use serde::{Deserialize, Serialize};

use awsm_renderer_scene::{CrossSectionDef, MeshRef, NodeId, PrimitiveShape, SweepUvMode, Trs};

/// The full editable recipe: a base generator + an ordered deformer list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ModifierStack {
    pub base: MeshBase,
    #[serde(default)]
    pub modifiers: Vec<Modifier>,
}

/// The geometry generator a stack starts from. The pure-data variants
/// (`Primitive` / `Lathe` / `Superquadric`) are evaluated entirely in
/// `awsm-renderer-meshgen`; `Sweep` (references a scene curve node) and `Captured`
/// (references stored bytes) are resolved editor-side and fed to the deformers as
/// a pre-baked base. (Phase 5 adds an `Sdf(SdfNode)` variant.)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum MeshBase {
    /// A built-in primitive shape (box / sphere / cylinder / …).
    Primitive(PrimitiveShape),
    /// Revolve a 2D `(height, radius)` profile around an axis — the LLM-native
    /// lathe (a baseball bat *is* a 1D radius profile). `angle` in radians
    /// (`TAU` = full revolution); `segments` = radial divisions.
    Lathe {
        profile: Vec<[f32; 2]>,
        segments: u32,
        angle: f32,
    },
    /// One exponent pair morphs box ↔ sphere ↔ cylinder ↔ octahedron.
    Superquadric {
        e1: f32,
        e2: f32,
        segments_long: u32,
        segments_lat: u32,
    },
    /// Sweep a cross-section along a scene curve (resolved editor-side).
    Sweep(SweepAlongCurveDef),
    /// Pre-captured geometry (resolved from the mesh store editor-side).
    Captured(MeshRef),
    /// An SDF/CSG graph meshed via surface nets (Phase 5). Pure data →
    /// trivially agent-composable ("a mug = cylinder minus a smaller cylinder,
    /// union a torus handle"). `resolution` is the sample-grid edge count.
    Sdf { node: SdfNode, resolution: u32 },
}

/// A signed-distance-field expression tree. Combinators carry an optional
/// `smooth` radius (0 = hard boolean; >0 = rounded/blended), which mesh booleans
/// cannot do — the deliberate reason SDF is the chosen CSG paradigm.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum SdfNode {
    /// A primitive SDF, positioned by its own transform via [`SdfNode::Transform`].
    Primitive(SdfPrimitive),
    /// `min` (rounded by `smooth`).
    Union { smooth: f32, children: Vec<SdfNode> },
    /// `a` minus the union of the rest.
    Subtract { smooth: f32, children: Vec<SdfNode> },
    /// `max` (rounded by `smooth`).
    Intersect { smooth: f32, children: Vec<SdfNode> },
    /// Translate/rotate/scale a child SDF.
    Transform { trs: Trs, child: Box<SdfNode> },
}

/// SDF primitive shapes (centered at the local origin).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum SdfPrimitive {
    Sphere {
        radius: f32,
    },
    /// Box of the given half-extents.
    Box {
        half: [f32; 3],
    },
    /// Capped cylinder along Y.
    Cylinder {
        radius: f32,
        height: f32,
    },
    Torus {
        major: f32,
        minor: f32,
    },
    /// Capsule along Y between ±height/2.
    Capsule {
        radius: f32,
        height: f32,
    },
}

/// A world/local axis selector for axis-parameterized deformers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum Axis {
    X,
    #[default]
    Y,
    Z,
}

/// A single non-destructive deformer. Each is a pure per-vertex (or
/// topology-changing) transform applied in stack order; the cheap organic /
/// symbolic ones land first (tier order — see the spec capability menu).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum Modifier {
    /// Scale cross-sections along `axis` linearly from `1.0` at the low end to
    /// `factor` at the high end.
    Taper { axis: Axis, factor: f32 },
    /// Rotate cross-sections progressively around `axis` (`turns` full turns end
    /// to end).
    Twist { axis: Axis, turns: f32 },
    /// Bend along `axis` by `angle` radians (total, end to end).
    Bend { axis: Axis, angle: f32 },
    /// Offset every vertex along its normal by `amount` ("puff it up").
    Inflate { amount: f32 },
    /// Morph each vertex toward a sphere of the mesh's bounding radius by
    /// `factor` (0 = unchanged, 1 = on the sphere).
    Spherify { factor: f32 },
    /// Random per-vertex jitter along the normal (natural/eroded look). `seed`
    /// makes it deterministic.
    Roughen { amount: f32, seed: u32 },
    /// Linear (midpoint) subdivision — `iterations` rounds, each splitting every
    /// triangle into four.
    Subdivide { iterations: u32 },
    /// Laplacian smoothing — `iterations` rounds, each moving a vertex `factor`
    /// of the way toward its neighbours' average.
    Smooth { iterations: u32, factor: f32 },
    /// Mirror across the plane through the origin with normal `axis` (keeps both
    /// halves — duplicates + reflects the geometry).
    Mirror { axis: Axis },
    /// Repeat the geometry `count` times, each copy offset by `offset` from the
    /// previous (a linear array).
    Array { count: u32, offset: [f32; 3] },
    /// Formula displacement along the normal: `expr` is evaluated per vertex over
    /// `(x, y, z, nx, ny, nz, u, v, i)` by the generic math evaluator. (For
    /// GPU-side displacement use the custom vertex-WGSL hook; for data-driven
    /// height use `displace_from_texture`.)
    Displace { expr: String },
}

/// Sweep a cross-section along a scene curve — a `MeshBase::Sweep` payload that
/// bakes to a blob. Lives here (the sweep evaluator's crate), not in the runtime
/// schema. `curve_node` references a `NodeId` in the live scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct SweepAlongCurveDef {
    pub curve_node: NodeId,
    pub cross_section: CrossSectionDef,
    pub uv_mode: SweepUvMode,
    pub up_hint: [f32; 3],
    pub samples: u32,
}

impl Default for SweepAlongCurveDef {
    fn default() -> Self {
        Self {
            curve_node: NodeId::nil(),
            cross_section: CrossSectionDef::default_tube(),
            uv_mode: SweepUvMode::default(),
            up_hint: [0.0, 1.0, 0.0],
            samples: 64,
        }
    }
}
