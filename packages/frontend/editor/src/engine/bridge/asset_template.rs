//! Per-import glTF **template**: a snapshot of the node hierarchy that
//! `populate_gltf` builds in the renderer.
//!
//! When a glTF/glb is imported, the renderer's `populate_gltf` walks the
//! document and creates a full transform tree + meshes (rig + skinning baked
//! in). The editor then *deconstructs* that into its own scene tree: every
//! glTF node becomes an editor [`Node`](crate::engine::scene::node::Node) —
//! a `Group` for pure transform/bone nodes, a `Mesh` for mesh-bearing nodes —
//! preserving each node's local transform.
//!
//! The template is now used only for **materials + structure**: it carries each
//! node's local transform, label, glTF material index per primitive, and skin
//! joint flags. Geometry is baked into captured `NodeKind::Mesh` assets at import
//! (CPU-extracted from the document accessors — see
//! `controller::state::build_editor_subtree`), so the renderer's own
//! `populate_gltf` meshes are *hidden* ([`hide_template_meshes`]) and exist only so
//! `populate_gltf` can extract/upload the materials + textures.

use std::collections::{HashMap, HashSet};

use awsm_renderer::materials::MaterialKey;
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::transforms::{Transform, TransformKey};
use awsm_renderer::AwsmRenderer;
use awsm_renderer_gltf::populate::GltfPopulateContext;

use crate::engine::scene::Trs;

/// One glTF node, mirrored as an editor scene node.
#[derive(Clone)]
pub struct AssetTemplateNode {
    /// Original glTF node index — used by the controller to look up this node's
    /// CPU-extracted geometry (`GltfImport::node_meshes`) and to bind imported
    /// animation channels (keyed by glTF node index) onto the minted editor node.
    pub gltf_node_index: u32,
    /// The renderer `TransformKey` `populate_gltf` baked for this node — the key
    /// the **skin** reads from for skinned meshes. The editor mirrors this node
    /// as its own scene node with a *separate* transform; the per-frame skin
    /// bridge ([`skin_bridge`](super::skin_bridge)) copies the editor node's
    /// local onto this baked key so animation/posing of the mirror bone actually
    /// deforms the skin.
    pub baked_transform_key: TransformKey,
    /// Whether this node's baked transform is a joint of some skin in this glTF
    /// (i.e. the skin's `skeleton_transforms` references it). Only these nodes
    /// need the per-frame editor→baked local copy.
    pub is_skin_joint: bool,
    /// glTF node name, if any.
    pub label: Option<String>,
    /// The node's local transform (as parsed from the glTF).
    pub local: Transform,
    /// Renderer mesh keys for this node's primitives (the template copies; hidden
    /// after import so they don't double-render with the captured Mesh nodes).
    pub mesh_keys: Vec<MeshKey>,
    /// One entry per `mesh_keys[i]`: the renderer material `populate_gltf`
    /// originally assigned to that mesh (captured at snapshot, i.e. before any
    /// editor reassignment). Static (hidden) meshes still carry it as their
    /// current material; **skinned** meshes get a node-owned material reassigned
    /// at materialize time ([`super::node_sync::materialize_skinned_mesh`]),
    /// orphaning this populate one — so it is recorded here and freed explicitly
    /// on teardown ([`remove_template_meshes`]) to reclaim its pooled textures.
    pub mesh_material_keys: Vec<MaterialKey>,
    /// One entry per `mesh_keys[i]`: the originating glTF material index
    /// (`None` ⇒ the primitive had no material, i.e. glTF's default). Used by
    /// the controller to assign each captured Mesh node its imported material and
    /// to decide whether to destructure a multi-material node per-primitive.
    pub mesh_gltf_material_indices: Vec<Option<usize>>,
    /// One entry per `mesh_keys[i]`: whether that primitive is **skinned** (its
    /// renderer resource carries a `SkinKey`). A node with any skinned primitive
    /// becomes a `NodeKind::SkinnedMesh` (the populate-baked mesh keeps rendering
    /// and deforming via the skeleton) rather than a baked-to-bind-pose captured
    /// `Mesh` — this is what fixes the step-2 skinned-import regression.
    pub mesh_is_skinned: Vec<bool>,
    /// One entry per `mesh_keys[i]`: whether that primitive carries morph
    /// targets (geometry or material). Morph-bearing nodes ride the SAME
    /// populate-baked path as skinned ones (`NodeKind::SkinnedMesh`) — a
    /// captured/editable `Mesh` would silently drop the morph buffers, freezing
    /// `set_morph_weight` + morph animation tracks for that node.
    pub mesh_has_morphs: Vec<bool>,
    /// Morph target names for this node's mesh, from the glTF
    /// `mesh.extras.targetNames` convention (empty when absent). Indexed like
    /// the weights `MorphData`/`set_morph_weight` operate on.
    pub morph_target_names: Vec<String>,
    /// `Some` when this glTF node carries a `KHR_lights_punctual` light. The
    /// controller materializes it as an editable `NodeKind::Light` (so it shows
    /// in the outliner, gets the shadow inspector, and — crucially — binds its
    /// renderer light to THIS editor node's transform_key, so animating the node
    /// moves the light). The duplicate light `populate_gltf` baked is removed at
    /// import (see `remove_template_lights`) so they don't double up.
    pub light: Option<awsm_editor_protocol::LightConfig>,
    pub children: Vec<AssetTemplateNode>,
}

