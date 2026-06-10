//! Scene-complete glTF/GLB **export** IR + writer.
//!
//! This crate is the one-way bake target: an editor project (any mix of
//! procedural recipes, raw-edited meshes, and imported models) is flattened to
//! triangles + materials and handed here as a [`GlbScene`], which [`write_glb`]
//! serializes to a self-contained `.glb` byte vector.
//!
//! It has **no GPU / editor / wasm dependencies** — only `awsm-meshgen` (for the
//! plain-data [`MeshData`]) and `gltf-json` (the JSON model used to *build* a
//! glTF). That keeps the whole writer natively unit-testable (`cargo test -p
//! awsm-glb-export`).
//!
//! ## Scene-complete by design
//!
//! The IR carries node hierarchy + transforms, meshes, materials, **lights**,
//! **cameras**, **animations**, and an **environment** slot up front — even
//! though the standalone Phase-1 export path only populates mesh + material. The
//! player-bundle publish path (Phase 6) reuses the exact same IR + writer for the
//! whole-runtime bake, so the shape must not be mesh-only.
//!
//! ## Material policy (lossless, portable)
//!
//! - Built-in **PBR** → real glTF PBR ([`ExportMaterial::Pbr`]).
//! - **Unlit** → `KHR_materials_unlit` ([`ExportMaterial::Unlit`]).
//! - **Non-PBR** (custom WGSL / Toon / anything not glTF-representable) →
//!   [`ExportMaterial::None`]: the primitive is emitted with an
//!   [`AWSM_MATERIALS_NONE`] extension and **no embedded material**, so a
//!   re-import leaves the material slot empty for scene-level resolution.
//! - **Textures are referenced-only**: the writer embeds exactly the images
//!   present in [`GlbScene::images`]; the editor includes only the images the
//!   *assigned* materials use, so reassigning a lighter material drops the heavy
//!   textures with no special "slim" flag.

mod bundle;
mod extract;
mod write;

pub use awsm_meshgen::MeshData;
pub use bundle::{assemble_bundle, BundleFile, BundleInputs, PlayerBundle};
pub use extract::{
    extract_node_mesh, extract_node_mesh_from_bytes, reexport_clean, reexport_clean_scene,
    scene_node_flat_indices,
};
pub use write::write_glb;

/// The primitive-level glTF extension marking a primitive whose real material is
/// **not** glTF-representable and must be resolved by the scene/player on import
/// (rather than defaulting to a glTF material). Defined here once; the importer
/// (`renderer-gltf`) recognizes the same token to leave the material slot empty.
pub const AWSM_MATERIALS_NONE: &str = "AWSM_materials_none";

/// A translate / rotate (xyzw quaternion) / scale local transform. Mirrors
/// `awsm_scene::Trs` but kept local so this crate stays decoupled from the
/// project schema.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trs {
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}

impl Trs {
    pub const IDENTITY: Self = Self {
        translation: [0.0, 0.0, 0.0],
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0, 1.0, 1.0],
    };
}

