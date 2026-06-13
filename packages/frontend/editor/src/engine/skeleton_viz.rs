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
//! One `LineKey` carries all segments; it's updated in place each frame the
//! overlay is on (`update_line_segments` reuses the GPU buffer — see the day-3
//! churn fix). The CPU-side gather buffers (`tks` / `bone_set` / `positions` /
//! `colors`) are likewise REUSED across frames via a thread-local `Scratch`
//! (cleared, not reallocated) so an enabled overlay allocates nothing at steady
//! state. `depth_test_always = true` so the skeleton reads through the skinned
//! mesh, like any rig overlay in a DCC. Honors the Settings → "Skeleton overlay"
//! toggle the same way light icons honor theirs.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::hash::Hash;

use awsm_renderer::render_passes::lines::LineKey;
use awsm_renderer::transforms::TransformKey;
use awsm_renderer::AwsmRenderer;
use glam::{Vec3, Vec4};

use crate::engine::bridge::bridge;

thread_local! {
    static SKELETON: Cell<Option<LineKey>> = const { Cell::new(None) };
    /// Per-frame gather buffers, reused across frames (cleared, capacity
    /// retained) so an enabled overlay does zero heap allocation at steady
    /// state.
    static SCRATCH: RefCell<Scratch> = RefCell::new(Scratch::new());
}

/// Reused per-frame working set for the overlay rebuild.
struct Scratch {
    tks: Vec<TransformKey>,
    bone_set: HashSet<TransformKey>,
    positions: Vec<Vec3>,
    colors: Vec<Vec4>,
}

impl Scratch {
    fn new() -> Self {
        Self {
            tks: Vec::new(),
            bone_set: HashSet::new(),
            positions: Vec::new(),
            colors: Vec::new(),
        }
    }
    fn clear(&mut self) {
        self.tks.clear();
        self.bone_set.clear();
        self.positions.clear();
        self.colors.clear();
    }
}

/// Saturated deep orange at full alpha. (HDR values > 1 do NOT work here —
/// the fat-line target clamps per channel, so [6, 2.2, 0.4] turned pale
/// yellow-white; saturation, not luminance, is what survives.)
const BONE_COLOR: Vec4 = Vec4::new(0.95, 0.30, 0.05, 1.0);
/// Root-tether segments (chain depth ≤ 1 — e.g. the floor-origin root joint up
/// to the pelvis) render dimmed: real root-motion information, but it shouldn't
/// shout over the anatomical skeleton.
const BONE_COLOR_ROOT: Vec4 = Vec4::new(0.95, 0.30, 0.05, 0.35);
const BONE_WIDTH: f32 = 3.0;

/// Chain depth of `tk` = how many bone ancestors it has, walking parents while
/// they remain in `bone_set` (0 = chain root). Generic over the key + a
/// parent-lookup so it's pure and unit-testable without a GPU `Transforms`.
/// Capped at 64 to defend against a cyclic/degenerate hierarchy.
fn chain_depth<K, F>(bone_set: &HashSet<K>, mut tk: K, parent_of: F) -> u32
where
    K: Copy + Eq + Hash,
    F: Fn(K) -> Option<K>,
{
    let mut d = 0u32;
    while let Some(p) = parent_of(tk) {
        if !bone_set.contains(&p) || d > 64 {
            break;
        }
        d += 1;
        tk = p;
    }
    d
}

