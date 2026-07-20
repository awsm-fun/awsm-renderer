//! Authored far-swap LOD registration ("geometry mipmap").
//!
//! Mirrors each mesh node's [`MeshLodConfig::far_swap`] into the renderer's
//! discrete-LOD registry: the base node's mesh is chain level 0, the far
//! node's mesh is level 1 with the authored object-space `error`. The
//! renderer's per-frame `update_lod_selection` (already running under the
//! `virtual_geometry` feature) then visibility-swaps between them by
//! projected screen error — the canonical cure for grazing-angle shimmer of
//! small relief detail (floor grooves, rails, wall tubes) that no screen-space
//! AA can reconstruct.
//!
//! Contract: a far node belongs to exactly ONE base (1:1) — chains sharing a
//! far mesh would fight over its hidden flag. The far node is an ordinary
//! scene node (save/load, materials); while registered it is force-hidden and
//! only drawn when its chain selects it.
//!
//! [`resync`] is idempotent and cheap (a map diff over a handful of chains);
//! call it after any batch that can change kinds, materialization, or loads.

use std::collections::HashMap;

use awsm_renderer::lod::{LodChain, LodLevel};
use awsm_renderer::meshes::MeshKey;

use crate::engine::bridge::state::bridge;
use crate::engine::context::renderer_handle;
use crate::engine::scene::NodeKind;

thread_local! {
    /// base MeshKey → far MeshKey currently registered by this module.
    static REGISTERED: std::cell::RefCell<HashMap<MeshKey, MeshKey>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Recompute the desired far-swap chain set from the scene and reconcile the
/// renderer's LOD registry to it.
pub async fn resync() {
    // Desired: (base_mk, far_mk, error) for every visible mesh node with a
    // far_swap whose target is also a live single-mesh node.
    let desired: Vec<(MeshKey, MeshKey, f32)> = {
        let b = bridge();
        let nodes = b.nodes.lock().unwrap();
        nodes
            .values()
            .filter_map(|entry| {
                let NodeKind::Mesh { lod, .. } = entry.node.kind.get_cloned() else {
                    return None;
                };
                let fs = lod.far_swap?;
                let base_mk = *entry.model_meshes.lock().unwrap().first()?;
                let far_entry = nodes.get(&fs.node)?;
                let far_mk = *far_entry.model_meshes.lock().unwrap().first()?;
                Some((base_mk, far_mk, fs.error.max(1e-4)))
            })
            .collect()
    };

    let handle = renderer_handle();
    let mut r = handle.lock().await;

    REGISTERED.with(|reg| {
        let mut reg = reg.borrow_mut();

        // Unregister chains whose base no longer wants a swap (or whose far
        // target changed — re-registered below).
        let desired_map: HashMap<MeshKey, (MeshKey, f32)> =
            desired.iter().map(|(b, f, e)| (*b, (*f, *e))).collect();
        let stale: Vec<(MeshKey, MeshKey)> = reg
            .iter()
            .filter(|(base, far)| desired_map.get(base).map(|(f, _)| f) != Some(far))
            .map(|(b, f)| (*b, *f))
            .collect();
        for (base, far) in stale {
            r.lod.unregister(base);
            // Restore both meshes to their scene visibility (the visibility
            // observer re-asserts effective state on the next toggle; here we
            // conservatively show the base and the far node's own eye state
            // is re-applied by node_sync's flows).
            let _ = r.set_mesh_hidden(base, false);
            let _ = r.set_mesh_hidden(far, false);
            reg.remove(&base);
        }

        // Register new/changed chains.
        for (base, far, error) in desired {
            if reg.get(&base) == Some(&far) {
                continue; // already registered; selection owns visibility now
            }
            r.lod.register(
                base,
                LodChain {
                    levels: vec![LodLevel {
                        mesh_key: far,
                        error,
                    }],
                    bounds_radius: 0.0,
                    current_level: 0,
                },
            );
            // Level starts hidden; base visible (chain state = level 0).
            let _ = r.set_mesh_hidden(far, true);
            let _ = r.set_mesh_hidden(base, false);
            reg.insert(base, far);
        }
    });
}