/// The whole node tree for one imported glTF/glb.
#[derive(Clone)]
pub struct AssetTemplate {
    pub roots: Vec<AssetTemplateNode>,
}

impl AssetTemplate {
    /// Depth-first lookup of a template node by its glTF node index. The bridge
    /// `materialize_skinned_mesh` path resolves a `SkinnedMeshRef`'s `node_index`
    /// to its populate-baked renderer mesh keys through this.
    pub fn find_by_node_index(&self, node_index: u32) -> Option<&AssetTemplateNode> {
        fn walk(nodes: &[AssetTemplateNode], idx: u32) -> Option<&AssetTemplateNode> {
            for n in nodes {
                if n.gltf_node_index == idx {
                    return Some(n);
                }
                if let Some(found) = walk(&n.children, idx) {
                    return Some(found);
                }
            }
            None
        }
        walk(&self.roots, node_index)
    }
}

/// Snapshot the renderer's transform tree (as just built by `populate_gltf`)
/// into an [`AssetTemplate`], using the populate context's key lookups to
/// recover each node's glTF index, label, and per-mesh material index.
pub fn build_from_context(renderer: &AwsmRenderer, ctx: &GltfPopulateContext) -> AssetTemplate {
    let (key_to_node_index, key_to_label, mesh_mat, all_keys) = {
        let lookups = ctx.key_lookups.lock().unwrap();
        let key_to_node_index: HashMap<TransformKey, u32> = lookups
            .node_index_to_transform
            .iter()
            .map(|(idx, key)| (*key, *idx as u32))
            .collect();
        let key_to_label: HashMap<TransformKey, String> = lookups
            .node_transforms
            .iter()
            .map(|(label, key)| (*key, label.clone()))
            .collect();
        let mesh_mat = lookups.mesh_key_to_gltf_material_index.clone();
        let all_keys: Vec<TransformKey> =
            lookups.node_index_to_transform.values().copied().collect();
        (key_to_node_index, key_to_label, mesh_mat, all_keys)
    };

    // Union of every skin's joint TransformKeys — the baked keys the renderer's
    // skin matrices are derived from. Used to flag which template nodes need the
    // per-frame editor→baked local copy (the skin bridge).
    let skin_joints: HashSet<TransformKey> = {
        let map = ctx.node_to_skin_transform.lock().unwrap();
        map.values().flat_map(|arc| arc.0.iter().copied()).collect()
    };

    // glTF node index → morph target names, from the `mesh.extras.targetNames`
    // convention (the only interoperable home for morph names in glTF 2.0).
    let morph_names_by_node: HashMap<u32, Vec<String>> = ctx
        .data
        .doc
        .nodes()
        .filter_map(|n| {
            let mesh = n.mesh()?;
            let raw = mesh.extras().as_ref()?;
            let v: serde_json::Value = serde_json::from_str(raw.get()).ok()?;
            let names = v.get("targetNames")?.as_array()?;
            Some((
                n.index() as u32,
                names
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect(),
            ))
        })
        .collect();

    // glTF node index → editor LightConfig for every KHR_lights_punctual node,
    // so each light becomes an editable `NodeKind::Light` mirror (bound to the
    // editor node's transform) instead of a dead Group.
    let lights_by_node: HashMap<u32, awsm_editor_protocol::LightConfig> = ctx
        .data
        .doc
        .nodes()
        .filter_map(|n| {
            n.light()
                .map(|l| (n.index() as u32, light_config_from_gltf(&l)))
        })
        .collect();

    let root = renderer.transforms.root_node;
    let top_level: Vec<TransformKey> = all_keys
        .into_iter()
        .filter(|k| renderer.transforms.get_parent(*k).ok() == Some(root))
        .collect();

    let roots = top_level
        .into_iter()
        .map(|k| {
            snapshot(
                renderer,
                k,
                &SnapshotLookups {
                    key_to_node_index: &key_to_node_index,
                    key_to_label: &key_to_label,
                    mesh_mat: &mesh_mat,
                    skin_joints: &skin_joints,
                    lights_by_node: &lights_by_node,
                    morph_names_by_node: &morph_names_by_node,
                },
            )
        })
        .collect();
    AssetTemplate { roots }
}