/// Per-frame: rebuild the bone-line overlay from the live mirror-bone worlds.
/// Called from the render loop (before `update_transforms`, like the other
/// overlays — worlds are last frame's, one frame of lag is invisible here).
pub fn per_frame_update(renderer: &mut AwsmRenderer) {
    let enabled = crate::controller::controller().settings.skeleton_viz.get();

    SCRATCH.with(|scratch| {
        let scratch = &mut *scratch.borrow_mut();
        scratch.clear();

        // Collect (mirror transform key) per registered bone.
        if enabled {
            let b = bridge();
            let joints = b.skin_joint_baked.lock().unwrap();
            if !joints.is_empty() {
                let nodes = b.nodes.lock().unwrap();
                scratch.tks.extend(
                    joints
                        .keys()
                        .filter_map(|id| nodes.get(id).map(|n| n.transform_key)),
                );
            }
        }

        if !scratch.tks.is_empty() {
            // Disjoint field borrows: `bone_set`/`tks` read while
            // `positions`/`colors` are written.
            let Scratch {
                tks,
                bone_set,
                positions,
                colors,
            } = scratch;
            bone_set.extend(tks.iter().copied());
            // A bone's parent segment is only drawn when the parent is ALSO a
            // bone (root joints get no segment — nothing meaningful to anchor
            // to). Segments whose PARENT depth ≤ 1 are the root tether — dimmed.
            for tk in tks.iter() {
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
                let parent_depth =
                    chain_depth(bone_set, parent, |k| renderer.transforms.get_parent(k).ok());
                let color = if parent_depth <= 1 {
                    BONE_COLOR_ROOT
                } else {
                    BONE_COLOR
                };
                positions.push(a.w_axis.truncate());
                positions.push(b.w_axis.truncate());
                colors.push(color);
                colors.push(color);
            }
        }

        // Update the ONE persistent overlay line in place (reuses the GPU
        // segment buffer; reallocates only when the bone count grows). The
        // previous remove+add-every-frame churned a fresh GPU buffer + uniform
        // buffer + bind group + uploaders 60×/sec (the day-3 tab-OOM suspect).
        let key = SKELETON.with(|c| c.get());
        if scratch.positions.is_empty() {
            if let Some(key) = key {
                renderer.remove_line(key);
                SKELETON.with(|c| c.set(None));
            }
            return;
        }
        match key {
            // `has_line` guards the entry having been torn down behind us (e.g.
            // a renderer rebuild) — fall through to a fresh add when it's gone.
            Some(key) if renderer.has_line(key) => {
                if let Err(err) =
                    renderer.update_line_segments(key, &scratch.positions, &scratch.colors)
                {
                    tracing::warn!("skeleton_viz: update_line_segments failed: {err}");
                }
            }
            _ => match renderer.add_line_segments(
                &scratch.positions,
                &scratch.colors,
                BONE_WIDTH,
                true,
            ) {
                Ok(key) => SKELETON.with(|c| c.set(key)),
                Err(err) => tracing::warn!("skeleton_viz: add_line_segments failed: {err}"),
            },
        }
    });
}

#[cfg(test)]
mod tests {
    use super::chain_depth;
    use std::collections::{HashMap, HashSet};

    fn parent_map(pairs: &[(u32, u32)]) -> HashMap<u32, u32> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn chain_depth_counts_bone_ancestors() {
        // Chain 1→2→3→4 (4 is the root); all are bones.
        let parents = parent_map(&[(1, 2), (2, 3), (3, 4)]);
        let bones: HashSet<u32> = [1, 2, 3, 4].into_iter().collect();
        let pof = |k: u32| parents.get(&k).copied();
        assert_eq!(chain_depth(&bones, 4, pof), 0, "root has depth 0");
        assert_eq!(chain_depth(&bones, 3, pof), 1);
        assert_eq!(chain_depth(&bones, 2, pof), 2);
        assert_eq!(chain_depth(&bones, 1, pof), 3, "leaf has full depth");
    }

    #[test]
    fn chain_depth_stops_at_non_bone_parent() {
        // 1→2→99 but 99 is NOT a bone → walk stops at 2.
        let parents = parent_map(&[(1, 2), (2, 99)]);
        let bones: HashSet<u32> = [1, 2].into_iter().collect();
        let pof = |k: u32| parents.get(&k).copied();
        assert_eq!(
            chain_depth(&bones, 1, pof),
            1,
            "depth stops when the parent leaves the bone set"
        );
    }

    #[test]
    fn chain_depth_caps_on_cycle() {
        // 1→2→1 cycle — must not loop forever; caps out.
        let parents = parent_map(&[(1, 2), (2, 1)]);
        let bones: HashSet<u32> = [1, 2].into_iter().collect();
        let pof = |k: u32| parents.get(&k).copied();
        let d = chain_depth(&bones, 1, pof);
        assert!(d <= 66, "cyclic hierarchy must cap, got {d}");
    }
}
