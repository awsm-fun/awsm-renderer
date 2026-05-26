use uuid::Uuid;

use super::{
    camera::CameraConfig,
    collider::ColliderShape,
    curve::CurveDef,
    decal::DecalConfig,
    dynamic_material::CustomMaterialInstance,
    instances::{InstancesAlongCurveDef, SweepAlongCurveDef},
    light::LightConfig,
    line::LineDef,
    material::MaterialDef,
    model::ModelRef,
    particle::ParticleEmitterDef,
    primitive::{MaterialRef, MeshRef, PrimitiveShape},
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
    /// authors can see and edit them. See `docs/game-editor-player.md`.
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

impl NodeId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
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

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Group,
    Model(ModelRef),
    Light(LightConfig),
    Collider(ColliderShape),
    Camera(CameraConfig),
    /// Procedural primitive with material reference. The renderer materializes
    /// the shape at load time via `awsm-meshgen`.
    Primitive {
        shape: PrimitiveShape,
        material: Option<MaterialRef>,
        /// Embedded fallback material, used when `material` is `None`. Useful for
        /// quick authoring without going through the asset table.
        #[serde(default)]
        inline_material: MaterialDef,
        /// Optional runtime-registered custom material. When `Some`,
        /// wins over both `material` and `inline_material` — the
        /// renderer-bridge resolves the name against the
        /// `MaterialRegistry` and constructs a `Material::Custom`.
        /// Old `project.json` files without this field round-trip via
        /// `#[serde(default)]`.
        #[serde(default)]
        custom_material: Option<CustomMaterialInstance>,
        #[serde(default)]
        shadow: MeshShadowConfig,
    },
    /// A captured procedural mesh stored as an asset; multiple nodes can share it.
    Mesh {
        mesh: MeshRef,
        material: Option<MaterialRef>,
        #[serde(default)]
        inline_material: MaterialDef,
        /// Optional runtime-registered custom material. See
        /// [`NodeKind::Primitive::custom_material`].
        #[serde(default)]
        custom_material: Option<CustomMaterialInstance>,
        #[serde(default)]
        shadow: MeshShadowConfig,
    },
    /// Catmull-Rom curve (control points + closed + tension). Emits no renderer
    /// node directly; consumed by sweep / instance / camera nodes.
    Curve(CurveDef),
    /// Sweep a cross-section along a curve to produce surface geometry.
    SweepAlongCurve {
        def: SweepAlongCurveDef,
        material: Option<MaterialRef>,
        #[serde(default)]
        inline_material: MaterialDef,
        /// Optional runtime-registered custom material. See
        /// [`NodeKind::Primitive::custom_material`].
        #[serde(default)]
        custom_material: Option<CustomMaterialInstance>,
        #[serde(default)]
        shadow: MeshShadowConfig,
    },
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
            Self::Model(_) => "model",
            Self::Light(_) => "light",
            Self::Collider(_) => "collider",
            Self::Camera(_) => "camera",
            Self::Primitive { .. } => "primitive",
            Self::Mesh { .. } => "mesh",
            Self::Curve(_) => "curve",
            Self::SweepAlongCurve { .. } => "sweep",
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
            Self::Model(r) => Some(&r.shadow),
            Self::Primitive { shadow, .. }
            | Self::Mesh { shadow, .. }
            | Self::SweepAlongCurve { shadow, .. } => Some(shadow),
            Self::InstancesAlongCurve(d) => Some(&d.shadow),
            _ => None,
        }
    }

    /// Mutable variant of [`Self::mesh_shadow`].
    pub fn mesh_shadow_mut(&mut self) -> Option<&mut MeshShadowConfig> {
        match self {
            Self::Model(r) => Some(&mut r.shadow),
            Self::Primitive { shadow, .. }
            | Self::Mesh { shadow, .. }
            | Self::SweepAlongCurve { shadow, .. } => Some(shadow),
            Self::InstancesAlongCurve(d) => Some(&mut d.shadow),
            _ => None,
        }
    }
}