impl Default for Trs {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// The root of an export: a node forest plus scene-wide animation, environment,
/// and the referenced-only image pool.
#[derive(Clone, Debug, Default)]
pub struct GlbScene {
    /// Top-level (root) nodes; children hang off [`ExportNode::children`].
    pub nodes: Vec<ExportNode>,
    /// Keyframe animations. Channels reference nodes by their **depth-first
    /// flatten index** (see [`ExportAnimChannel::node_index`]). Empty in Phase 1.
    pub animations: Vec<ExportAnimation>,
    /// Skins (skeletons). A node binds to one via [`ExportNode::skin`] (an index
    /// into this vector); its mesh carries per-vertex [`ExportNode::joints`] /
    /// [`ExportNode::weights`]. Empty for non-skinned scenes.
    pub skins: Vec<ExportSkin>,
    /// Image pool. Material texture refs index into this vector. The caller adds
    /// only images that assigned materials actually reference (the referenced-only
    /// rule); the writer embeds them all into the GLB `BIN` chunk.
    pub images: Vec<ExportImage>,
    /// Skybox / IBL references. glTF cannot carry IBL, so the player bundle
    /// (Phase 6) emits this as a sidecar; Phase 1 leaves it `None`.
    pub env: Option<EnvRef>,
}

/// A skin (skeleton) — a set of joint nodes + their inverse-bind matrices. The
/// player rebuilds the skinned deformation from this + the mesh's per-vertex
/// `JOINTS_0`/`WEIGHTS_0`; our clips animate the joint nodes' TRS.
#[derive(Clone, Debug, Default)]
pub struct ExportSkin {
    /// Joint node indices, by **depth-first flatten index** over [`GlbScene::nodes`]
    /// (the same order the writer assigns glTF node indices). Order matters: a
    /// vertex's `JOINTS_0` indexes into this list.
    pub joints: Vec<usize>,
    /// Per-joint inverse-bind matrix, column-major 16 floats (matches glTF's
    /// `inverseBindMatrices` accessor). Empty ⇒ identity for all joints.
    pub inverse_bind_matrices: Vec<[f32; 16]>,
    /// Optional skeleton-root node (flatten index). `None` lets the loader infer.
    pub skeleton: Option<usize>,
}

/// One morph target: per-vertex position (and optional normal) **deltas** added
/// to the base mesh, scaled by the target's weight. Parallel to the mesh's
/// vertices.
#[derive(Clone, Debug, Default)]
pub struct MorphTarget {
    pub name: Option<String>,
    /// Position deltas, one per base vertex.
    pub positions: Vec<[f32; 3]>,
    /// Optional normal deltas, one per base vertex.
    pub normals: Option<Vec<[f32; 3]>>,
}

/// One node in the export forest.
#[derive(Clone, Debug)]
pub struct ExportNode {
    pub name: String,
    pub transform: Trs,
    /// Baked triangle geometry for this node, if any.
    pub mesh: Option<MeshData>,
    /// The material applied to [`Self::mesh`]'s single primitive.
    pub material: Option<ExportMaterial>,
    /// Skin binding: the [`GlbScene::skins`] index this node's mesh is skinned by.
    /// Requires [`Self::joints`] + [`Self::weights`] on the mesh.
    pub skin: Option<usize>,
    /// Per-vertex `JOINTS_0` (4 joint indices into the bound skin's joint list),
    /// one per mesh vertex. `Some` only for skinned meshes.
    pub joints: Option<Vec<[u16; 4]>>,
    /// Per-vertex `WEIGHTS_0` (4 blend weights, summing to ~1), one per mesh
    /// vertex. `Some` only for skinned meshes.
    pub weights: Option<Vec<[f32; 4]>>,
    /// Morph targets on this node's mesh (position/normal deltas). Empty = none.
    pub morph_targets: Vec<MorphTarget>,
    /// Default morph-target weights (one per [`Self::morph_targets`] entry).
    pub morph_weights: Vec<f32>,
    /// Punctual light at this node (`KHR_lights_punctual`).
    pub light: Option<ExportLight>,
    /// Camera at this node.
    pub camera: Option<ExportCamera>,
    pub children: Vec<ExportNode>,
}

impl Default for ExportNode {
    fn default() -> Self {
        Self {
            name: String::new(),
            transform: Trs::IDENTITY,
            mesh: None,
            material: None,
            skin: None,
            joints: None,
            weights: None,
            morph_targets: Vec::new(),
            morph_weights: Vec::new(),
            light: None,
            camera: None,
            children: Vec::new(),
        }
    }
}

impl ExportNode {
    /// A named, identity-transform node with no payload.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Builder: attach baked geometry.
    pub fn with_mesh(mut self, mesh: MeshData) -> Self {
        self.mesh = Some(mesh);
        self
    }

    /// Builder: attach a material.
    pub fn with_material(mut self, material: ExportMaterial) -> Self {
        self.material = Some(material);
        self
    }
}

/// How a material is emitted into the glTF. See the crate-level material policy.
#[derive(Clone, Debug)]
pub enum ExportMaterial {
    /// Real glTF metallic-roughness PBR.
    Pbr(PbrMaterial),
    /// `KHR_materials_unlit` — base color only, no lighting.
    Unlit(UnlitMaterial),
    /// Not glTF-representable: emit the [`AWSM_MATERIALS_NONE`] primitive
    /// extension and **no** embedded glTF material. `id` is an optional stable
    /// material id the player-bundle manifest resolves (node/primitive →
    /// material); `None` round-trips as an empty slot.
    None { id: Option<String> },
}

/// glTF metallic-roughness PBR parameters. All textures are optional refs into
/// [`GlbScene::images`].
#[derive(Clone, Debug)]
pub struct PbrMaterial {
    pub name: String,
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: [f32; 3],
    pub alpha_mode: AlphaMode,
    pub double_sided: bool,
    pub base_color_texture: Option<TexRef>,
    pub metallic_roughness_texture: Option<TexRef>,
    pub normal_texture: Option<TexRef>,
    pub occlusion_texture: Option<TexRef>,
    pub emissive_texture: Option<TexRef>,
}

impl Default for PbrMaterial {
    fn default() -> Self {
        Self {
            name: String::new(),
            base_color: [1.0, 1.0, 1.0, 1.0],
            metallic: 1.0,
            roughness: 1.0,
            emissive: [0.0, 0.0, 0.0],
            alpha_mode: AlphaMode::Opaque,
            double_sided: false,
            base_color_texture: None,
            metallic_roughness_texture: None,
            normal_texture: None,
            occlusion_texture: None,
            emissive_texture: None,
        }
    }
}

/// `KHR_materials_unlit` material — only the base color (factor + optional
/// texture) is meaningful.
#[derive(Clone, Debug)]
pub struct UnlitMaterial {
    pub name: String,
    pub base_color: [f32; 4],
    pub base_color_texture: Option<TexRef>,
    pub alpha_mode: AlphaMode,
    pub double_sided: bool,
}

impl Default for UnlitMaterial {
    fn default() -> Self {
        Self {
            name: String::new(),
            base_color: [1.0, 1.0, 1.0, 1.0],
            base_color_texture: None,
            alpha_mode: AlphaMode::Opaque,
            double_sided: false,
        }
    }
}

/// glTF alpha rendering mode.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum AlphaMode {
    #[default]
    Opaque,
    Mask {
        cutoff: f32,
    },
    Blend,
}

