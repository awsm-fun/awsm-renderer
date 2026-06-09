//! The **authoring** mesh: `MeshDef` (a modifier-stack recipe + sparse per-vertex
//! overrides) and its provenance. The editor's bake step lowers this to the
//! runtime `awsm_scene::RuntimeMesh` / `MeshBlob`. Moved out of the old
//! scene-schema so the runtime crate stays free of authoring types.

use awsm_meshgen::recipe::{ModifierStack, SweepAlongCurveDef};
use awsm_scene::{AssetId, PrimitiveShape};

/// `source` records the kind the mesh was captured from. The editor's
/// Mesh inspector renders editable copies of those params; mutating
/// them auto-regenerates the bytes against the same AssetId, so every
/// referencing `NodeKind::Mesh` picks up the change without the user
/// having to find a source node in the tree.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MeshDef {
    pub label: String,
    #[serde(default)]
    pub source: Option<CapturedSource>,
    /// When true the mesh is *editable*: its geometry is the regenerable
    /// `.mesh.bin` cache (raw-vertex-edited or collapsed) re-evaluated from
    /// `stack`. `#[serde(default)]` keeps pre-feature project.json/bin files
    /// round-tripping (older captured meshes deserialize as `editable = false`).
    #[serde(default)]
    pub editable: bool,
    /// The procedural recipe (modifier stack) this mesh regenerates from. Every
    /// `MeshDef` carries one: a purely-captured / imported blob is a stack whose
    /// `base` is [`MeshBase::Captured`] (or the source's own recipe) with no
    /// modifiers; a primitive/sweep node mints a stack with the matching
    /// [`MeshBase::Primitive`] / [`MeshBase::Sweep`] base. The `.mesh.bin`
    /// triangle buffer is the regenerable bake of evaluating this stack. See
    /// [`ModifierStack`].
    pub stack: ModifierStack,
    /// Sparse, index-keyed **per-vertex authoring overrides** layered on top of
    /// the evaluated `stack` (see [`VertexOverrides`]). Per-vertex authoring is
    /// *terminal*: the first authoring op collapses `stack` to a frozen
    /// `Captured` base (locking topology), after which these maps are the only
    /// non-destructive edit layer. Empty by default — `#[serde(default)]` keeps
    /// pre-feature project.json round-tripping.
    #[serde(default)]
    pub overrides: VertexOverrides,
}

/// Sparse, vertex-index-keyed authoring overrides applied **after** the modifier
/// stack evaluates (see [`MeshDef::overrides`]). Each map keys a vertex index
/// (into the frozen, post-eval topology) to its authored value; an index absent
/// from a map rides along with the evaluated base. Positions are *edited*
/// (sculpt); colors/normals/uvs are *authored* channels (a channel is created on
/// the baked mesh if any override for it exists). This is the data behind the
/// `PaintVertexColors` / `SetVertexNormals` / migrated `SetVertexPositions` /
/// `SoftTransformVertices` commands.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct VertexOverrides {
    #[serde(default)]
    pub positions: std::collections::HashMap<u32, [f32; 3]>,
    #[serde(default)]
    pub colors: std::collections::HashMap<u32, [f32; 4]>,
    #[serde(default)]
    pub normals: std::collections::HashMap<u32, [f32; 3]>,
    #[serde(default)]
    pub uvs: std::collections::HashMap<u32, [f32; 2]>,
}

impl VertexOverrides {
    /// True when no override of any channel is present.
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
            && self.colors.is_empty()
            && self.normals.is_empty()
            && self.uvs.is_empty()
    }
}

/// Where a captured mesh's geometry came from. Stored on `MeshDef`
/// so the Mesh inspector can render the source params + re-capture
/// without a separate source node.
///
/// `Sweep`'s `curve_node` is a `NodeId` reference into the live
/// scene; if that node is deleted between captures the inspector
/// falls back to the legacy "pick a source from scene" picker.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturedSource {
    Primitive(PrimitiveShape),
    Sweep(SweepAlongCurveDef),
    /// Raw-vertex-edited / collapsed geometry — there is **no** recipe to
    /// regenerate from; the `.mesh.bin` triangle buffer *is* the source of truth.
    Editable,
    /// Geometry baked from an imported model. The original `.glb` on disk
    /// (referenced by `source`) remains the editable source of truth; the
    /// `.mesh.bin` is a bake for editing/export.
    Imported {
        source: AssetId,
    },
}

/// Captured procedural-mesh geometry, bitcode-serialized into the
/// project's `assets/<asset-id>.mesh.bin` side file. Mirrors the
/// in-memory shape of `awsm_meshgen::MeshData` so the materializer can
/// hand the data straight to the renderer without massaging.
///
/// The consuming crates own conversion helpers (editor bake → runtime MeshBlob).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CapturedMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Option<Vec<[f32; 3]>>,
    pub uvs: Option<Vec<[f32; 2]>>,
    pub colors: Option<Vec<[f32; 4]>>,
    pub indices: Vec<u32>,
}
