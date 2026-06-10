//! glTF scene population into renderer resources.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use awsm_renderer_core::texture::texture_pool::TextureColorInfo;
use glam::Mat4;

use awsm_renderer::materials::MaterialKey;
use awsm_renderer::{
    lights::LightKey, meshes::MeshKey, textures::TextureKey, transforms::TransformKey, AwsmRenderer,
};

use crate::{data::GltfData, error::AwsmGltfError};

pub(crate) mod animation;
pub(crate) mod extensions;
pub mod lights;
pub mod material;
pub(crate) mod mesh;
pub(crate) mod skin;
pub(crate) mod transforms;

/// Per-node skin info: the joint transforms + the inverse-bind matrices.
/// Lookup is keyed by glTF node index.
type NodeSkinTransform = Arc<(Vec<TransformKey>, Vec<SkinInverseBindMatrix>)>;

/// Per-node mesh entry: optional mesh name + the mesh keys created for each
/// of the node's primitives. Used as the value type in
/// `GltfKeyLookups::node_meshes`.
type NodeMeshEntry = (Option<String>, Vec<MeshKey>);

/// Where a primitive's renderer material comes from during populate.
///
/// The unifying knob that lets foreign glTF and our runtime glbs share one mesh
/// path: foreign glTF builds materials from the document; our geometry-only
/// runtime glbs carry NO materials (stripped at export — materials are ours,
/// applied from `scene.toml`), so the bundle loader supplies the one material
/// the node was assigned. Supplying it here avoids minting (and compiling a
/// pipeline for) a throwaway default PBR material, then replacing it.
#[derive(Clone, Copy, Debug, Default)]
pub enum GltfMaterialSource {
    /// Build (and dedupe) materials from the glTF document — the foreign-glTF
    /// default.
    #[default]
    Document,
    /// Use this one pre-built material for every primitive; skip the glTF
    /// material + texture creation entirely. Matches our one-material-per-node
    /// runtime model.
    Single(MaterialKey),
}

/// Options for [`populate_gltf`](crate::populate::populate_gltf). `Default` is
/// the foreign-glTF behavior (build materials from the document, finalize
/// textures at the end), so existing single-asset callers are unchanged.
#[derive(Clone, Copy, Debug, Default)]
pub struct PopulateGltfOpts {
    /// glTF scene index to load (`None` = the default/first scene).
    pub scene: Option<usize>,
    /// Root the document's scene nodes under this transform instead of the
    /// renderer root (used to place a bundle glb under its scene node's TRS).
    pub parent_transform: Option<TransformKey>,
    /// Where each primitive's material comes from.
    pub material_source: GltfMaterialSource,
    /// Whether to `finalize_gpu_textures` at the end. `false` lets a batch
    /// loader (the bundle) stage textures across many glbs and finalize once.
    pub finalize_textures: bool,
}

impl PopulateGltfOpts {
    /// Foreign-glTF defaults: build materials from the document + finalize
    /// textures. (A bare `Default::default()` sets `finalize_textures: false`;
    /// this is the "behaves like the old `populate_gltf`" constructor.)
    pub fn foreign() -> Self {
        Self {
            finalize_textures: true,
            ..Default::default()
        }
    }
}

/// Context and shared state used while populating glTF data.
pub struct GltfPopulateContext {
    pub data: Arc<GltfData>,
    pub textures: Mutex<HashMap<GltfTextureKey, TextureKey>>,
    pub(super) material_keys: Mutex<HashMap<GltfMaterialLookupKey, MaterialKey>>,
    /// Where primitive materials come from this populate (see
    /// [`GltfMaterialSource`]).
    pub(super) material_source: GltfMaterialSource,
    pub node_to_skin_transform: Mutex<HashMap<GltfIndex, NodeSkinTransform>>,
    pub transform_is_joint: Mutex<HashSet<TransformKey>>,
    pub transform_is_instanced: Mutex<HashSet<TransformKey>>,
    pub(super) node_animation_samplers: HashMap<GltfIndex, GltfNodeAnimationSamplers>,
    pub key_lookups: Arc<Mutex<GltfKeyLookups>>,
    /// Renderer light keys created from KHR_lights_punctual nodes during this
    /// populate. Empty when the asset doesn't reference the extension.
    pub punctual_lights: Vec<LightKey>,
}

