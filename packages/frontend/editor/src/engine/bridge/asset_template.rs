//! Per-import glTF **template**: a snapshot of the node hierarchy that
//! `populate_gltf` builds in the renderer.
//!
//! When a glTF/glb is imported, the renderer's `populate_gltf` walks the
//! document and creates a full transform tree + meshes (rig + skinning baked
//! in). The editor then *deconstructs* that into its own scene tree: every
//! glTF node becomes an editor [`Node`](crate::engine::scene::node::Node) —
//! a `Group` for pure transform/bone nodes, a `Model` for mesh-bearing nodes —
//! preserving each node's local transform.
//!
//! Each `Model` node materializes by **duplicating** the template's meshes
//! under its own (user-movable) transform via
//! `renderer.duplicate_mesh_with_transform`, which preserves the mesh's
//! skinning joint references (the joints still live in the renderer transform
//! tree). The template's own meshes are *hidden* (not removed) so they don't
//! double-render as ghosts and so the joints survive for skinning.

use std::collections::HashMap;

use awsm_renderer::meshes::MeshKey;
use awsm_renderer::transforms::{Transform, TransformKey};
use awsm_renderer::AwsmRenderer;
use awsm_renderer_gltf::populate::GltfPopulateContext;

use crate::engine::scene::Trs;

/// One glTF node, mirrored as an editor scene node.
#[derive(Clone)]
pub struct AssetTemplateNode {
    /// Original glTF node index — stored on the editor `Model` node's
    /// [`ModelRef`](awsm_scene_schema::ModelRef) so it can find these meshes.
    pub gltf_node_index: u32,
    /// glTF node name, if any.
    pub label: Option<String>,
    /// The node's local transform (as parsed from the glTF).
    pub local: Transform,
    /// Renderer mesh keys for this node's primitives (the template copies,
    /// hidden — Model nodes duplicate these under their own transform).
    pub mesh_keys: Vec<MeshKey>,
    /// One entry per `mesh_keys[i]`: the originating glTF material index
    /// (`None` ⇒ the primitive had no material, i.e. glTF's default). Used by
    /// the asset-extraction pass (#6.3) to swap in an editable material.
    #[allow(dead_code)] // consumed by the #6.3 material-extraction pass
    pub mesh_gltf_material_indices: Vec<Option<usize>>,
    /// One entry per `mesh_keys[i]`: whether that primitive is **skinned**.
    /// Skinned meshes are left rendering in place (the original copy) rather
    /// than duplicated under the editor node — duplicating/hiding them breaks
    /// the per-frame joint-matrix update and collapses the skin to bind pose.
    pub mesh_is_skinned: Vec<bool>,
    pub children: Vec<AssetTemplateNode>,
}

/// The whole node tree for one imported glTF/glb.
#[derive(Clone)]
pub struct AssetTemplate {
    pub roots: Vec<AssetTemplateNode>,
}

impl AssetTemplate {
    /// Depth-first lookup of a template node by its glTF node index.
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

    let root = renderer.transforms.root_node;
    let top_level: Vec<TransformKey> = all_keys
        .into_iter()
        .filter(|k| renderer.transforms.get_parent(*k).ok() == Some(root))
        .collect();

    let roots = top_level
        .into_iter()
        .map(|k| snapshot(renderer, k, &key_to_node_index, &key_to_label, &mesh_mat))
        .collect();
    AssetTemplate { roots }
}

fn snapshot(
    renderer: &AwsmRenderer,
    key: TransformKey,
    key_to_node_index: &HashMap<TransformKey, u32>,
    key_to_label: &HashMap<TransformKey, String>,
    mesh_mat: &HashMap<MeshKey, Option<usize>>,
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
    let mesh_is_skinned = mesh_keys
        .iter()
        .map(|mk| renderer.meshes.mesh_is_skinned(*mk))
        .collect();
    let children = renderer
        .transforms
        .get_children(key)
        .map(|kids| {
            kids.iter()
                .map(|c| snapshot(renderer, *c, key_to_node_index, key_to_label, mesh_mat))
                .collect()
        })
        .unwrap_or_default();
    AssetTemplateNode {
        gltf_node_index: key_to_node_index.get(&key).copied().unwrap_or(0),
        label: key_to_label.get(&key).cloned(),
        local,
        mesh_keys,
        mesh_gltf_material_indices,
        mesh_is_skinned,
        children,
    }
}

/// Hide every **non-skinned** mesh the template owns so the populate-baked copy
/// doesn't render as a ghost duplicate (the editor renders user-movable
/// duplicates of those instead). **Skinned** meshes are left visible and
/// rendering in place — duplicating/hiding them breaks the per-frame
/// joint-matrix update, collapsing the skin to bind pose (a flat blob). The
/// editor still mirrors their node hierarchy (bones as `Group`s); the mesh just
/// keeps rendering from the original copy.
pub fn hide_template_meshes(renderer: &mut AwsmRenderer, template: &AssetTemplate) {
    fn walk(renderer: &mut AwsmRenderer, nodes: &[AssetTemplateNode]) {
        for n in nodes {
            for (mk, &skinned) in n.mesh_keys.iter().zip(n.mesh_is_skinned.iter()) {
                if !skinned {
                    let _ = renderer.set_mesh_hidden(*mk, true);
                }
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
