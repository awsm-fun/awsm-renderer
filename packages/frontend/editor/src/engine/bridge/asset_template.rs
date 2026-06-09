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
    /// One entry per `mesh_keys[i]`: the originating glTF material index
    /// (`None` ⇒ the primitive had no material, i.e. glTF's default). Used by
    /// the controller to assign each captured Mesh node its imported material and
    /// to decide whether to destructure a multi-material node per-primitive.
    pub mesh_gltf_material_indices: Vec<Option<usize>>,
    pub children: Vec<AssetTemplateNode>,
}

/// The whole node tree for one imported glTF/glb.
#[derive(Clone)]
pub struct AssetTemplate {
    pub roots: Vec<AssetTemplateNode>,
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
                &key_to_node_index,
                &key_to_label,
                &mesh_mat,
                &skin_joints,
            )
        })
        .collect();
    AssetTemplate { roots }
}

fn snapshot(
    renderer: &AwsmRenderer,
    key: TransformKey,
    key_to_node_index: &HashMap<TransformKey, u32>,
    key_to_label: &HashMap<TransformKey, String>,
    mesh_mat: &HashMap<MeshKey, Option<usize>>,
    skin_joints: &HashSet<TransformKey>,
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
        .map(|mk| mesh_mat.get(mk).copied().unwrap_or(None))
        .collect();
    let children = renderer
        .transforms
        .get_children(key)
        .map(|kids| {
            kids.iter()
                .map(|c| {
                    snapshot(
                        renderer,
                        *c,
                        key_to_node_index,
                        key_to_label,
                        mesh_mat,
                        skin_joints,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    AssetTemplateNode {
        gltf_node_index: key_to_node_index.get(&key).copied().unwrap_or(0),
        baked_transform_key: key,
        is_skin_joint: skin_joints.contains(&key),
        label: key_to_label.get(&key).cloned(),
        local,
        mesh_keys,
        mesh_gltf_material_indices,
        children,
    }
}

/// Hide **every** mesh the template owns so the populate-baked copies don't
/// render as ghost duplicates: geometry is now baked into captured Mesh nodes at
/// import (see `controller::state::build_editor_subtree`), so the renderer's own
/// populate meshes are kept only to extract materials/textures. This includes
/// skinned meshes — they now bake to their bind pose as static captured geometry
/// (JOINTS/WEIGHTS aren't read), so leaving the original rendering would
/// double-render. The template hierarchy + baked transform keys are still kept so
/// the skin bridge can drive bone transforms if reintroduced.
pub fn hide_template_meshes(renderer: &mut AwsmRenderer, template: &AssetTemplate) {
    fn walk(renderer: &mut AwsmRenderer, nodes: &[AssetTemplateNode]) {
        for n in nodes {
            for mk in n.mesh_keys.iter() {
                let _ = renderer.set_mesh_hidden(*mk, true);
            }
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
