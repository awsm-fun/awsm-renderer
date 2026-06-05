//! Renderer bridge: mirrors the reactive scene tree onto the GPU renderer.
//! A per-node `RendererNode` tracks the GPU resources a scene node owns
//! (transform, meshes, materials, light); observers materialize/teardown them
//! as the node's kind/transform/visibility change. M4-C handles primitives +
//! lights + passive kinds; models/curves/particles/decals/etc. layer in later.

pub mod animation_sync;
pub mod asset_template;
pub mod collider_wire;
pub mod dynamic;
pub mod env_sync;
pub mod gltf;
pub mod material;
pub mod mesh_cache;
pub mod node_sync;
pub mod particles;

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

use awsm_renderer::decals::DecalKey;
use awsm_renderer::lights::LightKey;
use awsm_renderer::materials::MaterialKey;
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::render_passes::lines::LineKey;
use awsm_renderer::transforms::TransformKey;
use awsm_web_shared::prelude::AsyncLoader;

use crate::engine::scene::{AssetId, Node, NodeId, NodeKind};
use asset_template::AssetTemplate;
use std::sync::{Arc, Mutex};

/// GPU-side mirror of one scene node.
pub struct RendererNode {
    pub node_id: NodeId,
    pub node: Arc<Node>,
    /// The node's own transform in the renderer (parents the node's meshes).
    pub transform_key: TransformKey,
    /// Mesh keys this node materialized (cleared on kind change / teardown).
    pub model_meshes: Mutex<Vec<MeshKey>>,
    /// Sub-transforms created for this node's meshes.
    pub model_transforms: Mutex<Vec<TransformKey>>,
    /// Owned inline materials (freed on teardown).
    pub material_keys: Mutex<Vec<MaterialKey>>,
    /// The renderer light, if this node is a Light.
    pub light_key: Mutex<Option<LightKey>>,
    /// Fat-line strips this node owns (Line / Curve viz / collider wireframe).
    pub line_keys: Mutex<Vec<LineKey>>,
    /// Projection decals this node owns.
    pub decal_keys: Mutex<Vec<DecalKey>>,
    /// Last kind materialized (identity fast-path / teardown gating).
    pub last_kind: Mutex<Option<NodeKind>>,
    /// Per-node observer tasks; dropping cancels them (on node removal).
    pub loaders: Mutex<Vec<AsyncLoader>>,
}

impl RendererNode {
    pub fn new(node: Arc<Node>, transform_key: TransformKey) -> Arc<Self> {
        Arc::new(Self {
            node_id: node.id,
            node,
            transform_key,
            model_meshes: Mutex::new(Vec::new()),
            model_transforms: Mutex::new(Vec::new()),
            material_keys: Mutex::new(Vec::new()),
            light_key: Mutex::new(None),
            line_keys: Mutex::new(Vec::new()),
            decal_keys: Mutex::new(Vec::new()),
            last_kind: Mutex::new(None),
            loaders: Mutex::new(Vec::new()),
        })
    }
}

/// The scene→GPU mirror.
pub struct Bridge {
    pub nodes: Mutex<HashMap<NodeId, Arc<RendererNode>>>,
    /// Index of light nodes for the per-frame light transform sync.
    pub light_node_ids: Mutex<HashSet<NodeId>>,
    /// Ordered child ids per parent (root = `None`) — maps a `RemoveAt` index
    /// back to the node id to tear down.
    pub child_order: Mutex<HashMap<Option<NodeId>, Vec<NodeId>>>,
    /// Reverse map for GPU picking: a hit `MeshKey` → the owning scene node.
    pub mesh_to_node: Mutex<HashMap<MeshKey, NodeId>>,
    /// Per-imported-glTF node templates, keyed by the source file's `AssetId`.
    /// `Model` nodes look up their meshes here (see `asset_template`).
    pub templates: Mutex<HashMap<AssetId, Arc<AssetTemplate>>>,
}

impl Bridge {
    fn new() -> Self {
        Self {
            nodes: Mutex::new(HashMap::new()),
            light_node_ids: Mutex::new(HashSet::new()),
            child_order: Mutex::new(HashMap::new()),
            mesh_to_node: Mutex::new(HashMap::new()),
            templates: Mutex::new(HashMap::new()),
        }
    }

    /// Cache a glTF node template under its source file's `AssetId`.
    pub fn insert_template(&self, id: AssetId, template: Arc<AssetTemplate>) {
        self.templates.lock().unwrap().insert(id, template);
    }
    /// The node template for an imported glTF, if still cached.
    pub fn get_template(&self, id: AssetId) -> Option<Arc<AssetTemplate>> {
        self.templates.lock().unwrap().get(&id).cloned()
    }

    /// Register a materialized mesh so a GPU pick can resolve it to its node.
    pub fn register_mesh(&self, mesh: MeshKey, node: NodeId) {
        self.mesh_to_node.lock().unwrap().insert(mesh, node);
    }
    pub fn unregister_mesh(&self, mesh: MeshKey) {
        self.mesh_to_node.lock().unwrap().remove(&mesh);
    }
    /// The scene node owning a picked mesh, if any.
    pub fn node_for_mesh(&self, mesh: MeshKey) -> Option<NodeId> {
        self.mesh_to_node.lock().unwrap().get(&mesh).copied()
    }
}

/// Re-materialize every mesh that references custom material `id` — called
/// after it's (re)registered so assigned meshes pick up the now-live shader.
/// Re-triggers each affected node's kind observer (a same-value `set` fires it).
pub fn rematerialize_for_material(id: crate::engine::scene::AssetId) {
    use crate::engine::scene::node::Node;
    use crate::engine::scene::{AssetId, NodeKind};

    fn walk(nodes: &[Arc<Node>], id: AssetId) {
        for node in nodes {
            let kind = node.kind.get_cloned();
            match &kind {
                NodeKind::Primitive {
                    custom_material: Some(inst),
                    ..
                } if inst.material == id => {
                    node.kind.set(kind.clone());
                }
                // A Model node carries one assigned library material (set at
                // import, swappable in the inspector); if the edited material is
                // it, re-materialize so the model reflects the edit.
                NodeKind::Model(model_ref)
                    if model_ref.material.as_ref().map(|i| i.material) == Some(id) =>
                {
                    node.kind.set(kind.clone());
                }
                _ => {}
            }
            walk(&node.children.lock_ref(), id);
        }
    }

    let ctrl = crate::controller::controller();
    let roots: Vec<Arc<Node>> = ctrl.scene.nodes.lock_ref().iter().cloned().collect();
    walk(&roots, id);
}

thread_local! {
    static BRIDGE: OnceCell<Arc<Bridge>> = const { OnceCell::new() };
}

/// The bridge singleton (created on first access).
pub fn bridge() -> Arc<Bridge> {
    BRIDGE.with(|b| b.get_or_init(|| Arc::new(Bridge::new())).clone())
}

/// Start mirroring the controller's scene onto the renderer. Call once, after
/// the renderer context is ready.
pub fn init() {
    node_sync::start();
    // Drives the renderer skybox + IBL from `scene.environment`; its first
    // emission applies the default Simple Sky so the editor never boots black.
    env_sync::start();
    // Lowers authored animation clips + mixer into the renderer's clip-group
    // runtime and drives the transport clock (§6.5).
    animation_sync::start();
}