/// Lookup tables for glTF node, mesh, and primitive keys.
#[derive(Debug, Clone, Default)]
pub struct GltfKeyLookups {
    pub node_transforms: HashMap<String, TransformKey>,
    // for all nodes with a name, get mesh_keys per primitive for that node, and optional mesh name
    pub node_meshes: HashMap<String, Vec<NodeMeshEntry>>,
    // for all the meshes with a name, get mesh_keys per primitive for that mesh
    pub mesh_primitives: HashMap<String, Vec<MeshKey>>,
    pub node_index_to_transform: HashMap<GltfIndex, TransformKey>,
    pub all_mesh_keys: HashMap<GltfIndex, Vec<MeshKey>>,
    /// For each renderer `MeshKey` produced by the glTF populate pass,
    /// the originating glTF material index (`None` if the primitive had
    /// no material set, which glTF treats as the spec default material).
    ///
    /// Consumers like the editor's scene-editor crate use this to
    /// override the renderer-baked material with an editable
    /// `MaterialDef` extracted at import time — see
    /// `crates/frontend/scene-editor/src/renderer_bridge/node_sync.rs`.
    pub mesh_key_to_gltf_material_index: HashMap<MeshKey, Option<GltfIndex>>,
}

impl GltfKeyLookups {
    /// Records a transform key for a glTF node.
    pub fn insert_transform(&mut self, node: &gltf::Node, key: TransformKey) {
        if let Some(name) = node.name() {
            self.node_transforms.insert(name.to_string(), key);
        }

        self.node_index_to_transform.insert(node.index(), key);
    }

    /// Records a mesh key for a glTF node and mesh.
    pub fn insert_mesh(&mut self, node: &gltf::Node, mesh: &gltf::Mesh, mesh_key: MeshKey) {
        self.all_mesh_keys
            .entry(mesh.index())
            .or_default()
            .push(mesh_key);

        if let Some(mesh_name) = mesh.name() {
            self.mesh_primitives
                .entry(mesh_name.to_string())
                .or_default()
                .push(mesh_key);
        }

        if let Some(node_name) = node.name() {
            let entry = self.node_meshes.entry(node_name.to_string()).or_default();
            match mesh.name() {
                None => {
                    // no mesh name, just add to the list with None
                    entry.push((None, vec![mesh_key]));
                }
                Some(name) => {
                    // see if we already have an entry for this mesh name
                    let mut found = false;
                    for (mesh_name_opt, mesh_keys) in entry.iter_mut() {
                        if let Some(mesh_name) = mesh_name_opt {
                            if mesh_name == name {
                                mesh_keys.push(mesh_key);
                                found = true;
                            }
                        }
                    }

                    // otherwise add a new entry
                    if !found {
                        entry.push((Some(name.to_string()), vec![mesh_key]));
                    }
                }
            }
        }
    }

    /// Returns an iterator over meshes for a node name.
    pub fn meshes_for_node_iter(&self, node_name: &str) -> impl Iterator<Item = &MeshKey> {
        self.node_meshes
            .get(node_name)
            .into_iter()
            .flat_map(|entries| entries.iter())
            .flat_map(|(_mesh_name_opt, mesh_keys)| mesh_keys.iter())
    }
}

/// Key that identifies a glTF texture plus color info.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct GltfTextureKey {
    pub index: GltfIndex,
    pub color: TextureColorInfo,
}

type SkinInverseBindMatrix = Mat4;

type GltfIndex = usize;

#[derive(Clone, Copy, Debug)]
pub(super) struct GltfAnimationSamplerRef {
    pub animation_index: usize,
    pub channel_index: usize,
    pub sampler_index: usize,
}

