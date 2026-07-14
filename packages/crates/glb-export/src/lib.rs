//! Scene-complete glTF/GLB **export** IR + writer.
//!
//! This crate is the one-way bake target: an editor project (any mix of
//! procedural recipes, raw-edited meshes, and imported models) is flattened to
//! triangles + materials and handed here as a [`GlbScene`], which [`write_glb`]
//! serializes to a self-contained `.glb` byte vector.
//!
//! It has **no GPU / editor / wasm dependencies** — only `awsm-renderer-meshgen` (for the
//! plain-data [`MeshData`]) and `gltf-json` (the JSON model used to *build* a
//! glTF). That keeps the whole writer natively unit-testable (`cargo test -p
//! awsm-renderer-glb-export`).
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

mod compress;
mod extract;
mod quant;
mod tangents;
mod write;

pub use awsm_renderer_meshgen::MeshData;
pub use compress::{compress_glb, strip_materials_and_images};
pub use extract::{
    extract_node_mesh, extract_node_mesh_from_bytes, extract_node_mesh_with_skin_from_bytes,
    extract_texture_images, extract_texture_images_from_bytes,
    extract_texture_images_with_external, reexport_clean, reexport_clean_scene,
    reexport_clean_scene_with_images, scene_node_flat_indices, ExtractedMorph, ExtractedNodeMesh,
    ExtractedSkin,
};
pub use write::write_glb;

/// The primitive-level glTF extension marking a primitive whose real material is
/// **not** glTF-representable and must be resolved by the scene/player on import
/// (rather than defaulting to a glTF material). Defined here once; the importer
/// (`renderer-gltf`) recognizes the same token to leave the material slot empty.
pub const AWSM_MATERIALS_NONE: &str = "AWSM_materials_none";

/// A translate / rotate (xyzw quaternion) / scale local transform. Mirrors
/// `awsm_renderer_scene::Trs` but kept local so this crate stays decoupled from the
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
    /// Per-vertex `TANGENT` (vec4: xyz + handedness), one per mesh vertex, carried
    /// from the source glTF's AUTHORED tangent attribute. `Some` ⇒ the writer emits
    /// these verbatim instead of regenerating via MikkTSpace, so a save→reload
    /// round-trip preserves the exact basis a normal map was baked against
    /// (regenerated tangents shade differently — see the writer's TANGENT branch).
    /// `None` ⇒ the writer bakes tangents from normals+uvs.
    pub tangents: Option<Vec<[f32; 4]>>,
    /// Morph targets on this node's mesh (position/normal deltas). Empty = none.
    pub morph_targets: Vec<MorphTarget>,
    /// Default morph-target weights (one per [`Self::morph_targets`] entry).
    pub morph_weights: Vec<f32>,
    /// Additional primitives on this node's mesh, each with its OWN material.
    /// glTF materials are per-primitive, so a multi-material source mesh
    /// round-trips as one primitive per material on the SAME node — node count
    /// untouched, which keeps skin-joint flatten indices (and therefore
    /// animation-clip bindings) valid. The writer emits these after the main
    /// primitive in the same glTF mesh. Empty for the common single-material
    /// case.
    pub extra_primitives: Vec<ExtraPrimitive>,
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
            tangents: None,
            morph_targets: Vec::new(),
            morph_weights: Vec::new(),
            extra_primitives: Vec::new(),
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

/// An additional primitive on an [`ExportNode`]'s mesh — see
/// [`ExportNode::extra_primitives`]. Carries everything a primitive owns:
/// geometry, material, skinning attributes, and morph deltas. Its morph-target
/// COUNT must match the main primitive's (a glTF mesh has one `weights` array
/// shared by all primitives); a valid source glTF guarantees this.
#[derive(Clone, Debug, Default)]
pub struct ExtraPrimitive {
    pub mesh: MeshData,
    pub material: Option<ExportMaterial>,
    /// Per-vertex `JOINTS_0` for THIS primitive's vertices.
    pub joints: Option<Vec<[u16; 4]>>,
    /// Per-vertex `WEIGHTS_0` for THIS primitive's vertices.
    pub weights: Option<Vec<[f32; 4]>>,
    /// Per-vertex authored `TANGENT` for THIS primitive's vertices (see
    /// [`ExportNode::tangents`]). `Some` ⇒ emitted verbatim, not regenerated.
    pub tangents: Option<Vec<[f32; 4]>>,
    /// This primitive's morph targets (deltas parallel to its vertices).
    pub morph_targets: Vec<MorphTarget>,
}

/// How a material is emitted into the glTF. See the crate-level material policy.
// PbrMaterial is the large variant (it carries the per-material extension JSON). This
// is a cold export IR — one value per material at export time, never a hot path — so
// the size skew doesn't matter; boxing would only add an alloc + indirection.
#[allow(clippy::large_enum_variant)]
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
    /// `KHR_materials_ior` — index of refraction (`None` = absent / default 1.5).
    pub ior: Option<f32>,
    /// `KHR_materials_emissive_strength` — emissive scale (`None` = absent / 1.0).
    pub emissive_strength: Option<f32>,
    /// Other KHR_* material extensions, by extension name → already-prepared JSON
    /// object (texture `index`es ALREADY remapped to the clean glb's pool indices).
    /// Written verbatim into the material's `extensions.others` map. The extractor
    /// fills this (typed extensions built from the gltf accessors; raw ones passed
    /// through + index-remapped) so `write_glb` stays a dumb serializer (GAP 3).
    pub extensions_json: serde_json::Map<String, serde_json::Value>,
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
            ior: None,
            emissive_strength: None,
            extensions_json: serde_json::Map::new(),
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

/// `KHR_texture_transform` — a UV offset/rotation/scale (+ optional texcoord override)
/// on a single textureInfo. Carried on [`TexRef`] so the clean re-export preserves it.
#[derive(Clone, Copy, Debug)]
pub struct TexTransform {
    pub offset: [f32; 2],
    pub rotation: f32,
    pub scale: [f32; 2],
    pub tex_coord: Option<u32>,
}

/// A reference from a material slot to an image in [`GlbScene::images`].
#[derive(Clone, Copy, Debug)]
pub struct TexRef {
    /// Index into [`GlbScene::images`].
    pub image: usize,
    /// Which `TEXCOORD_n` set the material samples (usually 0).
    pub tex_coord: u32,
    /// `KHR_texture_transform` on this textureInfo, if any.
    pub transform: Option<TexTransform>,
}

impl TexRef {
    pub fn new(image: usize) -> Self {
        Self {
            image,
            tex_coord: 0,
            transform: None,
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
    /// Basis-supercompressed KTX2 (`KHR_texture_basisu`). Carried for
    /// passthrough export; the writer declares the extension when embedding.
    Ktx2,
}

impl ImageMime {
    pub fn as_str(self) -> &'static str {
        match self {
            ImageMime::Png => "image/png",
            ImageMime::Jpeg => "image/jpeg",
            ImageMime::Ktx2 => "image/ktx2",
        }
    }

    /// File extension (no dot) for this mime — for content-hash-addressed
    /// `assets/<hash>.<ext>` side files.
    pub fn ext(self) -> &'static str {
        match self {
            ImageMime::Png => "png",
            ImageMime::Jpeg => "jpg",
            ImageMime::Ktx2 => "ktx2",
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
