//! Mesh→GPU re-sync: re-materialize captured-mesh geometry when an editable
//! mesh's bytes change without a node-kind change.
//!
//! `SetMeshData` (raw per-vertex editing / a collapsed modifier bake) overwrites
//! an `assets/<id>.mesh.bin` entry in the [`mesh_cache`](super::mesh_cache) store
//! but leaves every referencing `NodeKind::Mesh` node's *kind* unchanged — so the
//! per-node `node.kind` observer in `node_sync` never re-fires. This mirrors
//! `animation_sync`'s single-counter model: the controller bumps `mesh_revision`
//! for every [`affects_mesh`](crate::controller::EditorCommand::affects_mesh)
//! command, and the one observer here re-materializes the affected nodes.

use futures_signals::signal::SignalExt;

use crate::controller::controller;
use crate::prelude::*;

/// Begin re-materializing captured-mesh nodes on every `mesh_revision` bump.
/// Call once, after the renderer context is ready.
pub fn start() {
    spawn_local(async move {
        controller()
            .mesh_revision
            .signal()
            .for_each(|_| async {
                super::node_sync::rematerialize_mesh_nodes().await;
            })
            .await;
    });
}