#[derive(Clone, Debug, Default)]
pub(super) struct GltfNodeAnimationSamplers {
    pub translation: Option<GltfAnimationSamplerRef>,
    pub rotation: Option<GltfAnimationSamplerRef>,
    pub scale: Option<GltfAnimationSamplerRef>,
    pub morph: Option<GltfAnimationSamplerRef>,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(super) struct GltfMaterialLookupKey {
    pub material_index: Option<usize>,
    pub vertex_color_set_index: Option<usize>,
    pub hud: bool,
}

/// Populates renderer resources from a glTF asset.
///
/// The driver: walks the scene tree four times — transforms (so node-index →
/// transform-key lookup is populated before mesh / skin / animation paths
/// need it), then EXT_mesh_gpu_instancing, then skinning, then animation,
/// then meshes. Each pass uses the per-crate extension traits defined in
/// the `populate::{transforms, mesh, skin, animation, extensions::instancing}`
/// submodules.
pub async fn populate_gltf(
    renderer: &mut AwsmRenderer,
    gltf_data: impl Into<Arc<GltfData>>,
    opts: PopulateGltfOpts,
) -> anyhow::Result<GltfPopulateContext> {
    use crate::populate::animation::GltfAnimationExt;
    use crate::populate::extensions::instancing::GltfInstancingExt;
    use crate::populate::mesh::GltfMeshExt;
    use crate::populate::skin::GltfSkinExt;
    use crate::populate::transforms::GltfTransformsExt;

    let PopulateGltfOpts {
        scene,
        parent_transform,
        material_source,
        finalize_textures,
    } = opts;

    let gltf_data = gltf_data.into();
    // The old `awsm-renderer` `gltf` cache field stored these `Arc<GltfData>`
    // refs write-only; nothing in the renderer ever read them back. Removed
    // as part of the C-2 extraction.

    let mut mesh_keys = Vec::new();
    let node_animation_samplers = build_node_animation_sampler_lookup(&gltf_data.doc);

    let mut ctx = GltfPopulateContext {
        data: gltf_data,
        textures: Mutex::new(HashMap::new()),
        material_keys: Mutex::new(HashMap::new()),
        material_source,
        node_to_skin_transform: Mutex::new(HashMap::new()),
        transform_is_joint: Mutex::new(HashSet::new()),
        transform_is_instanced: Mutex::new(HashSet::new()),
        node_animation_samplers,
        key_lookups: Arc::new(Mutex::new(GltfKeyLookups::default())),
        punctual_lights: Vec::new(),
    };

    let scene = match scene {
        Some(index) => ctx
            .data
            .doc
            .scenes()
            .nth(index)
            .ok_or(AwsmGltfError::InvalidScene(index))?,
        None => match ctx.data.doc.default_scene() {
            Some(scene) => scene,
            None => ctx
                .data
                .doc
                .scenes()
                .next()
                .ok_or(AwsmGltfError::NoDefaultScene)?,
        },
    };

    for node in scene.nodes() {
        renderer.populate_gltf_node_transform(&ctx, &node, parent_transform)?;
    }

    for node in scene.nodes() {
        renderer.populate_gltf_node_extension_instancing(&ctx, &node)?;
    }

    for node in scene.nodes() {
        renderer.populate_gltf_node_skin(&ctx, &node)?;
    }

    for node in scene.nodes() {
        renderer.populate_gltf_node_animation(&ctx, &node)?;
    }

    for node in scene.nodes() {
        mesh_keys.push(renderer.populate_gltf_node_mesh(&ctx, &node).await?);
    }

    ctx.punctual_lights = crate::populate::lights::populate_gltf_lights(renderer, &ctx)?;

    // A batch loader (the bundle) defers this so it can stage textures across
    // many glbs and commit them in one upload; a single-asset caller finalizes
    // here. (For `GltfMaterialSource::Single` no glTF textures are created, so
    // this is a near no-op regardless.)
    if finalize_textures {
        renderer.finalize_gpu_textures().await?;
    }

    Ok(ctx)
}

impl GltfPopulateContext {
    pub(super) fn resolve_animation_sampler(
        &self,
        sampler_ref: GltfAnimationSamplerRef,
    ) -> Result<gltf::animation::Sampler<'_>, AwsmGltfError> {
        self.data
            .doc
            .animations()
            .nth(sampler_ref.animation_index)
            .and_then(|animation| animation.samplers().nth(sampler_ref.sampler_index))
            .ok_or(AwsmGltfError::MissingAnimationSampler {
                animation_index: sampler_ref.animation_index,
                channel_index: sampler_ref.channel_index,
                sampler_index: sampler_ref.sampler_index,
            })
    }
}

fn build_node_animation_sampler_lookup(
    doc: &gltf::Document,
) -> HashMap<GltfIndex, GltfNodeAnimationSamplers> {
    let mut out = HashMap::<GltfIndex, GltfNodeAnimationSamplers>::new();

    for animation in doc.animations() {
        for channel in animation.channels() {
            let node_index = channel.target().node().index();
            let entry = out.entry(node_index).or_default();
            let sampler_ref = GltfAnimationSamplerRef {
                animation_index: animation.index(),
                channel_index: channel.index(),
                sampler_index: channel.sampler().index(),
            };

            match channel.target().property() {
                gltf::animation::Property::Translation => {
                    if entry.translation.is_none() {
                        entry.translation = Some(sampler_ref);
                    }
                }
                gltf::animation::Property::Rotation => {
                    if entry.rotation.is_none() {
                        entry.rotation = Some(sampler_ref);
                    }
                }
                gltf::animation::Property::Scale => {
                    if entry.scale.is_none() {
                        entry.scale = Some(sampler_ref);
                    }
                }
                gltf::animation::Property::MorphTargetWeights => {
                    if entry.morph.is_none() {
                        entry.morph = Some(sampler_ref);
                    }
                }
            }
        }
    }

    out
}
