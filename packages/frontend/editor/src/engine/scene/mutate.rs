//! Tree mutation operations. All inserts/removes/moves go through here so
//! that callers don't have to know about the top-level vs nested distinction.
//!
//! Each op returns enough information for the caller to commit a history
//! snapshot (via `state::history::commit`).

#![allow(dead_code)] // `reparent` and `is_ancestor_of` are planned for the tree-view drag flow.

use crate::engine::scene::{
    node::{Node, NodeId},
    Scene,
};
use std::sync::Arc;

/// Find a node by id, walking the whole tree.
pub fn find_by_id(scene: &Scene, id: NodeId) -> Option<Arc<Node>> {
    fn walk(nodes: &[Arc<Node>], id: NodeId) -> Option<Arc<Node>> {
        for node in nodes {
            if node.id == id {
                return Some(node.clone());
            }
            let children = node.children.lock_ref();
            if let Some(found) = walk(children.as_slice(), id) {
                return Some(found);
            }
        }
        None
    }
    let nodes = scene.nodes.lock_ref();
    walk(nodes.as_slice(), id)
}

/// Find the parent of the node with `id`. `None` means the node is at the
/// top level (or doesn't exist).
pub fn find_parent(scene: &Scene, id: NodeId) -> Option<Arc<Node>> {
    fn walk(nodes: &[Arc<Node>], id: NodeId) -> Option<Arc<Node>> {
        for node in nodes {
            let children = node.children.lock_ref();
            if children.iter().any(|child| child.id == id) {
                return Some(node.clone());
            }
            if let Some(found) = walk(children.as_slice(), id) {
                return Some(found);
            }
        }
        None
    }
    let nodes = scene.nodes.lock_ref();
    walk(nodes.as_slice(), id)
}

/// Insert a node under `parent_id`, or at the scene root if `parent_id` is
/// `None`. Returns `true` on success.
pub fn insert_under(scene: &Scene, parent_id: Option<NodeId>, node: Arc<Node>) -> bool {
    match parent_id {
        None => {
            scene.nodes.lock_mut().push_cloned(node);
            true
        }
        Some(id) => match find_by_id(scene, id) {
            Some(parent) => {
                parent.children.lock_mut().push_cloned(node);
                true
            }
            None => false,
        },
    }
}

/// Remove the node with `id` from wherever it lives in the tree.
/// Returns the removed node.
pub fn remove_by_id(scene: &Scene, id: NodeId) -> Option<Arc<Node>> {
    // Top-level first.
    {
        let mut nodes = scene.nodes.lock_mut();
        if let Some(position) = nodes.iter().position(|node| node.id == id) {
            return Some(nodes.remove(position));
        }
    }
    // Then walk into children.
    fn walk(nodes: &[Arc<Node>], id: NodeId) -> Option<Arc<Node>> {
        for node in nodes {
            {
                let mut children = node.children.lock_mut();
                if let Some(position) = children.iter().position(|child| child.id == id) {
                    return Some(children.remove(position));
                }
            }
            let children = node.children.lock_ref();
            if let Some(found) = walk(children.as_slice(), id) {
                return Some(found);
            }
        }
        None
    }
    let nodes = scene.nodes.lock_ref();
    walk(nodes.as_slice(), id)
}

/// Duplicate the node with `id`, giving the clone fresh UUIDs, and insert
/// the clone as a sibling immediately after the original. Returns the new id
/// plus the full `original → clone` [`NodeId`] map for the subtree, so the
/// caller can retarget node-referencing satellites (animation-clip tracks —
/// see the Duplicate handler in `controller::state`).
pub fn duplicate_by_id(
    scene: &Scene,
    id: NodeId,
    new_root_id: Option<NodeId>,
) -> Option<(NodeId, std::collections::HashMap<NodeId, NodeId>)> {
    let original = find_by_id(scene, id)?;
    let clone = match new_root_id {
        Some(rid) => original.deep_clone_with_root_id(rid),
        None => original.deep_clone_with_new_ids(),
    };
    // The clone copied every node's `kind` verbatim, so intra-subtree NodeId
    // references still point at the ORIGINAL's nodes — most importantly a
    // skinned mesh's `skin.joints[].node` bone bindings (leaving those stale
    // makes a duplicated rig deform to the original's skeleton, rendering
    // superimposed on it while its own joints do nothing). Remap every
    // reference that resolves inside the duplicated subtree onto the cloned
    // ids; references to nodes OUTSIDE the subtree (e.g. an instances node
    // whose curve lives elsewhere) are intentionally left pointing at the
    // shared original.
    let mut id_map = std::collections::HashMap::new();
    collect_clone_id_map(&original, &clone, &mut id_map);
    remap_cloned_node_refs(&clone, &id_map);
    // Propagate per-joint bind/rest records onto the cloned bone ids so
    // `ResetToBindPose` works on duplicated rigs too (rest is otherwise only
    // registered at import, keyed by the ORIGINAL bone ids).
    {
        let bridge = crate::engine::bridge::bridge();
        let mut rest = bridge.joint_rest.lock().unwrap();
        let copies: Vec<_> = id_map
            .iter()
            .filter_map(|(old, new)| rest.get(old).map(|r| (*new, *r)))
            .collect();
        rest.extend(copies);
    }
    let new_id = clone.id;

    // Insert as sibling after the original.
    let parent = find_parent(scene, id);
    match parent {
        Some(parent) => {
            let mut children = parent.children.lock_mut();
            if let Some(position) = children.iter().position(|child| child.id == id) {
                children.insert_cloned(position + 1, clone);
            } else {
                children.push_cloned(clone);
            }
        }
        None => {
            let mut nodes = scene.nodes.lock_mut();
            if let Some(position) = nodes.iter().position(|node| node.id == id) {
                nodes.insert_cloned(position + 1, clone);
            } else {
                nodes.push_cloned(clone);
            }
        }
    }

    Some((new_id, id_map))
}

