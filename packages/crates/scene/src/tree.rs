use uuid::Uuid;

use super::{
    assets::AssetId, camera::CameraConfig, collider::ColliderShape, curve::CurveDef,
    decal::DecalConfig, dynamic_material::MaterialInstance, instances::InstancesAlongCurveDef,
    light::LightConfig, line::LineDef, particle::ParticleEmitterDef, primitive::MeshRef,
    sprite::SpriteDef, transform::Trs,
};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EditorNode {
    pub id: NodeId,
    pub name: String,
    #[serde(default)]
    pub transform: Trs,
    pub kind: NodeKind,
    #[serde(default)]
    pub locked: bool,
    /// Per-node visibility toggle from the editor's eye icon. When
    /// `false`, the renderer hides the node's meshes (Model), zeros its
    /// light intensity (Light), or skips its wireframe (Collision); the
    /// node itself stays in the tree and remains editable. Hiding a
    /// `Group` propagates to its descendants. Persisted across save/load.
    #[serde(default = "default_visible")]
    pub visible: bool,
    /// Marks this node as a prefab root. The game-player runtime skips
    /// prefab subtrees during scene-build and exposes them in a prefab
    /// table so per-game code can instantiate copies on demand. The flag
    /// is **root-only**: descendants are not implicitly prefabs and may
    /// themselves be marked, creating nested prefabs (each independently
    /// instantiable). The editor renders prefab subtrees verbatim so
    /// authors can see and edit them.
    #[serde(default)]
    pub prefab: bool,
    #[serde(default)]
    pub children: Vec<EditorNode>,
}

/// `serde(default)` for `bool` is `false`; visibility defaults to `true`,
/// so legacy `project.json` files (no `visible` field) load with every
/// node visible.
fn default_visible() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
#[derive(Eq, Hash, Copy)]
pub struct NodeId(pub Uuid);

// A `NodeId` is a UUID string on the wire — describe it as such for JSON Schema
// (the MCP server's typed tool params) rather than recursing into Uuid.
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for NodeId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "NodeId".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string", "format": "uuid" })
    }
}

impl NodeId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// The stable all-zeros sentinel, meaning "no node referenced / unset". Unlike
    /// [`NodeId::default`] (which mints a fresh random id), this is a fixed value
    /// usable as a real "none" marker for optional node references (curve/source
    /// picks, etc.). Pair with [`NodeId::is_nil`].
    pub const fn nil() -> Self {
        Self(Uuid::nil())
    }

    /// True when this is the [`NodeId::nil`] sentinel (an unset reference).
    pub fn is_nil(&self) -> bool {
        self.0.is_nil()
    }

    /// Borrow the NodeId as a 16-byte slice. Used by the player → per-
    /// game-player FFI bridge: `&[u8]` is one of the few zero-config
    /// `wasm-bindgen` parameter types and we want to keep this hot path
    /// allocation-free on the caller side.
    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }

    /// Counterpart of [`Self::as_bytes`] — recover a `NodeId` from a
    /// 16-byte slice received across an FFI / wasm-bindgen boundary.
    /// Errors if the slice isn't exactly 16 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, NodeIdFromBytesError> {
        Uuid::from_slice(bytes)
            .map(Self)
            .map_err(|_| NodeIdFromBytesError {
                got_len: bytes.len(),
            })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("NodeId::from_bytes: expected 16 bytes, got {got_len}")]
pub struct NodeIdFromBytesError {
    pub got_len: usize,
}

impl Default for NodeId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