/// glTF `KHR_lights_punctual` light → editor `LightConfig` (1:1 with
/// `renderer-gltf`'s `to_renderer_light`; position/direction come from the
/// editor node's transform at materialize time). Shadow config defaults to the
/// editor's authored-light default (cast on) — the user controls it via the
/// light inspector's Shadows section.
fn light_config_from_gltf(
    light: &gltf::khr_lights_punctual::Light,
) -> awsm_editor_protocol::LightConfig {
    use awsm_editor_protocol::{LightConfig, LightShadowConfig};
    let color = light.color();
    let intensity = light.intensity();
    // glTF `range` is `Option`; 0.0 means "unlimited" in our renderer.
    let range = light.range().unwrap_or(0.0);
    let shadow = LightShadowConfig::default();
    match light.kind() {
        gltf::khr_lights_punctual::Kind::Directional => LightConfig::Directional {
            color,
            intensity,
            shadow,
        },
        gltf::khr_lights_punctual::Kind::Point => LightConfig::Point {
            color,
            intensity,
            range,
            shadow,
        },
        gltf::khr_lights_punctual::Kind::Spot {
            inner_cone_angle,
            outer_cone_angle,
        } => LightConfig::Spot {
            color,
            intensity,
            range,
            inner_angle: inner_cone_angle,
            outer_angle: outer_cone_angle,
            shadow,
        },
    }
}

/// Read-only lookup tables shared by every node of one template snapshot —
/// bundled so the recursive walk passes one reference instead of six.
struct SnapshotLookups<'a> {
    key_to_node_index: &'a HashMap<TransformKey, u32>,
    key_to_label: &'a HashMap<TransformKey, String>,
    mesh_mat: &'a HashMap<MeshKey, Option<usize>>,
    skin_joints: &'a HashSet<TransformKey>,
    lights_by_node: &'a HashMap<u32, awsm_editor_protocol::LightConfig>,
    morph_names_by_node: &'a HashMap<u32, Vec<String>>,
}

fn snapshot(
    renderer: &AwsmRenderer,
    key: TransformKey,
    lk: &SnapshotLookups<'_>,
) -> AssetTemplateNode {
    let local = renderer
        .transforms
        .get_local(key)
        .cloned()
        .unwrap_or(Transform::IDENTITY);
    let mesh_keys: Vec<MeshKey> = renderer
        .meshes
        .keys_by_transform_key(key)
        .cloned()
        .unwrap_or_default();
    let mesh_gltf_material_indices = mesh_keys
        .iter()
        .map(|mk| lk.mesh_mat.get(mk).copied().unwrap_or(None))
        .collect();
    // Captured BEFORE any editor reassignment (snapshot runs right after
    // `populate_gltf`): the populate material per primitive. Needed so a skinned
    // mesh's orphaned populate material is reclaimable on teardown.
    let mesh_material_keys = mesh_keys
        .iter()
        .map(|mk| {
            renderer
                .meshes
                .get(*mk)
                .map(|m| m.material_key)
                .unwrap_or_default()
        })
        .collect();
    let mesh_is_skinned = mesh_keys
        .iter()
        .map(|mk| renderer.meshes.mesh_is_skinned(*mk))
        .collect();
    let mesh_has_morphs = mesh_keys
        .iter()
        .map(|mk| {
            renderer.meshes.geometry_morph_key_for_mesh(*mk).is_some()
                || renderer.meshes.material_morph_key_for_mesh(*mk).is_some()
        })
        .collect();
    let children = renderer
        .transforms
        .get_children(key)
        .map(|kids| kids.iter().map(|c| snapshot(renderer, *c, lk)).collect())
        .unwrap_or_default();
    let gltf_node_index = lk.key_to_node_index.get(&key).copied().unwrap_or(0);
    AssetTemplateNode {
        gltf_node_index,
        baked_transform_key: key,
        is_skin_joint: lk.skin_joints.contains(&key),
        label: lk.key_to_label.get(&key).cloned(),
        local,
        mesh_keys,
        mesh_material_keys,
        mesh_gltf_material_indices,
        mesh_is_skinned,
        mesh_has_morphs,
        morph_target_names: lk
            .morph_names_by_node
            .get(&gltf_node_index)
            .cloned()
            .unwrap_or_default(),
        light: lk.lights_by_node.get(&gltf_node_index).cloned(),
        children,
    }
}

