use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

use awsm_renderer::camera::CameraKey;
use awsm_renderer::decals::DecalKey;
use awsm_renderer::lights::LightKey;
use awsm_renderer::materials::MaterialKey;
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::render_passes::lines::LineKey;
use awsm_renderer::transforms::TransformKey;
use awsm_renderer_web_shared::prelude::AsyncLoader;

use super::asset_template::AssetTemplate;
use super::{animation_sync, env_sync, mesh_sync, node_sync};
use crate::engine::scene::{AssetId, Node, NodeId, NodeKind};
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
    /// The renderer camera-params slot, if this node is a Camera. Mirrors the
    /// node's `CameraConfig` (kept in sync by the kind observer) and is the
    /// store an `AnimationTarget::Camera` channel mutates.
    pub camera_key: Mutex<Option<CameraKey>>,
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
            camera_key: Mutex::new(None),
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
    /// `SkinnedMesh` nodes resolve their populate-baked renderer mesh keys here
    /// (see `node_sync::materialize_skinned_mesh` + `AssetTemplate`). Only
    /// skinned imports need this; static geometry bakes to captured meshes.
    pub templates: Mutex<HashMap<AssetId, Arc<AssetTemplate>>>,
    /// Live editor nodes minted for each import, keyed by the import's template
    /// `AssetId` — the refcount that lets a template's populate-baked renderer
    /// resources be reclaimed mid-session when its LAST instance is deleted
    /// (not just at project reset). Populated at import
    /// (`finish_model_import`), drained per-node in `node_sync::remove_node`.
    pub template_instances: Mutex<HashMap<AssetId, HashSet<NodeId>>>,
    /// Reverse of [`Self::template_instances`]: editor node → its origin import
    /// template, so a removed node can be untracked in O(1).
    pub node_to_template: Mutex<HashMap<NodeId, AssetId>>,
    /// Skin bridge: editor bone `NodeId` → the baked joint `TransformKey` the
    /// renderer's skin reads. A skinned glTF renders from its baked
    /// `populate_gltf` copy, but the editor drives a *separate* mirror-bone
    /// transform; each frame [`skin_bridge`] copies the mirror's local onto the
    /// baked key so animation + posing actually deform the skin (#2).
    pub skin_joint_baked: Mutex<HashMap<NodeId, TransformKey>>,
    /// Each skin-joint node's IMPORT-TIME local transform — the glTF rest/bind
    /// pose. Captured once per import (alongside [`Self::skin_joint_baked`])
    /// and read by `ResetToBindPose`, which is otherwise impossible after
    /// direct `SetTransform` pose edits mutate the scene-base transforms
    /// (handoff #8: `reset_pose` re-syncs FROM scene base, so it can't undo
    /// them).
    pub joint_rest: Mutex<HashMap<NodeId, crate::engine::scene::Trs>>,
}

impl Bridge {
    fn new() -> Self {
        Self {
            nodes: Mutex::new(HashMap::new()),
            light_node_ids: Mutex::new(HashSet::new()),
            child_order: Mutex::new(HashMap::new()),
            mesh_to_node: Mutex::new(HashMap::new()),
            templates: Mutex::new(HashMap::new()),
            template_instances: Mutex::new(HashMap::new()),
            node_to_template: Mutex::new(HashMap::new()),
            skin_joint_baked: Mutex::new(HashMap::new()),
            joint_rest: Mutex::new(HashMap::new()),
        }
    }

    /// Cache a glTF node template under its source file's `AssetId` (skinned
    /// imports only). `SkinnedMesh` nodes look up their meshes here.
    pub fn insert_template(&self, id: AssetId, template: Arc<AssetTemplate>) {
        self.templates.lock().unwrap().insert(id, template);
    }
    /// The node template for an imported glTF, if still cached.
    pub fn get_template(&self, id: AssetId) -> Option<Arc<AssetTemplate>> {
        self.templates.lock().unwrap().get(&id).cloned()
    }