/// Reference to a **skinned** glTF mesh: the imported source file + which node
/// (and optionally which primitive) inside it. The renderer's `populate_gltf`
/// builds the skinned mesh + skeleton at import; the bridge looks this node up
/// in the per-import template (keyed by `source`) to find the populate-baked
/// renderer mesh that deforms via the skeleton joints. There is no `MeshRef`/
/// captured-geometry side: skinned geometry lives only in the renderer skin
/// path until `drop_skinning` bakes its bind pose into a captured `Mesh`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SkinnedMeshRef {
    /// The imported glTF/glb source file's `AssetId` (an `AssetSource::Filename`
    /// entry) — the key under which the bridge caches this import's node template.
    pub source: AssetId,
    /// Which node inside the referenced ORIGINAL glTF file carries this skinned
    /// mesh. Stable identity used by `drop_skinning` / export / the bind-pose
    /// `skinned_bake_cache` (all original-indexed). NOT used to address the rig
    /// glb — see [`Self::rig_node_index`].
    pub node_index: u32,
    /// That same node's index in the re-exported **clean rig glb**
    /// (`assets/<source>.glb`) — the DFS-flatten index `reexport_clean` assigns
    /// (which differs from `node_index` when the source isn't already DFS-ordered).
    /// This is what the MATERIALISER decodes the rig glb at to rebuild the skinned
    /// drawable (geometry + skin) from our-format, uniformly for first-show /
    /// reload / re-materialise. Captured at import from `node_flat_indices`;
    /// `#[serde(default)]` (0) for legacy projects saved before this field — those
    /// re-import to repopulate it. Shares the rig glb's single flat index space
    /// with [`SkinJoint::index`].
    #[serde(default)]
    pub rig_node_index: u32,
    /// Optional primitive index within that node (for a multi-material skinned
    /// node destructured into per-primitive children). `None` = the whole node.
    /// Same value for the original AND the rig glb (re-export preserves primitive
    /// order).
    #[serde(default)]
    pub primitive_index: Option<u32>,
    /// Bone correspondence for driving the rig from our clips: each skeleton
    /// joint's scene bone `NodeId` paired with its node index in the re-exported
    /// clean rig glb (`assets/<source>.glb`) — the index space the player's
    /// `populate_gltf` assigns when it loads that glb. Our clips' `Transform`
    /// tracks target bone `NodeId`s; the player maps those NodeIds → the rig's
    /// baked joint transforms through this table so animating a bone deforms the
    /// skin. Captured at skinned-glTF import; **empty** for legacy projects (the
    /// rig then poses at bind pose). Every skinned node of one import shares the
    /// same table (one rig glb, one flat index space).
    #[serde(default)]
    pub joints: Vec<SkinJoint>,
}

/// One skin-joint correspondence: a skeleton bone's scene [`NodeId`] paired with
/// its node index in the re-exported clean rig glb. See
/// [`SkinnedMeshRef::joints`].
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SkinJoint {
    /// The bone's scene node id (an animation `Transform` track targets this).
    pub node: NodeId,
    /// That bone's node index in the re-exported clean rig glb.
    pub index: u32,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
// `Mesh` (the common, dominant variant) inlines a `MaterialInstance`, which makes
// it much larger than the leaf variants. Boxing it would penalise the hot path to
// shrink the rare ones, so accept the size spread (same call as `AssetSource`).
#[allow(clippy::large_enum_variant)]
pub enum NodeKind {
    Group,
    Light(LightConfig),
    Collider(ColliderShape),
    Camera(CameraConfig),
    /// The sole procedural-geometry node. Backed by an `AssetSource::Mesh`
    /// ([`super::material::MeshDef`]) referenced by `MeshRef`; the `MeshDef`
    /// always carries a `ModifierStack { base, modifiers }`, so a box/sphere/…,
    /// a sweep, a lathe, an SDF, or a raw-edited capture are all the same node
    /// kind — only the stack `base` differs. Multiple nodes can share one mesh
    /// asset.
    Mesh {
        mesh: MeshRef,
        /// The node's single material assignment. `None` means *unassigned*
        /// and renders flat magenta (the missing-material sentinel).
        #[serde(default)]
        material: Option<MaterialInstance>,
        #[serde(default)]
        shadow: MeshShadowConfig,
    },
    /// A **skinned** mesh imported from a glTF — a deliberate *second* geometry
    /// category, distinct from `Mesh`. It is **not** a `MeshDef`/`ModifierStack`
    /// (no base/edits/overrides) and so **not editable**: per-vertex skin
    /// weights can't survive topology-changing edits. It is rendered + deformed
    /// by the renderer's existing glTF skin path (joints driven by the editor's
    /// mirror bones + imported animation clips), NOT the captured-mesh pipeline.
    /// `drop_skinning` is the explicit, terminal bridge to editing: it bakes the
    /// bind-pose geometry into a captured `Mesh{ stack:{base:Captured} }` and
    /// swaps the node to `NodeKind::Mesh`.
    SkinnedMesh {
        skin: SkinnedMeshRef,
        /// Single material assignment (same one-material-per-node model as
        /// `Mesh`); `None` renders flat magenta.
        #[serde(default)]
        material: Option<MaterialInstance>,
        #[serde(default)]
        shadow: MeshShadowConfig,
    },
    /// Catmull-Rom curve (control points + closed + tension). Emits no renderer
    /// node directly; consumed by sweep / instance / camera nodes.
    Curve(CurveDef),
    /// Place copies of a source node along a curve.
    InstancesAlongCurve(InstancesAlongCurveDef),
    /// Authored polyline (debug-draw / neon rails / curve handles).
    Line(LineDef),
    /// Camera-facing or world-aligned textured quad.
    Sprite(SpriteDef),
    /// CPU particle emitter.
    ParticleEmitter(ParticleEmitterDef),
    /// Projection decal. The node's
    /// transform supplies the oriented unit-cube volume; the
    /// renderer projects the configured texture down the local -Z
    /// axis onto whatever opaque geometry sits inside.
    Decal(DecalConfig),
}

/// Per-mesh shadow flags. Sprite, line, and particle nodes do NOT
/// carry this — they are hard-coded to no-cast / no-receive in v1.
///
/// Defaults to both `cast` and `receive` true. Transparent materials
/// should override this to `TRANSPARENT_DEFAULT` (both off) — the
/// renderer bridge or scene loader is responsible for that
/// reinterpretation since the schema doesn't know the resolved
/// material's alpha mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MeshShadowConfig {
    /// Whether the mesh appears in the shadow-generation pass.
    #[serde(default = "default_true_msc")]
    pub cast: bool,
    /// Whether the mesh's shaded pixels darken under shadow.
    #[serde(default = "default_true_msc")]
    pub receive: bool,
}

