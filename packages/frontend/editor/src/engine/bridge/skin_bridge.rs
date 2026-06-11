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
//! animation pose is pinned and before world matrices are derived, copy each
//! mirror bone's **renderer local** onto its baked joint key. Reading the renderer
//! local (not the editor node's authored `Trs` signal) captures BOTH sources —
//! `animation_sync::pin_pose` writes the animated pose to the mirror's transform
//! key, and `node_sync` writes manual edits there too — so playback and posing
//! both deform the skin. The copy is guarded by an equality check, so an idle
//! skeleton dirties nothing.

use awsm_renderer::transforms::Transform;
use awsm_renderer::AwsmRenderer;

use super::bridge;

/// Copy every mapped mirror-bone local onto its baked joint local. Call under the
/// held renderer guard, AFTER `animation_sync::pin_pose` and BEFORE
/// `update_transforms` (so the dirtied baked joints feed the skin update).
pub fn sync_bones_to_skin(renderer: &mut AwsmRenderer) {
    let bridge = bridge();
    // Snapshot the map (NodeId → baked key) so we don't hold its lock while
    // touching the renderer / the nodes map.
    let pairs: Vec<(
        crate::engine::scene::NodeId,
        awsm_renderer::transforms::TransformKey,
    )> = {
        let map = bridge.skin_joint_baked.lock().unwrap();
        if map.is_empty() {
            return;
        }
        map.iter().map(|(n, k)| (*n, *k)).collect()
    };

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
        let Ok(src) = renderer.transforms.get_local(editor_key).cloned() else {
            continue;
        };
        // Skip the write when already equal so an un-animated skeleton stays clean
        // (no skin recompute churn).
        let unchanged = matches!(
            renderer.transforms.get_local(baked_key).cloned(),
            Ok(dst) if transforms_eq(&dst, &src)
        );
        if unchanged {
            continue;
        }
        let _ = renderer.transforms.set_local(baked_key, src);
        copied += 1;
    }
    if copied > 0 {
        // Breadcrumb for the pose-doesn't-deform investigations: proves the
        // mirror→baked copy actually ran this frame (rate-limited by nature —
        // only fires on change).
        tracing::info!("skin bridge: copied {copied} changed bone local(s) → baked joints");
    }
}

fn transforms_eq(a: &Transform, b: &Transform) -> bool {
    a.translation == b.translation && a.rotation == b.rotation && a.scale == b.scale
}
