//! Skin bridge (#2): connect the editor's animated mirror-bone transforms to
//! the renderer skin that actually deforms the mesh.
//!
//! A skinned glTF is imported by keeping its baked `populate_gltf` copy rendering
//! in place — the renderer's skin derives its joint matrices from *that* copy's
//! joint `TransformKey`s. The editor, however, mirrors every glTF node as its own
//! scene node with a **separate** transform, and animation + gizmo posing drive
//! those mirror transforms. Nothing connected the two, so the joint data animated
//! but the skin stayed frozen.
//!
//! This module bridges them the cheapest correct way: each frame, after the
//! animation pose is pinned and before world matrices are derived, sync each
//! mirror bone onto its baked joint key. Reading the renderer local/world (not the
//! editor node's authored `Trs` signal) captures BOTH sources —
//! `animation_sync::pin_pose` writes the animated pose to the mirror's transform
//! key, and `node_sync` writes manual edits there too — so playback and posing
//! both deform the skin. The copy is guarded by an equality check, so an idle
//! skeleton dirties nothing.
//!
//! **Placement-aware (whole-character moves):** the baked `populate_gltf` copy is
//! rooted at the renderer root (parent `None`), NOT under the editor's placement
//! node — so it never sees the transform of scene nodes ABOVE the skeleton (the
//! rig-root group / any ancestor the user moves or scales to place the character).
//! A plain per-bone *local* copy carries the pose but drops that ancestor offset,
//! so moving the rig-root moved the mirror bones (gizmos) while the skinned mesh
//! stayed at the copy's origin. To fix, a **skeleton-root** joint (one whose baked
//! parent is the static copy root, not another driven joint) is synced so its
//! baked *world* equals the mirror bone's *world* — which already folds in every
//! ancestor — via `baked_local = inverse(baked_parent_world) * mirror_world`.
//! Interior joints keep the plain local copy (both hierarchies share the same
//! relative bone pose, so their worlds then match too). This only affects the live
//! editor preview; export reads the mirror scene nodes, and the runtime loader
//! parents the rig under the placement node itself (`populate_gltf_under`), so
//! neither is touched.

use std::collections::HashSet;

use awsm_renderer::transforms::{Transform, TransformKey};
use awsm_renderer::AwsmRenderer;
use glam::Mat4;

use super::bridge;

/// Sync every mapped mirror bone onto its baked joint (placement-aware). Call
/// under the held renderer guard, AFTER `animation_sync::pin_pose` and BEFORE
/// `update_transforms` (so the dirtied baked joints feed the skin update).
pub fn sync_bones_to_skin(renderer: &mut AwsmRenderer) {
    let bridge = bridge();
    // Snapshot the map (NodeId → baked key) so we don't hold its lock while
    // touching the renderer / the nodes map.
    let pairs: Vec<(crate::engine::scene::NodeId, TransformKey)> = {
        let map = bridge.skin_joint_baked.lock().unwrap();
        if map.is_empty() {
            return;
        }
        map.iter().map(|(n, k)| (*n, *k)).collect()
    };

    // The set of driven baked joints. A baked joint whose parent is NOT in this
    // set is a skeleton ROOT: its baked parent is the static copy root, so the
    // placement/ancestor offset must be folded in here (see module docs).
    let baked_keys: HashSet<TransformKey> = pairs.iter().map(|(_, k)| *k).collect();

    let mut copied = 0usize;
    for (node_id, baked_key) in pairs {
        // Resolve the mirror bone's renderer transform key (materialized async by
        // node_sync; absent until then → skip this frame).
        let editor_key = {
            let nodes = bridge.nodes.lock().unwrap();
            match nodes.get(&node_id) {
                Some(n) => n.transform_key,
                None => continue,
            }
        };

        // Root joint (baked parent is the static copy root, not another driven
        // joint) → placement-aware; interior joint → plain local copy.
        let baked_parent = renderer.transforms.get_parent(baked_key).ok();
        let is_root_joint = baked_parent.is_none_or(|p| !baked_keys.contains(&p));

        let target = if is_root_joint {
            // Fold in the ancestor offset: choose the baked local so the baked
            // joint's WORLD matches the mirror bone's WORLD (which already
            // includes the rig-root group's TRS). The baked parent — the static
            // copy root — never moves, so its world is a stable reference.
            let Ok(mirror_world) = renderer.transforms.get_world(editor_key).copied() else {
                continue;
            };
            let baked_parent_world = baked_parent
                .and_then(|p| renderer.transforms.get_world(p).ok().copied())
                .unwrap_or(Mat4::IDENTITY);
            Transform::from(baked_parent_world.inverse() * mirror_world)
        } else {
            // Interior joint: the mirror local is already correct relative to its
            // (also-driven) parent — copy it verbatim.
            match renderer.transforms.get_local(editor_key).cloned() {
                Ok(src) => src,
                Err(_) => continue,
            }
        };

        // Skip the write when already equal so an un-animated, un-moved skeleton
        // stays clean (no skin recompute churn).
        let unchanged = matches!(
            renderer.transforms.get_local(baked_key).cloned(),
            Ok(dst) if transforms_eq(&dst, &target)
        );
        if unchanged {
            continue;
        }
        let _ = renderer.transforms.set_local(baked_key, target);
        copied += 1;
    }
    if copied > 0 {
        // Breadcrumb for the pose-doesn't-deform investigations: proves the
        // mirror→baked sync actually ran this frame (rate-limited by nature —
        // only fires on change).
        tracing::debug!("skin bridge: synced {copied} changed bone(s) → baked joints");
    }
}

fn transforms_eq(a: &Transform, b: &Transform) -> bool {
    a.translation == b.translation && a.rotation == b.rotation && a.scale == b.scale
}
