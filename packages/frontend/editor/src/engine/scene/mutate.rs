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
/// the clone as a sibling immediately after the original. Returns the new id.
pub fn duplicate_by_id(scene: &Scene, id: NodeId, new_root_id: Option<NodeId>) -> Option<NodeId> {
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

    Some(new_id)
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