/// Build the original→clone [`NodeId`] map by walking both trees in lockstep
/// (a deep clone mirrors the original's child order exactly).
fn collect_clone_id_map(
    original: &Arc<Node>,
    clone: &Arc<Node>,
    map: &mut std::collections::HashMap<NodeId, NodeId>,
) {
    map.insert(original.id, clone.id);
    let oc = original.children.lock_ref();
    let cc = clone.children.lock_ref();
    for (o, c) in oc.iter().zip(cc.iter()) {
        collect_clone_id_map(o, c, map);
    }
}

/// Rewrite intra-subtree [`NodeId`] references inside a freshly-cloned
/// subtree's kinds (skin joint bindings, instances-along-curve node refs) onto
/// the cloned ids. Ids absent from `map` reference nodes outside the
/// duplicated subtree and are preserved. Runs before the clone is inserted
/// into the scene, so no observer sees the intermediate state.
fn remap_cloned_node_refs(clone: &Arc<Node>, map: &std::collections::HashMap<NodeId, NodeId>) {
    use crate::engine::scene::NodeKind;
    let mut kind = clone.kind.get_cloned();
    let mut changed = false;
    match &mut kind {
        NodeKind::SkinnedMesh { skin, .. } => {
            for joint in skin.joints.iter_mut() {
                if let Some(new) = map.get(&joint.node) {
                    joint.node = *new;
                    changed = true;
                }
            }
        }
        NodeKind::InstancesAlongCurve(def) => {
            if let Some(new) = map.get(&def.curve_node) {
                def.curve_node = *new;
                changed = true;
            }
            if let Some(new) = map.get(&def.source_node) {
                def.source_node = *new;
                changed = true;
            }
        }
        _ => {}
    }
    if changed {
        clone.kind.set(kind);
    }
    for child in clone.children.lock_ref().iter() {
        remap_cloned_node_refs(child, map);
    }
}

/// Move `id` to become a child of `new_parent_id` at `position` (or `None`
/// for "append"). `new_parent_id` of `None` means top level. Returns `true`
/// on success. Refuses to move a node into its own descendants.
pub fn reparent(
    scene: &Scene,
    id: NodeId,
    new_parent_id: Option<NodeId>,
    position: Option<usize>,
) -> bool {
    // Guard against moving into a descendant.
    if let Some(new_parent_id) = new_parent_id {
        if id == new_parent_id {
            return false;
        }
        if is_ancestor_of(scene, id, new_parent_id) {
            return false;
        }
    }

    let Some(node) = remove_by_id(scene, id) else {
        return false;
    };

    // Tell the bridge this subtree is MOVING, not being deleted: the remove +
    // insert below reach it as two independent async diffs, and an unmarked
    // remove runs the full delete teardown — reclaiming the subtree's pooled
    // GPU textures + import template out from under the immediate re-add (the
    // re-materialize then cache-hits dead TextureKeys and every textured mesh
    // in the subtree renders untextured). Mark the WHOLE subtree: the bridge's
    // remove recurses per node and consumes one mark each.
    {
        fn collect(node: &Arc<Node>, out: &mut Vec<NodeId>) {
            out.push(node.id);
            for child in node.children.lock_ref().iter() {
                collect(child, out);
            }
        }
        let mut ids = Vec::new();
        collect(&node, &mut ids);
        crate::engine::bridge::bridge().mark_moving(ids);
    }

    match new_parent_id {
        None => {
            let mut nodes = scene.nodes.lock_mut();
            let idx = position.unwrap_or(nodes.len()).min(nodes.len());
            nodes.insert_cloned(idx, node);
        }
        Some(parent_id) => {
            let Some(parent) = find_by_id(scene, parent_id) else {
                // Parent vanished mid-move; drop on the floor to avoid a
                // silent corruption. Caller should refuse this up-front.
                return false;
            };
            let mut children = parent.children.lock_mut();
            let idx = position.unwrap_or(children.len()).min(children.len());
            children.insert_cloned(idx, node);
        }
    }

    true
}

