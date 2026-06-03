//! Undo/redo stack of `SceneSnapshot`s. Snapshot-based (whole-scene) is
//! cheap enough for editor-sized scenes and avoids hand-writing inverse ops
//! for every mutator.

use crate::scene::SceneSnapshot;

const DEFAULT_MAX_DEPTH: usize = 64;

pub struct History {
    undo_stack: Vec<SceneSnapshot>,
    redo_stack: Vec<SceneSnapshot>,
    max_depth: usize,
}

impl History {
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }

    /// Record `previous` as a restorable state. Call *before* applying a
    /// mutation, passing the pre-mutation snapshot. Drops the redo stack.
    pub fn commit(&mut self, previous: SceneSnapshot) {
        if let Some(top) = self.undo_stack.last() {
            if top == &previous {
                // No-op mutation; don't pollute the stack.
                return;
            }
        }
        self.undo_stack.push(previous);
        if self.undo_stack.len() > self.max_depth {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    /// Pop one entry off the undo stack, pushing `current` onto the redo
    /// stack. Returns the snapshot to apply.
    pub fn undo(&mut self, current: SceneSnapshot) -> Option<SceneSnapshot> {
        let previous = self.undo_stack.pop()?;
        self.redo_stack.push(current);
        Some(previous)
    }

    /// Inverse of `undo`.
    pub fn redo(&mut self, current: SceneSnapshot) -> Option<SceneSnapshot> {
        let next = self.redo_stack.pop()?;
        self.undo_stack.push(current);
        Some(next)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }
}