/// Hide every **non-skinned** mesh the template owns so the populate-baked copy
/// doesn't render as a ghost duplicate: non-skinned geometry is baked into
/// captured `Mesh` nodes at import (see `controller::state::build_editor_subtree`),
/// so the renderer's own populate copies are kept only to extract
/// materials/textures and are hidden. **Skinned** meshes are left **visible** and
/// rendering in place — they are the live skin the renderer deforms via the
/// skeleton joints (driven by the editor's mirror bones + imported animation
/// clips, see `skin_bridge`). The matching editor `NodeKind::SkinnedMesh` node
/// owns + re-materials that populate copy rather than baking it; duplicating or
/// hiding it would collapse the skin to bind pose (a flat blob).
pub fn hide_template_meshes(renderer: &mut AwsmRenderer, template: &AssetTemplate) {
    fn walk(renderer: &mut AwsmRenderer, nodes: &[AssetTemplateNode]) {
        for n in nodes {
            // Hide EVERY populate mesh — static AND skinned. The editor renders its
            // own copy of each: static geometry → a captured `Mesh` node; skinned →
            // a NODE-OWNED drawable the materialiser builds from the clean rig glb
            // (`node_sync::raw_mesh_from_rig`). The populate skinned copy is no longer
            // the rendered geometry, so leaving it visible would double-render. The
            // legacy template-reuse fallback (`materialize_skinned_from_template`,
            // morph-only / no-rig) un-hides the specific copy it renders.
            for mk in n.mesh_keys.iter() {
                let _ = renderer.set_mesh_hidden(*mk, true);
            }
            walk(renderer, &n.children);
        }
    }
    walk(renderer, &template.roots);
}

/// Remove the lights `populate_gltf` baked for this import. Each is bound to the
/// renderer's populate transform tree, which the editor does NOT animate — the
/// editor mirrors every KHR light as an editable `NodeKind::Light` bound to its
/// own node's transform instead (see [`AssetTemplateNode::light`]). Removing the
/// populate copies here prevents two lights per glTF light (one frozen, one
/// live). Called at import right after [`build_from_context`].
pub fn remove_template_lights(renderer: &mut AwsmRenderer, ctx: &GltfPopulateContext) {
    for key in &ctx.punctual_lights {
        renderer.remove_light(*key);
    }
}

/// Teardown counterpart to [`hide_template_meshes`]: remove EVERY renderer
/// resource this import's `populate_gltf` baked — skinned copies AND hidden
/// static copies alike — from the renderer: the GPU meshes, their materials
/// (which, via [`crate::AwsmRenderer::remove_material`], also reclaims the
/// pooled TEXTURES + texture-transforms when no other live material references
/// them), and the per-node baked transforms.
///
/// `clear_templates` only drops the template *metadata*; the populate copies are
/// template-owned, so `remove_node`/`teardown` deliberately leave them. Without
/// reclaiming them here on a project reset they linger as ghosts (a flat
/// bind-pose blob) AND their pooled textures/transforms leak across project
/// loads (a Chrome "aw snap" contributor). Freeing the material here is safe:
/// `remove_material`'s scan keeps any texture a still-live material references,
/// so it is safe whenever the template is definitively gone. Called on project
/// reset/clear AND mid-session when the last instance of an import is deleted
/// (`node_sync::remove_node`, gated by `Bridge::template_instances` refcount +
/// a live-`SkinnedMesh` guard so a template another node still renders from is
/// never freed).
pub fn remove_template_meshes(renderer: &mut AwsmRenderer, template: &AssetTemplate) {
    fn walk(renderer: &mut AwsmRenderer, nodes: &[AssetTemplateNode]) {
        for n in nodes {
            for (i, mk) in n.mesh_keys.iter().enumerate() {
                // Free the mesh's CURRENT material (static: the populate material;
                // skinned: a node-owned material already freed by teardown → a
                // stale-key no-op) AND the ORIGINAL populate material recorded at
                // snapshot. Skinned meshes reassign their material at materialize,
                // orphaning the populate one, so freeing the current key alone
                // leaks it (+ its pooled textures). `remove_material` is idempotent
                // (slotmap versioned keys), so freeing both — even when they
                // coincide (static) — is a safe double-free + dangle-free
                // (`remove_material`'s scan keeps any texture a live material
                // still references).
                let current = renderer.meshes.get(*mk).ok().map(|m| m.material_key);
                renderer.remove_mesh(*mk);
                if let Some(current) = current {
                    renderer.remove_material(current);
                }
                if let Some(orig) = n.mesh_material_keys.get(i).copied() {
                    renderer.remove_material(orig);
                }
            }
            // Reclaim the node's baked transform (template-owned; previously left
            // as an orphan even on reset).
            renderer.transforms.remove(n.baked_transform_key);
            walk(renderer, &n.children);
        }
    }
    walk(renderer, &template.roots);
}

/// Convert a renderer [`Transform`] into the schema [`Trs`].
pub fn transform_to_trs(t: &Transform) -> Trs {
    Trs {
        translation: t.translation.to_array(),
        rotation: t.rotation.to_array(),
        scale: t.scale.to_array(),
    }
}