/// Is `ancestor_id` an ancestor of `descendant_id` (or equal to it)?
pub fn is_ancestor_of(scene: &Scene, ancestor_id: NodeId, descendant_id: NodeId) -> bool {
    if ancestor_id == descendant_id {
        return true;
    }
    let Some(ancestor) = find_by_id(scene, ancestor_id) else {
        return false;
    };
    fn walk(nodes: &[Arc<Node>], id: NodeId) -> bool {
        for node in nodes {
            if node.id == id {
                return true;
            }
            let children = node.children.lock_ref();
            if walk(children.as_slice(), id) {
                return true;
            }
        }
        false
    }
    let children = ancestor.children.lock_ref();
    walk(children.as_slice(), descendant_id)
}

/// Filter `ids` so that if any id has an ancestor also in the set, it is
/// removed. Useful for bulk delete / duplicate / drag operations where the
/// descendant comes along for the ride.
pub fn ancestor_dedup<I: IntoIterator<Item = NodeId>>(scene: &Scene, ids: I) -> Vec<NodeId> {
    let all: Vec<NodeId> = ids.into_iter().collect();
    let mut kept = Vec::with_capacity(all.len());
    for &id in &all {
        let has_ancestor_in_set = all
            .iter()
            .any(|&other| other != id && is_ancestor_of(scene, other, id));
        if !has_ancestor_in_set {
            kept.push(id);
        }
    }
    kept
}

/// Move many nodes under a new parent at `position`, preserving their
/// relative order. Any node that would create a cycle is silently skipped.
/// Returns the ids that were actually moved.
pub fn reparent_many<I: IntoIterator<Item = NodeId>>(
    scene: &Scene,
    ids: I,
    new_parent_id: Option<NodeId>,
    position: Option<usize>,
) -> Vec<NodeId> {
    let deduped = ancestor_dedup(scene, ids);

    // Sort by current tree order so the group lands contiguously in the
    // same relative order.
    let order = flatten_tree_order(scene);
    let mut with_positions: Vec<(usize, NodeId)> = deduped
        .into_iter()
        .filter_map(|id| order.iter().position(|o| *o == id).map(|pos| (pos, id)))
        .collect();
    with_positions.sort_by_key(|(pos, _)| *pos);
    let ordered: Vec<NodeId> = with_positions.into_iter().map(|(_, id)| id).collect();

    let mut moved = Vec::new();
    let mut insert_at = position;
    for id in ordered {
        if reparent(scene, id, new_parent_id, insert_at) {
            moved.push(id);
            if let Some(pos) = insert_at.as_mut() {
                *pos += 1;
            }
        }
    }
    moved
}

/// Depth-first order of every node's id, used for range selection and for
/// the stable ordering when moving multiple nodes.
pub fn flatten_tree_order(scene: &Scene) -> Vec<NodeId> {
    fn walk(nodes: &[Arc<Node>], out: &mut Vec<NodeId>) {
        for node in nodes {
            out.push(node.id);
            let children = node.children.lock_ref();
            walk(children.as_slice(), out);
        }
    }
    let mut out = Vec::new();
    let nodes = scene.nodes.lock_ref();
    walk(nodes.as_slice(), &mut out);
    out
}

/// Depth-first order of every *visible* node id (skipping collapsed
/// subtrees). Used for Shift+click range selection and arrow-key navigation.
pub fn flatten_visible_order(scene: &Scene) -> Vec<NodeId> {
    fn walk(nodes: &[Arc<Node>], out: &mut Vec<NodeId>) {
        for node in nodes {
            out.push(node.id);
            if node.expanded.get() {
                let children = node.children.lock_ref();
                walk(children.as_slice(), out);
            }
        }
    }
    let mut out = Vec::new();
    let nodes = scene.nodes.lock_ref();
    walk(nodes.as_slice(), &mut out);
    out
}

#[cfg(test)]
mod duplicate_tests {
    use super::*;
    use crate::engine::scene::{NodeKind, Trs};
    use awsm_renderer_editor_protocol::{AssetId, SkinJoint, SkinnedMeshRef};

