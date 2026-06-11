//! Skeleton bone-line overlay — draws every registered skin's bone hierarchy as
//! fat lines in the viewport (parent joint → child joint), live during posing
//! and animation playback.
//!
//! Data sources are the same registries the skin bridge runs on: the bridge's
//! `skin_joint_baked` map names which editor nodes are bones, the bridge node
//! entries give each bone's MIRROR transform key, and the renderer transform
//! hierarchy (mirrors are parented exactly like the scene tree) gives the
//! parent→child segments. Reading mirror worlds (not the baked rig's) keeps the
//! overlay attached to what posing/animation actually drive.
//!
//! One `LineKey` carries all segments; it's rebuilt each frame the overlay is
//! on (bone counts are tiny — tens of segments — so rebuild beats diffing).
//! `depth_test_always = true` so the skeleton reads through the skinned mesh,
//! like any rig overlay in a DCC. Honors the Settings → "Skeleton overlay"
//! toggle the same way light icons honor theirs.

use std::cell::Cell;

use awsm_renderer::render_passes::lines::LineKey;
use awsm_renderer::AwsmRenderer;
use glam::{Vec3, Vec4};

use crate::engine::bridge::bridge;

thread_local! {
    static SKELETON: Cell<Option<LineKey>> = const { Cell::new(None) };
}

/// Warm orange, distinct from the collider greens/blues and the amber vertex
/// markers; alpha < 1 so dense rigs don't shout.
const BONE_COLOR: Vec4 = Vec4::new(1.0, 0.55, 0.15, 0.9);
const BONE_WIDTH: f32 = 2.0;

/// Per-frame: rebuild the bone-line overlay from the live mirror-bone worlds.
/// Called from the render loop (before `update_transforms`, like the other
/// overlays — worlds are last frame's, one frame of lag is invisible here).
pub fn per_frame_update(renderer: &mut AwsmRenderer) {
    let enabled = crate::controller::controller().settings.skeleton_viz.get();

    // Collect (mirror transform key) per registered bone + a membership set so
    // a bone's parent segment is only drawn when the parent is ALSO a bone
    // (root joints get no segment — there's nothing meaningful to anchor to).
    let mut tks: Vec<awsm_renderer::transforms::TransformKey> = Vec::new();
    if enabled {
        let b = bridge();
        let joints = b.skin_joint_baked.lock().unwrap();
        if !joints.is_empty() {
            let nodes = b.nodes.lock().unwrap();
            tks = joints
                .keys()
                .filter_map(|id| nodes.get(id).map(|n| n.transform_key))
                .collect();
        }
    }

    let mut positions: Vec<Vec3> = Vec::new();
    let mut colors: Vec<Vec4> = Vec::new();
    if !tks.is_empty() {
        let bone_set: std::collections::HashSet<_> = tks.iter().copied().collect();
        for tk in &tks {
            let Ok(parent) = renderer.transforms.get_parent(*tk) else {
                continue;
            };
            if !bone_set.contains(&parent) {
                continue;
            }
            let (Ok(a), Ok(b)) = (
                renderer.transforms.get_world(parent),
                renderer.transforms.get_world(*tk),
            ) else {
                continue;
            };
            positions.push(a.w_axis.truncate());
            positions.push(b.w_axis.truncate());
            colors.push(BONE_COLOR);
            colors.push(BONE_COLOR);
        }
    }

    // Replace last frame's overlay wholesale (remove + add). Segment counts
    // are tiny; correctness-by-reconstruction over diff bookkeeping.
    if let Some(key) = SKELETON.with(|c| c.take()) {
        renderer.remove_line(key);
    }
    if !positions.is_empty() {
        match renderer.add_line_segments(&positions, &colors, BONE_WIDTH, true) {
            Ok(key) => SKELETON.with(|c| c.set(key)),
            Err(err) => tracing::warn!("skeleton_viz: add_line_segments failed: {err}"),
        }
    }
}
