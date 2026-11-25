//! TAA "new object" flags (`TaaTuning::reactive_new_objects`, `docs/TAA.md`
//! §3.6): which meshes are in their first rendered frame.
//!
//! A mesh is marked when it is inserted or shown again after being hidden.
//! It waits until it is drawable (a live add is not drawn before its
//! `commit_load`, a mesh whose pipeline is not ready is skipped), then gets
//! bit 2 of material-meta word 22 for that one frame (the reactive seed then
//! writes the weight on its pixels); the meshes flagged the frame before get
//! it cleared. For [`MAX_CARRY_FRAMES`] frames after that, a mesh that jumps
//! (its bounds move by more than their radius in one frame) is flagged again
//! for the frame of the jump: an app that shows an object a frame or two
//! before it poses it at its new place (Dreidel Derby's respawned coins)
//! spent the flag at the old place. A marked mesh that is hidden again (or gone) is dropped: being
//! shown marks it again. Pure bookkeeping: the caller says which marked
//! meshes are drawable and patches the meta words.

use crate::meshes::MeshKey;

/// Bit 2 of material-meta word 22 (the TAA flags word): the mesh is in its
/// first rendered frame.
pub const TAA_FLAG_NEW_OBJECT: u32 = 1 << 2;

/// Frames after its first flagged frame during which a mesh that jumps
/// (moves by more than its bounds' radius in one frame) is flagged again.
pub const MAX_CARRY_FRAMES: u8 = 6;

/// Where a mesh's world bounds are this frame: centre and radius.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub center: glam::Vec3,
    pub radius: f32,
}

#[derive(Clone, Copy, Debug)]
struct Watched {
    /// Frames since it was first flagged (0 on that frame).
    age: u8,
    /// Its bounds last frame, `None` without bounds.
    bounds: Option<Bounds>,
    /// The meta flag is set this frame.
    flagged: bool,
}

/// What a marked mesh is at a frame start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewObjectState {
    /// It draws this frame: flag it now.
    Drawable,
    /// Not drawable yet (not committed, pipeline compiling): keep it marked.
    Wait,
    /// Hidden again, or gone: forget it.
    Drop,
}

/// The meshes waiting for their first frame, and the ones flagged now.
/// Sets: a teardown removing thousands of meshes while thousands are marked
/// stays O(n).
#[derive(Debug, Default)]
pub struct NewObjects {
    pending: std::collections::HashSet<MeshKey>,
    /// Flagged now, or watched for a jump after their first flagged frame.
    watched: std::collections::HashMap<MeshKey, Watched>,
}

/// What a frame start changes: the meta words to clear and to set (sorted).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct NewObjectsStep {
    pub clear: Vec<MeshKey>,
    pub set: Vec<MeshKey>,
}

impl NewObjects {
    /// A mesh became visible (inserted, or un-hidden).
    pub fn mark(&mut self, key: MeshKey) {
        self.pending.insert(key);
    }

    /// The mesh is gone: forget it. O(1).
    pub fn remove(&mut self, key: MeshKey) {
        self.pending.remove(&key);
        self.watched.remove(&key);
    }

    /// Whether the mesh carries the flag this frame.
    pub fn is_live(&self, key: MeshKey) -> bool {
        self.watched.get(&key).is_some_and(|w| w.flagged)
    }

    /// Whether any mesh carries the flag this frame.
    pub fn any_live(&self) -> bool {
        self.watched.values().any(|w| w.flagged)
    }

