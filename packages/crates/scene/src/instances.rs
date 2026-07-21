//! Instancing schemas: the curve-sampled placer (`InstancesAlongCurveDef`) and
//! the explicit instancer (`InstancerDef`) — one node that owns N authored
//! instance transforms.

use super::primitive::MeshRef;
use super::transform::Trs;
use super::tree::{MeshLodConfig, MeshShadowConfig, NodeId};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct InstancesAlongCurveDef {
    pub curve_node: NodeId,
    pub source_node: NodeId,
    pub spacing: f32,
    pub side_offset: f32,
    pub orient_to_tangent: bool,
    /// Per-instance color overrides applied in order; if shorter than the placed
    /// instance count, the last value is repeated.
    pub per_instance_colors: Vec<[f32; 4]>,
    /// Per-instance shadow cast / receive flags. Applies to every
    /// placed instance.
    #[serde(default)]
    pub shadow: MeshShadowConfig,
    /// Per-instance LOD opt-out. Applies to every placed instance.
    #[serde(default)]
    pub lod: MeshLodConfig,
}

impl Default for InstancesAlongCurveDef {
    fn default() -> Self {
        Self {
            curve_node: NodeId::nil(),
            source_node: NodeId::nil(),
            spacing: 1.0,
            side_offset: 0.0,
            orient_to_tangent: true,
            per_instance_colors: vec![],
            shadow: MeshShadowConfig::default(),
            lod: MeshLodConfig::default(),
        }
    }
}

/// Explicit GPU instancer: ONE node that references a mesh **asset** and OWNS
/// its N instance transforms, so thousands of instances never become thousands
/// of scene nodes. Unlike [`InstancesAlongCurveDef`] (which derives placement
/// from a curve and references a *source node*), the instancer is fully
/// self-contained: it references the mesh asset directly (the same
/// [`MeshRef`] a `NodeKind::Mesh` carries) and stores the authored transform
/// list verbatim. The renderer draws it as ONE geometry upload + one instance
/// buffer (`enable_mesh_instancing_opaque`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct InstancerDef {
    /// The instanced mesh asset (an `AssetSource::Mesh` entry, exactly like
    /// `NodeKind::Mesh`'s ref). [`crate::AssetId::nil`] = not wired up yet —
    /// the node renders empty until a mesh is picked.
    pub mesh: MeshRef,
    /// One local transform per instance (relative to this node's transform).
    pub transforms: Vec<Trs>,
    /// Per-instance color overrides applied in order; if shorter than
    /// `transforms`, the last value is repeated (same semantics as
    /// [`InstancesAlongCurveDef::per_instance_colors`]).
    #[serde(default)]
    pub per_instance_colors: Vec<[f32; 4]>,
    /// The ONE material every instance renders with (instancing shares one
    /// mesh + one pipeline, so this is mesh-level — there is no per-node
    /// variant palette here and `add_material_variant` rejects instancers).
    /// `None` = the flat default; per-instance colours apply either way. A
    /// custom-WGSL assignment reads them via `material_vertex_color(input, 0u)`.
    /// Set via `patch_kind {instancer: {material: ..}}`.
    #[serde(default)]
    pub material: Option<crate::dynamic_material::MaterialInstance>,
    /// Shadow cast / receive flags. Applies to every instance (instancing
    /// shares one mesh, so this is a mesh-level flag).
    #[serde(default)]
    pub shadow: MeshShadowConfig,
    /// LOD opt-out (default on). Applies to every instance.
    #[serde(default)]
    pub lod: MeshLodConfig,
}

impl Default for InstancerDef {
    fn default() -> Self {
        Self {
            mesh: MeshRef(crate::AssetId::nil()),
            transforms: Vec::new(),
            per_instance_colors: Vec::new(),
            material: None,
            shadow: MeshShadowConfig::default(),
            lod: MeshLodConfig::default(),
        }
    }
}

// `SweepAlongCurveDef` is an authoring *recipe* (a `MeshBase::Sweep` payload that
// bakes to a blob) — it lives in `awsm-renderer-meshgen` alongside the sweep evaluator,
// not in the runtime schema.