impl Default for MeshShadowConfig {
    fn default() -> Self {
        Self {
            cast: true,
            receive: true,
        }
    }
}

impl MeshShadowConfig {
    /// Conservative default for transparent materials.
    pub const TRANSPARENT_DEFAULT: Self = Self {
        cast: false,
        receive: false,
    };
}

fn default_true_msc() -> bool {
    true
}

impl NodeKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Light(_) => "light",
            Self::Collider(_) => "collider",
            Self::Camera(_) => "camera",
            Self::Mesh { .. } => "mesh",
            Self::SkinnedMesh { .. } => "skinned_mesh",
            Self::Curve(_) => "curve",
            Self::InstancesAlongCurve(_) => "instances",
            Self::Line(_) => "line",
            Self::Sprite(_) => "sprite",
            Self::ParticleEmitter(_) => "particle",
            Self::Decal(_) => "decal",
        }
    }

    /// Returns this node's mesh shadow config if the variant carries
    /// one; returns `None` for non-renderable nodes (groups, lights,
    /// cameras, curves, lines, sprites, particles).
    pub fn mesh_shadow(&self) -> Option<&MeshShadowConfig> {
        match self {
            Self::Mesh { shadow, .. } => Some(shadow),
            Self::SkinnedMesh { shadow, .. } => Some(shadow),
            Self::InstancesAlongCurve(d) => Some(&d.shadow),
            _ => None,
        }
    }

    /// Mutable variant of [`Self::mesh_shadow`].
    pub fn mesh_shadow_mut(&mut self) -> Option<&mut MeshShadowConfig> {
        match self {
            Self::Mesh { shadow, .. } => Some(shadow),
            Self::SkinnedMesh { shadow, .. } => Some(shadow),
            Self::InstancesAlongCurve(d) => Some(&mut d.shadow),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `SkinnedMesh` node with a joint table round-trips through TOML — the
    /// bundle's `scene.toml` format. `Vec<SkinJoint>` must serialize as an array
    /// of tables (NOT a `Vec<(NodeId, u32)>` tuple, which would be a mixed-type
    /// TOML array and fail to parse).
    #[test]
    fn skinned_mesh_joints_toml_round_trip() {
        let node = EditorNode {
            id: NodeId::new(),
            name: "Cylinder".into(),
            transform: Trs::default(),
            kind: NodeKind::SkinnedMesh {
                skin: SkinnedMeshRef {
                    source: AssetId::new(),
                    node_index: 2,
                    rig_node_index: 2,
                    primitive_index: None,
                    joints: vec![
                        SkinJoint {
                            node: NodeId::new(),
                            index: 3,
                        },
                        SkinJoint {
                            node: NodeId::new(),
                            index: 4,
                        },
                    ],
                },
                material: None,
                shadow: MeshShadowConfig::default(),
            },
            locked: false,
            visible: true,
            prefab: false,
            children: Vec::new(),
        };

        let text = toml::to_string(&node).expect("serialize");
        let back: EditorNode = toml::from_str(&text).expect("deserialize");
        assert_eq!(node, back);
        match back.kind {
            NodeKind::SkinnedMesh { skin, .. } => {
                assert_eq!(skin.joints.len(), 2);
                assert_eq!(skin.joints[0].index, 3);
                assert_eq!(skin.joints[1].index, 4);
            }
            other => panic!("expected SkinnedMesh, got {other:?}"),
        }
    }

    /// A legacy `SkinnedMeshRef` with no `joints` key deserializes to an empty
    /// table (the `#[serde(default)]` path → bind-pose, no animation binding).
    #[test]
    fn skinned_mesh_ref_joints_default_empty() {
        let toml = r#"
            source = "00000000-0000-0000-0000-000000000000"
            node_index = 1
        "#;
        let skin: SkinnedMeshRef = toml::from_str(toml).expect("deserialize legacy");
        assert!(skin.joints.is_empty());
        assert_eq!(skin.node_index, 1);
    }
}
