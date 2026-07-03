use uuid::Uuid;

use super::{
    assets::AssetId,
    camera::CameraConfig,
    collider::ColliderShape,
    curve::CurveDef,
    decal::DecalConfig,
    dynamic_material::{MaterialInstance, MaterialVariant, VariantId},
    instances::InstancesAlongCurveDef,
    light::LightConfig,
    line::LineDef,
    particle::ParticleEmitterDef,
    primitive::MeshRef,
    sprite::SpriteDef,
    transform::Trs,
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
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct SkinJoint {
    /// The bone's scene node id (an animation `Transform` track targets this).
    pub node: NodeId,
    /// That bone's node index in the re-exported clean rig glb.
    pub index: u32,
}

/// Reference to a **pre-baked cluster-LOD ("nanite") mesh**: the imported source
/// asset whose baked cluster DAG (`assets/<source>.clusters.bin`) + base geometry
/// (`assets/<source>.glb`) the renderer streams through the bounded cluster
/// pipeline. Like [`SkinnedMeshRef`], this is a deliberately **view-only**,
/// renderer-managed geometry category — it is NOT a `MeshDef`/`ModifierStack`, so
/// it carries no editable stack/overrides. It exists so a large mesh can be brought
/// into the editor and rendered as nanite (bounded draw + VRAM) via the SAME path
/// the player uses, without the dense visibility-geometry explode that would
/// otherwise crash on a multi-million-triangle mesh.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ClusterMeshRef {
    /// The imported source asset's `AssetId`. Its baked cluster DAG side file
    /// (`assets/<source>.clusters.bin`) is loaded + materialized by the cluster
    /// pipeline (`scene-loader::materialize_cluster_mesh`).
    pub source: AssetId,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
// `Mesh` (the common, dominant variant) inlines a `MaterialInstance`, which makes
// it much larger than the leaf variants. Boxing it would penalise the hot path to
// shrink the rare ones, so accept the size spread (same call as `AssetSource`).
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[allow(clippy::large_enum_variant)]
pub enum NodeKind {
    Group,
    Light(LightConfig),
    /// A physics collider. Its SIZE comes entirely from the `ColliderShape`
    /// extents (`half_extents` / `radius` / `half_height`) — NOT from the node's
    /// transform scale. A Rapier collider has no scale: its placement is an
    /// isometry, so only the node's translation + rotation are honored at export
    /// (`ColliderSpec::from_node`). Node scale on a collider is locked to
    /// `[1,1,1]` by the editor and ignored by the runtime — to make a collider
    /// bigger, edit its shape extents, never the transform scale. Rotation IS
    /// honored and is the only way to orient a Y-aligned Capsule/Cylinder/Cone
    /// along X or Z, or to tilt a Box.
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
        /// The mesh's material palette. Every material this mesh can render
        /// is an entry here — a library-material reference plus THIS mesh's
        /// independent overrides (see [`MaterialVariant`]). The editor's
        /// Material dropdown lists exactly this; the player loader pre-builds
        /// every entry into a ready `MaterialKey`.
        #[serde(default)]
        material_variants: Vec<MaterialVariant>,
        /// Which variant renders. `None` = unassigned (flat magenta, the
        /// missing-material sentinel) — legal even with a populated list.
        #[serde(default)]
        selected_variant: Option<VariantId>,
        #[serde(default)]
        shadow: MeshShadowConfig,
        /// Per-mesh LOD opt-out (default on). Authored in the editable project,
        /// consumed by the export-time LOD bake. See [`MeshLodConfig`].
        #[serde(default)]
        lod: MeshLodConfig,
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
        /// The mesh's material palette (same model as [`Self::Mesh`]).
        #[serde(default)]
        material_variants: Vec<MaterialVariant>,
        /// Which variant renders; `None` = magenta.
        #[serde(default)]
        selected_variant: Option<VariantId>,
        #[serde(default)]
        shadow: MeshShadowConfig,
        /// Per-mesh LOD opt-out (default on). See [`MeshLodConfig`].
        #[serde(default)]
        lod: MeshLodConfig,
    },
    /// A **pre-baked cluster-LOD ("nanite") mesh** — a third, deliberately
    /// **view-only** geometry category (like [`Self::SkinnedMesh`], not editable):
    /// no `MeshDef`/stack, rendered + cut by the renderer's cluster pipeline from a
    /// baked `assets/<source>.clusters.bin`. Brought in via the offline `awsm-renderer-lod-bake`
    /// pre-bake so a huge mesh views as nanite in-editor (bounded) without re-baking
    /// or a dense explode. No `lod` toggle — it IS the LOD.
    ClusterMesh {
        cluster: ClusterMeshRef,
        /// The mesh's material palette (same model as [`Self::Mesh`]).
        #[serde(default)]
        material_variants: Vec<MaterialVariant>,
        /// Which variant renders; `None` = magenta.
        #[serde(default)]
        selected_variant: Option<VariantId>,
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
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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

/// Per-mesh LOD opt-**out** flag. LOD is the norm for a general game renderer,
/// so this defaults **on**; authors flip it off for hero assets where any
/// simplification is unacceptable, already-low-poly meshes (bake cost, no
/// benefit), or HUD/UI meshes.
///
/// Authored in the editable project (persists in `project.toml` like
/// [`MeshShadowConfig`]) and consumed by the **export-time** LOD bake — it has
/// no meaning at import. One `enabled: bool` to start; grows later to carry
/// params (target ratios, level count, error threshold).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct MeshLodConfig {
    /// Whether the export bake generates simplified LOD levels for this mesh.
    #[serde(default = "default_true_mlc")]
    pub enabled: bool,
}

impl Default for MeshLodConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

fn default_true_mlc() -> bool {
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
            Self::ClusterMesh { .. } => "cluster_mesh",
            Self::Curve(_) => "curve",
            Self::InstancesAlongCurve(_) => "instances",
            Self::Line(_) => "line",
            Self::Sprite(_) => "sprite",
            Self::ParticleEmitter(_) => "particle",
            Self::Decal(_) => "decal",
        }
    }

    /// The node's material palette, if this kind carries one (Mesh /
    /// SkinnedMesh / ClusterMesh).
    pub fn material_variants(&self) -> Option<&Vec<MaterialVariant>> {
        match self {
            Self::Mesh {
                material_variants, ..
            }
            | Self::SkinnedMesh {
                material_variants, ..
            }
            | Self::ClusterMesh {
                material_variants, ..
            } => Some(material_variants),
            _ => None,
        }
    }

    /// Mutable variant of [`Self::material_variants`].
    pub fn material_variants_mut(&mut self) -> Option<&mut Vec<MaterialVariant>> {
        match self {
            Self::Mesh {
                material_variants, ..
            }
            | Self::SkinnedMesh {
                material_variants, ..
            }
            | Self::ClusterMesh {
                material_variants, ..
            } => Some(material_variants),
            _ => None,
        }
    }

    /// The id of the variant this mesh renders (`None` = unassigned/magenta,
    /// or a non-mesh kind).
    pub fn selected_variant_id(&self) -> Option<VariantId> {
        match self {
            Self::Mesh {
                selected_variant, ..
            }
            | Self::SkinnedMesh {
                selected_variant, ..
            }
            | Self::ClusterMesh {
                selected_variant, ..
            } => *selected_variant,
            _ => None,
        }
    }

    /// Point the mesh at a variant (or `None` = unassigned). Returns `false`
    /// for kinds without a material palette.
    pub fn set_selected_variant(&mut self, id: Option<VariantId>) -> bool {
        match self {
            Self::Mesh {
                selected_variant, ..
            }
            | Self::SkinnedMesh {
                selected_variant, ..
            }
            | Self::ClusterMesh {
                selected_variant, ..
            } => {
                *selected_variant = id;
                true
            }
            _ => false,
        }
    }

    /// The SELECTED variant's material instance — what this mesh renders.
    /// `None` = unassigned (magenta), a dangling selection, or a non-mesh kind.
    pub fn selected_material(&self) -> Option<&MaterialInstance> {
        let id = self.selected_variant_id()?;
        self.material_variants()?
            .iter()
            .find(|v| v.id == id)
            .map(|v| &v.instance)
    }

    /// Mutable variant of [`Self::selected_material`] — the write target for
    /// every per-mesh material edit (inline params, texture binds, overrides).
    pub fn selected_material_mut(&mut self) -> Option<&mut MaterialInstance> {
        let id = self.selected_variant_id()?;
        self.material_variants_mut()?
            .iter_mut()
            .find(|v| v.id == id)
            .map(|v| &mut v.instance)
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

    /// Returns this node's mesh LOD config if the variant carries one; `None`
    /// for non-mesh kinds. Mirrors [`Self::mesh_shadow`].
    pub fn mesh_lod(&self) -> Option<&MeshLodConfig> {
        match self {
            Self::Mesh { lod, .. } => Some(lod),
            Self::SkinnedMesh { lod, .. } => Some(lod),
            Self::InstancesAlongCurve(d) => Some(&d.lod),
            _ => None,
        }
    }

    /// Mutable variant of [`Self::mesh_lod`].
    pub fn mesh_lod_mut(&mut self) -> Option<&mut MeshLodConfig> {
        match self {
            Self::Mesh { lod, .. } => Some(lod),
            Self::SkinnedMesh { lod, .. } => Some(lod),
            Self::InstancesAlongCurve(d) => Some(&mut d.lod),
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
                material_variants: Vec::new(),
                selected_variant: None,
                shadow: MeshShadowConfig::default(),
                lod: MeshLodConfig::default(),
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

    /// A `MeshLodConfig` round-trips through TOML, and a legacy `Mesh` node with
    /// no `lod` table defaults to `enabled = true` (LOD is opt-out, default on).
    /// This is the backwards-compat guarantee for projects saved before the LOD
    /// toggle existed.
    #[test]
    fn mesh_lod_config_default_and_round_trip() {
        // Explicit opt-out survives a round-trip.
        let off = MeshLodConfig { enabled: false };
        let text = toml::to_string(&off).expect("serialize");
        let back: MeshLodConfig = toml::from_str(&text).expect("deserialize");
        assert_eq!(off, back);

        // A legacy mesh kind TOML with no `lod` table → enabled defaults true.
        let legacy = r#"
            [mesh]
            mesh = "00000000-0000-0000-0000-000000000000"
        "#;
        let kind: NodeKind = toml::from_str(legacy).expect("deserialize legacy mesh kind");
        assert_eq!(
            kind.mesh_lod().copied(),
            Some(MeshLodConfig { enabled: true }),
            "absent `lod` must default to enabled (opt-out, default on)"
        );
    }
}

#[cfg(all(test, feature = "schemars"))]
mod schema_tests {
    use crate::NodeKind;

    // §3: the generated NodeKind schema must expose every variant's real field
    // shape (transitively, via $defs) — that's what makes it machine-readable for
    // authoring a fresh kind without an existing instance to copy.
    #[test]
    fn node_kind_schema_lists_variant_fields() {
        let json = serde_json::to_string(&schemars::schema_for!(NodeKind)).unwrap();
        for needle in [
            "ParticleEmitterDef",
            "spawn_rate", // a ParticleEmitterDef field
            "CameraConfig",
            "projection",       // a CameraConfig field
            "LightConfig",      // a NodeKind::Light sub-type
            "MaterialInstance", // inlined by NodeKind::Mesh
            "AnisotropyExt",    // a macro-generated KHR PBR extension, deep in the tree
        ] {
            assert!(json.contains(needle), "NodeKind schema missing `{needle}`");
        }
    }
}
