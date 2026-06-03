//! Pointer-based drag/drop for tree rows. Drag state itself lives on
//! `AppState` (see `state.rs`); this module holds the drop-zone enum and
//! the drop application logic.

use crate::scene::{mutate, NodeId};
use crate::state::app_state;

/// Which part of a row the pointer is over. Drives where a drop will land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropZone {
    /// Drop as a sibling above the target node.
    Above,
    /// Drop as the last child of the target node.
    Inside,
    /// Drop as a sibling below the target node.
    Below,
}

/// Work out which drop zone the pointer is in, based on its vertical
/// position within a row of height `row_height`. Top 40% → Above,
/// bottom 40% → Below, middle 20% → Inside. Above/Below need to be
/// generous so users can easily drop as a sibling of the row (the
/// common case of "pull a child out of its parent" lives here).
pub fn zone_from_offset(offset_y: f64, row_height: f64) -> DropZone {
    let edge = row_height * 0.4;
    if offset_y < edge {
        DropZone::Above
    } else if offset_y > row_height - edge {
        DropZone::Below
    } else {
        DropZone::Inside
    }
}

/// Perform the drop for `state.drag_state.dragged` onto `(target_id, zone)`.
/// Commits one history snapshot.
pub fn apply_drop(target_id: NodeId, zone: DropZone, dragged: &[NodeId]) {
    if dragged.is_empty() {
        return;
    }
    let state = app_state();

    let previous = state.snapshot_scene();

    let (new_parent_id, position) = match zone {
        DropZone::Inside => (Some(target_id), None),
        DropZone::Above | DropZone::Below => {
            let parent = mutate::find_parent(&state.scene, target_id);
            let (index, siblings_len) = match &parent {
                Some(parent) => {
                    let children = parent.children.lock_ref();
                    let idx = children
                        .iter()
                        .position(|c| c.id == target_id)
                        .unwrap_or(children.len());
                    (idx, children.len())
                }
                None => {
                    let nodes = state.scene.nodes.lock_ref();
                    let idx = nodes
                        .iter()
                        .position(|n| n.id == target_id)
                        .unwrap_or(nodes.len());
                    (idx, nodes.len())
                }
            };
            let mut target_pos = match zone {
                DropZone::Above => index,
                DropZone::Below => index + 1,
                DropZone::Inside => unreachable!(),
            };
            target_pos = target_pos.min(siblings_len);
            (parent.map(|p| p.id), Some(target_pos))
        }
    };

    let moved = mutate::reparent_many(
        &state.scene,
        dragged.iter().copied(),
        new_parent_id,
        position,
    );
    if moved.is_empty() {
        return;
    }
    state.scene.bump_revision();
    state.commit_history(previous);
    tracing::info!(
        "tree::drop — moved {} node(s) under {:?} zone={:?}",
        moved.len(),
        new_parent_id,
        zone,
    );
}