    fn group(name: &str) -> Arc<Node> {
        Node::new_with_transform_and_kind(name, Trs::IDENTITY, NodeKind::Group)
    }

    fn skinned(name: &str, joints: Vec<SkinJoint>) -> Arc<Node> {
        Node::new_with_transform_and_kind(
            name,
            Trs::IDENTITY,
            NodeKind::SkinnedMesh {
                skin: SkinnedMeshRef {
                    source: AssetId::new(),
                    node_index: 3,
                    rig_node_index: 3,
                    primitive_index: None,
                    joints,
                },
                material_variants: Vec::new(),
                selected_variant: None,
                shadow: Default::default(),
                lod: Default::default(),
            },
        )
    }

    /// Build [root → (bone_a → bone_b, skinned)] with the skinned node's
    /// joints binding bone_a, bone_b, and one node OUTSIDE the subtree.
    /// Returns (scene, root_id, bone ids, outside id).
    fn skinned_scene() -> (
        Arc<crate::engine::scene::Scene>,
        NodeId,
        [NodeId; 2],
        NodeId,
    ) {
        let scene = crate::engine::scene::Scene::new();
        let outside = group("outside");
        let outside_id = outside.id;
        let root = group("root");
        let root_id = root.id;
        let bone_a = group("bone_a");
        let bone_b = group("bone_b");
        let (a_id, b_id) = (bone_a.id, bone_b.id);
        bone_a.children.lock_mut().push_cloned(bone_b);
        let mesh = skinned(
            "mesh",
            vec![
                SkinJoint {
                    node: a_id,
                    index: 1,
                },
                SkinJoint {
                    node: b_id,
                    index: 2,
                },
                SkinJoint {
                    node: outside_id,
                    index: 7,
                },
            ],
        );
        root.children.lock_mut().push_cloned(bone_a);
        root.children.lock_mut().push_cloned(mesh);
        scene.nodes.lock_mut().push_cloned(outside);
        scene.nodes.lock_mut().push_cloned(root);
        (scene, root_id, [a_id, b_id], outside_id)
    }

    // The id map covers the WHOLE duplicated subtree (root + every
    // descendant), maps onto the clone's actual ids, and never maps a node
    // onto itself.
    #[test]
    fn id_map_covers_the_subtree_with_fresh_ids() {
        let (scene, root_id, [a_id, b_id], _) = skinned_scene();
        let (new_id, map) = duplicate_by_id(&scene, root_id, None).expect("duplicate");
        assert_eq!(map.len(), 4, "root + bone_a + bone_b + mesh");
        assert_eq!(map[&root_id], new_id);
        for old in [root_id, a_id, b_id] {
            let new = map[&old];
            assert_ne!(new, old, "clone must mint a fresh id");
            assert!(find_by_id(&scene, new).is_some(), "mapped id must exist");
        }
    }

    // Skin joint bindings inside the duplicated subtree are REMAPPED onto the
    // cloned bones; a joint referencing a node outside the subtree keeps its
    // original binding (shared, deliberately).
    #[test]
    fn skin_joints_remap_inside_the_subtree_only() {
        let (scene, root_id, [a_id, b_id], outside_id) = skinned_scene();
        let (new_id, map) = duplicate_by_id(&scene, root_id, None).expect("duplicate");
        let clone_root = find_by_id(&scene, new_id).unwrap();
        let clone_mesh = clone_root.children.lock_ref()[1].clone();
        let NodeKind::SkinnedMesh { skin, .. } = clone_mesh.kind.get_cloned() else {
            panic!("expected the cloned SkinnedMesh");
        };
        assert_eq!(skin.joints[0].node, map[&a_id]);
        assert_eq!(skin.joints[1].node, map[&b_id]);
        assert_eq!(skin.joints[2].node, outside_id, "outside ref preserved");
        // Rig-glb indices are identity, not node ids — untouched.
        assert_eq!(
            skin.joints.iter().map(|j| j.index).collect::<Vec<_>>(),
            vec![1, 2, 7]
        );
    }

    // The clone lands as the sibling immediately after the original.
    #[test]
    fn clone_is_inserted_after_the_original() {
        let (scene, root_id, _, _) = skinned_scene();
        let (new_id, _) = duplicate_by_id(&scene, root_id, None).expect("duplicate");
        let order: Vec<NodeId> = scene.nodes.lock_ref().iter().map(|n| n.id).collect();
        let root_pos = order.iter().position(|id| *id == root_id).unwrap();
        assert_eq!(order[root_pos + 1], new_id);
    }
}
