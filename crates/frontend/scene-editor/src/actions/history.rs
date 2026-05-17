//! Undo / Redo.
//!
//! The undo stack stores whole-scene `SceneSnapshot`s, captured *before*
//! each mutation. Undo pops one off, swaps it for the current scene's
//! snapshot (so redo can return to the post-mutation state), and applies
//! the popped snapshot to the live scene. Selection is cleared because
//! NodeIds in the popped snapshot may no longer point at live nodes.

use crate::state::app_state;

pub fn undo() {
    let state = app_state();
    let current = state.snapshot_scene();
    let previous = match state.history.lock().unwrap().undo(current) {
        Some(snap) => snap,
        None => return,
    };
    crate::scene::snapshot::apply_to(&previous, &state.scene);
    state.scene.bump_revision();
    state.clear_selection();
    state.refresh_history_signals();
    state.mark_dirty();
    tracing::info!("action: history::undo — done");
}

pub fn redo() {
    let state = app_state();
    let current = state.snapshot_scene();
    let next = match state.history.lock().unwrap().redo(current) {
        Some(snap) => snap,
        None => return,
    };
    crate::scene::snapshot::apply_to(&next, &state.scene);
    state.scene.bump_revision();
    state.clear_selection();
    state.refresh_history_signals();
    state.mark_dirty();
    tracing::info!("action: history::redo — done");
}