    /// Track every editor node minted for an import against its template id, so
    /// the template's renderer resources can be freed when the last instance is
    /// deleted mid-session. Called once at import with the whole minted subtree.
    pub fn register_template_instances(
        &self,
        id: AssetId,
        nodes: impl IntoIterator<Item = NodeId>,
    ) {
        let mut inst = self.template_instances.lock().unwrap();
        let mut rev = self.node_to_template.lock().unwrap();
        let set = inst.entry(id).or_default();
        for n in nodes {
            set.insert(n);
            rev.insert(n, id);
        }
    }

    /// Drop a node from template tracking (called for every removed node).
    /// Returns the template id the node belonged to, if any — the caller then
    /// checks [`Self::template_instance_count`] to decide whether to reclaim.
    pub fn untrack_template_node(&self, node: NodeId) -> Option<AssetId> {
        let id = self.node_to_template.lock().unwrap().remove(&node)?;
        if let Some(set) = self.template_instances.lock().unwrap().get_mut(&id) {
            set.remove(&node);
        }
        Some(id)
    }

    /// How many tracked instances of this template remain live.
    pub fn template_instance_count(&self, id: AssetId) -> usize {
        self.template_instances
            .lock()
            .unwrap()
            .get(&id)
            .map_or(0, |s| s.len())
    }

    /// Remove a template's metadata + instance tracking (after its renderer
    /// resources have been freed). Counterpart to [`Self::insert_template`].
    pub fn remove_template(&self, id: AssetId) {
        self.templates.lock().unwrap().remove(&id);
        self.template_instances.lock().unwrap().remove(&id);
    }

    /// Whether any live `SkinnedMesh` node still renders from this import's
    /// template (its baked meshes). A duplicated skinned node carries the same
    /// `skin.source` but is NOT in `template_instances`, so this scan — not the
    /// refcount alone — is what keeps template reclamation dangle-free.
    pub fn any_live_skinned_from(&self, id: AssetId) -> bool {
        self.nodes.lock().unwrap().values().any(|n| {
            matches!(
                n.node.kind.get_cloned(),
                NodeKind::SkinnedMesh { skin, .. } if skin.source == id
            )
        })
    }

    /// Register a skinned-model bone: editor `NodeId` → baked joint key (#2).
    pub fn register_skin_joint(&self, node: NodeId, baked: TransformKey) {
        self.skin_joint_baked.lock().unwrap().insert(node, baked);
    }
    /// Record a skin-joint node's import-time local TRS — the bind/rest pose
    /// `ResetToBindPose` restores.
    pub fn register_joint_rest(&self, node: NodeId, rest: crate::engine::scene::Trs) {
        self.joint_rest.lock().unwrap().insert(node, rest);
    }
    /// Drop all skin-joint mappings (project reset).
    pub fn clear_skin_joints(&self) {
        self.skin_joint_baked.lock().unwrap().clear();
        self.joint_rest.lock().unwrap().clear();
    }
    /// Drop a single skin-joint mapping (a skinned-model bone node deleted).
    pub fn unregister_skin_joint(&self, node: NodeId) {
        self.skin_joint_baked.lock().unwrap().remove(&node);
        self.joint_rest.lock().unwrap().remove(&node);
    }