/// A reference from a material slot to an image in [`GlbScene::images`].
#[derive(Clone, Copy, Debug)]
pub struct TexRef {
    /// Index into [`GlbScene::images`].
    pub image: usize,
    /// Which `TEXCOORD_n` set the material samples (usually 0).
    pub tex_coord: u32,
}

impl TexRef {
    pub fn new(image: usize) -> Self {
        Self {
            image,
            tex_coord: 0,
        }
    }
}

/// An embedded image (referenced-only). Stored in the GLB `BIN` chunk via a
/// buffer view + `image.mimeType`.
#[derive(Clone, Debug)]
pub struct ExportImage {
    pub name: String,
    pub bytes: Vec<u8>,
    pub mime: ImageMime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageMime {
    Png,
    Jpeg,
}

impl ImageMime {
    pub fn as_str(self) -> &'static str {
        match self {
            ImageMime::Png => "image/png",
            ImageMime::Jpeg => "image/jpeg",
        }
    }
}

/// Punctual light (`KHR_lights_punctual`). glTF directional/point/spot.
#[derive(Clone, Copy, Debug)]
pub enum ExportLight {
    Directional {
        color: [f32; 3],
        intensity: f32,
    },
    Point {
        color: [f32; 3],
        intensity: f32,
        range: Option<f32>,
    },
    Spot {
        color: [f32; 3],
        intensity: f32,
        range: Option<f32>,
        inner_cone_angle: f32,
        outer_cone_angle: f32,
    },
}

/// glTF camera projection.
#[derive(Clone, Copy, Debug)]
pub enum ExportCamera {
    Perspective {
        yfov: f32,
        aspect_ratio: Option<f32>,
        znear: f32,
        zfar: Option<f32>,
    },
    Orthographic {
        xmag: f32,
        ymag: f32,
        znear: f32,
        zfar: f32,
    },
}

/// A keyframe animation. Lowered to glTF animations on write.
#[derive(Clone, Debug)]
pub struct ExportAnimation {
    pub name: String,
    pub channels: Vec<ExportAnimChannel>,
}

/// One animation channel: a sampler (times → values) bound to a node TRS / morph
/// target.
#[derive(Clone, Debug)]
pub struct ExportAnimChannel {
    /// Target node, by its **depth-first flatten index** over [`GlbScene::nodes`]
    /// (root nodes first, then each node's children, recursively). This is the
    /// same order the writer assigns glTF node indices.
    pub node_index: usize,
    pub path: AnimPath,
    pub interpolation: AnimInterp,
    /// Keyframe input times (seconds), strictly increasing.
    pub times: Vec<f32>,
    /// Keyframe output values, flattened: 3/comp for translation+scale, 4 for
    /// rotation, N for weights (and ×3 for `CubicSpline`, in/vertex/out order).
    pub values: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimPath {
    Translation,
    Rotation,
    Scale,
    Weights,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimInterp {
    Linear,
    Step,
    CubicSpline,
}

/// Environment references (skybox / IBL). Written to a sidecar by the player
/// bundle (Phase 6); glTF itself carries no IBL.
#[derive(Clone, Debug, Default)]
pub struct EnvRef {
    pub skybox: Option<String>,
    pub ibl: Option<String>,
}