    /// The marked and the flagged meshes (the caller gives their bounds).
    pub fn candidates(&self) -> impl Iterator<Item = MeshKey> + '_ {
        self.pending.iter().chain(self.watched.keys()).copied()
    }

    /// The marked meshes not yet flagged (the caller classifies them).
    pub fn pending(&self) -> impl Iterator<Item = MeshKey> + '_ {
        self.pending.iter().copied()
    }

    /// Frame start. `enabled`: TAA is on with a weight above 0 (otherwise
    /// nothing is kept). `reset`: this frame's resolve discards all history
    /// anyway, so a mesh drawable now needs no flag (a scene load, a cut);
    /// one still waiting stays marked. `state` classifies each marked mesh;
    /// `bounds` gives a mesh's world bounds this frame. The meshes flagged
    /// last frame are cleared unless flagged again or jumped (see the module
    /// docs). `set` lists the meshes whose flag is newly set this frame (a
    /// carried one keeps its flag without a patch).
    pub fn advance(
        &mut self,
        enabled: bool,
        reset: bool,
        state: impl Fn(MeshKey) -> NewObjectState,
        bounds: impl Fn(MeshKey) -> Option<Bounds>,
    ) -> NewObjectsStep {
        let marked = std::mem::take(&mut self.pending);
        let mut set = Vec::new();
        let mut clear = Vec::new();
        let mut fresh = std::collections::HashSet::new();
        if enabled {
            for key in marked {
                match state(key) {
                    NewObjectState::Drawable if !reset => {
                        fresh.insert(key);
                        set.push(key);
                        self.watched.insert(
                            key,
                            Watched {
                                age: 0,
                                bounds: bounds(key),
                                flagged: true,
                            },
                        );
                    }
                    NewObjectState::Drawable | NewObjectState::Drop => {}
                    NewObjectState::Wait => {
                        self.pending.insert(key);
                    }
                }
            }
        }
        let mut forget = Vec::new();
        for (key, w) in self.watched.iter_mut() {
            if fresh.contains(key) {
                continue;
            }
            let now = bounds(*key);
            let jumped = match (w.bounds, now) {
                (Some(a), Some(b)) => a.center.distance(b.center) > a.radius.max(b.radius),
                _ => false,
            };
            let age = w.age.saturating_add(1);
            let flag_now = enabled && !reset && jumped && age <= MAX_CARRY_FRAMES;
            if w.flagged && !flag_now {
                clear.push(*key);
            } else if !w.flagged && flag_now {
                set.push(*key);
            }
            *w = Watched {
                age,
                bounds: now,
                flagged: flag_now,
            };
            if !flag_now && (age >= MAX_CARRY_FRAMES || !enabled) {
                forget.push(*key);
            }
        }
        for key in forget {
            self.watched.remove(&key);
        }
        set.sort();
        clear.sort();
        NewObjectsStep { clear, set }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slotmap::SlotMap;

    fn keys(n: usize) -> Vec<MeshKey> {
        let mut sm: SlotMap<MeshKey, ()> = SlotMap::with_key();
        (0..n).map(|_| sm.insert(())).collect()
    }

    #[test]
    fn a_new_mesh_is_flagged_for_exactly_one_frame() {
        let k = keys(1);
        let mut n = NewObjects::default();
        n.mark(k[0]);
        assert!(!n.any_live(), "nothing flagged before the frame starts");
        let s = n.advance(true, false, |_| NewObjectState::Drawable, |_| None);
        assert_eq!(s.set, vec![k[0]]);
        assert!(s.clear.is_empty());
        assert!(n.is_live(k[0]));
        let s = n.advance(true, false, |_| NewObjectState::Drawable, |_| None);
        assert_eq!(s.clear, vec![k[0]]);
        assert!(s.set.is_empty());
        assert!(!n.any_live());
        assert_eq!(
            n.advance(true, false, |_| NewObjectState::Drawable, |_| None),
            NewObjectsStep::default()
        );
    }

    #[test]
    fn off_or_reset_flags_nothing_but_still_clears() {
        let k = keys(2);
        let mut n = NewObjects::default();
        n.mark(k[0]);
        n.advance(true, false, |_| NewObjectState::Drawable, |_| None);
        n.mark(k[1]);
        let s = n.advance(false, false, |_| NewObjectState::Drawable, |_| None);
        assert_eq!(s.clear, vec![k[0]]);
        assert!(s.set.is_empty());
        n.mark(k[1]);
        let s = n.advance(true, true, |_| NewObjectState::Drawable, |_| None);
        assert!(s.set.is_empty() && s.clear.is_empty());
        assert!(!n.any_live());
    }

    #[test]
    fn marked_twice_and_marked_while_live_keep_the_flag_one_more_frame() {
        let k = keys(1);
        let mut n = NewObjects::default();
        n.mark(k[0]);
        n.mark(k[0]);
        assert_eq!(
            n.advance(true, false, |_| NewObjectState::Drawable, |_| None)
                .set,
            vec![k[0]]
        );
        // Hidden and shown again during its first frame: flagged again, not
        // cleared.
        n.mark(k[0]);
        let s = n.advance(true, false, |_| NewObjectState::Drawable, |_| None);
        assert_eq!(s.set, vec![k[0]]);
        assert!(s.clear.is_empty());
        assert!(n.is_live(k[0]));
    }

    #[test]
    fn a_mesh_waits_until_it_is_drawable_and_a_hidden_one_is_dropped() {
        let k = keys(2);
        let mut n = NewObjects::default();
        n.mark(k[0]);
        n.mark(k[1]);
        // k0 not drawable yet (a live add before its commit), k1 hidden again.
        let s = n.advance(
            true,
            false,
            |key| {
                if key == k[0] {
                    NewObjectState::Wait
                } else {
                    NewObjectState::Drop
                }
            },
            |_| None,
        );
        assert!(s.set.is_empty());
        assert_eq!(n.pending().collect::<Vec<_>>(), vec![k[0]]);
        // A reset while it still waits keeps it waiting.
        let s = n.advance(true, true, |_| NewObjectState::Wait, |_| None);
        assert!(s.set.is_empty());
        assert_eq!(n.pending().collect::<Vec<_>>(), vec![k[0]]);
        // Drawable at last: flagged that frame.
        let s = n.advance(true, false, |_| NewObjectState::Drawable, |_| None);
        assert_eq!(s.set, vec![k[0]]);
        assert_eq!(n.pending().count(), 0);
        // A reset on the frame it becomes drawable: no flag needed.
        n.mark(k[1]);
        let s = n.advance(true, true, |_| NewObjectState::Drawable, |_| None);
        assert!(s.set.is_empty());
        assert_eq!(s.clear, vec![k[0]]);
    }

    #[test]
    fn a_mesh_that_jumps_right_after_its_first_frame_keeps_the_flag() {
        let k = keys(1);
        let at = |x: f32| {
            Some(Bounds {
                center: glam::Vec3::new(x, 0.0, 0.0),
                radius: 0.5,
            })
        };
        let mut n = NewObjects::default();
        n.mark(k[0]);
        // Shown at the old place (the flag is spent there)...
        assert_eq!(
            n.advance(true, false, |_| NewObjectState::Drawable, |_| at(0.0))
                .set,
            vec![k[0]]
        );
        // ...stays put a frame (cleared, still watched)...
        let s = n.advance(true, false, |_| NewObjectState::Drawable, |_| at(0.0));
        assert_eq!(s.clear, vec![k[0]]);
        assert!(!n.is_live(k[0]));
        // ...then is posed 10 m away: flagged again for that frame.
        let s = n.advance(true, false, |_| NewObjectState::Drawable, |_| at(10.0));
        assert_eq!(s.set, vec![k[0]]);
        assert!(n.is_live(k[0]));
        // Moving on normally (less than its radius per frame): cleared, and
        // once the window is over no longer watched.
        let s = n.advance(true, false, |_| NewObjectState::Drawable, |_| at(10.2));
        assert_eq!(s.clear, vec![k[0]]);
        for _ in 0..MAX_CARRY_FRAMES {
            n.advance(true, false, |_| NewObjectState::Drawable, |_| at(10.2));
        }
        assert_eq!(n.candidates().count(), 0);
        // A jump after the window is not flagged.
        n.mark(k[0]);
        n.advance(true, false, |_| NewObjectState::Drawable, |_| at(0.0));
        for _ in 0..MAX_CARRY_FRAMES {
            n.advance(true, false, |_| NewObjectState::Drawable, |_| at(0.0));
        }
        let s = n.advance(true, false, |_| NewObjectState::Drawable, |_| at(10.0));
        assert!(s.set.is_empty());
    }

    #[test]
    fn a_removed_mesh_is_forgotten() {
        let k = keys(2);
        let mut n = NewObjects::default();
        n.mark(k[0]);
        n.mark(k[1]);
        n.advance(true, false, |_| NewObjectState::Drawable, |_| None);
        n.remove(k[0]);
        let s = n.advance(true, false, |_| NewObjectState::Drawable, |_| None);
        assert_eq!(s.clear, vec![k[1]]);
    }
}