    /// Drop all cached import templates + their instance tracking (project reset).
    pub fn clear_templates(&self) {
        self.templates.lock().unwrap().clear();
        self.template_instances.lock().unwrap().clear();
        self.node_to_template.lock().unwrap().clear();
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

/// Propagate a BUILT-IN library material's def edit to every mesh variant that
/// references it: each variant's per-mesh `instance.inline` store re-seeds
/// **field-wise** — a slot that still MIRRORED the prior library def adopts the
/// new def's value, while a slot the mesh customized (≠ prior def) is preserved.
/// This is `assign_material`-style template→instance carry-over applied in
/// place, and it is what makes `update_builtin_material`'s documented "assigned
/// meshes re-materialize" true for VALUES, not just for shader variants: without
/// it the re-trigger below re-applies the stale inline copy and nothing visibly
/// changes (glTF-imported materials seeded every mesh's inline store at import).
/// Every touched node's kind is re-`set`, which fires its observer and
/// re-materializes it.
pub fn reseed_inline_for_material(
    id: crate::engine::scene::AssetId,
    prior: &awsm_renderer_editor_protocol::MaterialDef,
    new_def: &awsm_renderer_editor_protocol::MaterialDef,
) {
    use crate::engine::scene::node::Node;
    use crate::engine::scene::AssetId;
    use awsm_renderer_editor_protocol::MaterialDef;

    /// Field-wise template→instance merge; `true` when anything changed.
    fn reseed(inline: &mut MaterialDef, prior: &MaterialDef, new_def: &MaterialDef) -> bool {
        let mut changed = false;
        macro_rules! seed {
            ($($path:ident).+) => {
                if inline.$($path).+ == prior.$($path).+ && inline.$($path).+ != new_def.$($path).+ {
                    inline.$($path).+ = new_def.$($path).+.clone();
                    changed = true;
                }
            };
        }
        seed!(label);
        seed!(base_color);
        seed!(base_color_texture);
        seed!(metallic);
        seed!(roughness);
        seed!(metallic_roughness_texture);
        seed!(emissive);
        seed!(emissive_texture);
        seed!(normal_texture);
        seed!(normal_scale);
        seed!(occlusion_texture);
        seed!(occlusion_strength);
        seed!(ssr_mask);
        seed!(double_sided);
        seed!(vertex_colors_enabled);
        seed!(alpha_mode);
        seed!(shading);
        // Extensions per-slot, so one customized extension doesn't pin the rest.
        seed!(extensions.emissive_strength);
        seed!(extensions.ior);
        seed!(extensions.specular);
        seed!(extensions.transmission);
        seed!(extensions.diffuse_transmission);
        seed!(extensions.volume);
        seed!(extensions.clearcoat);
        seed!(extensions.sheen);
        seed!(extensions.dispersion);
        seed!(extensions.anisotropy);
        seed!(extensions.iridescence);
        changed
    }

    fn walk(nodes: &[Arc<Node>], id: AssetId, prior: &MaterialDef, new_def: &MaterialDef) {
        for node in nodes {
            let mut kind = node.kind.get_cloned();
            let mut touched = false;
            if let Some(variants) = kind.material_variants_mut() {
                for v in variants.iter_mut() {
                    if v.instance.asset == id {
                        reseed(&mut v.instance.inline, prior, new_def);
                        // Re-set even when no field changed: the def edit may
                        // still be a shader-variant change (extensions toggled)
                        // the observer must pick up.
                        touched = true;
                    }
                }
            }
            if touched {
                node.kind.set(kind);
            }
            walk(&node.children.lock_ref(), id, prior, new_def);
        }
    }

    let ctrl = crate::controller::controller();
    let roots: Vec<Arc<Node>> = ctrl.scene.nodes.lock_ref().iter().cloned().collect();
    walk(&roots, id, prior, new_def);
}

/// Re-materialize every mesh that references custom material `id` — called
/// after it's (re)registered so assigned meshes pick up the now-live shader.
/// Re-triggers each affected node's kind observer (a same-value `set` fires it).
pub fn rematerialize_for_material(id: crate::engine::scene::AssetId) {
    use crate::engine::scene::node::Node;
    use crate::engine::scene::{AssetId, NodeKind};

    fn node_assigned_material(kind: &NodeKind) -> Option<AssetId> {
        kind.selected_material().map(|i| i.asset)
    }

    fn walk(nodes: &[Arc<Node>], id: AssetId) {
        for node in nodes {
            let kind = node.kind.get_cloned();
            // Any geometry node assigned the edited material re-materializes so it
            // picks up the now-live shader / variant edit (a same-value `set`
            // re-triggers the kind observer).
            if node_assigned_material(&kind) == Some(id) {
                node.kind.set(kind.clone());
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
    // runtime and drives the transport clock.
    animation_sync::start();
    // Re-materializes captured-mesh nodes when SetMeshData replaces an editable
    // mesh's bytes (no node-kind change → the kind observer wouldn't re-fire).
    mesh_sync::start();
    // Read-only vertex-selection highlight overlay (draws markers at the
    // controller's `vertex_selection`; no geometry mutation).
    super::vertex_highlight::start();
}
