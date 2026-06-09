//! Read-only vertex-selection highlight: observes the controller's transient
//! `vertex_selection` view-state and draws a small marker at each selected
//! vertex in the viewport. **Observability only** — no geometry mutation.
//!
//! Each selected vertex is drawn as a 3-axis "cross" marker (three short
//! world-space line segments through the vertex). All markers for the current
//! selection share a single fat-line draw, tracked by one `LineKey` so the
//! previous highlight is torn down before the next is built.
//!
//! First-cut limitation: the markers are rebuilt only when `vertex_selection`
//! changes. If the user later moves/transforms the highlighted node, the
//! markers go stale (baked at the world matrix observed at selection time)
//! until the selection is re-emitted. That's acceptable for a transient
//! "show me what the query matched" overlay.

use std::cell::Cell;

use awsm_renderer::render_passes::lines::LineKey;
use futures_signals::signal::SignalExt;
use glam::{Mat4, Vec3, Vec4};

use super::bridge;
use crate::controller::controller;
use crate::engine::context::with_renderer_mut;
use crate::engine::scene::NodeId;
use crate::prelude::*;

thread_local! {
    /// The line key of the markers currently drawn (if any). Replaced on each
    /// `vertex_selection` emission.
    static HIGHLIGHT: Cell<Option<LineKey>> = const { Cell::new(None) };
}

/// Bright, distinct amber so the markers read against typical geometry.
const HIGHLIGHT_COLOR: Vec4 = Vec4::new(1.0, 0.75, 0.2, 1.0);

/// Begin mirroring the controller's `vertex_selection` onto a viewport overlay.
pub fn start() {
    spawn_local(async move {
        controller()
            .vertex_selection
            .signal_cloned()
            .for_each(|sel| async move {
                rebuild(sel).await;
            })
            .await;
    });
}

/// Tear down the previous markers, then (if any vertices are selected) build a
/// fresh marker set for the new selection.
async fn rebuild(sel: Option<(NodeId, Vec<u32>)>) {
    // Collect the world-space marker geometry *outside* the renderer lock; the
    // mesh / world-matrix lookups don't need the renderer mutable borrow except
    // for the world matrix, which we fold in below.
    let mut positions: Vec<Vec3> = Vec::new();

    // Resolve the node's mesh + transform key up front (controller-side; no
    // renderer borrow). `None`/empty selection just clears.
    let resolved = sel.and_then(|(node, indices)| {
        if indices.is_empty() {
            return None;
        }
        let ctrl = controller();
        let bridge = bridge();
        let (tk, kind) = {
            let nodes = bridge.nodes.lock().unwrap();
            let entry = nodes.get(&node)?;
            // Take the transform key + the node's current kind.
            (entry.transform_key, entry.node.kind.get_cloned())
        };
        let mesh = crate::controller::export::node_mesh(&ctrl.scene, &kind)?;
        if mesh.positions.is_empty() {
            return None;
        }
        Some((tk, mesh, indices))
    });

    let line_key = with_renderer_mut(move |r| {
        // Always drop the prior highlight first.
        if let Some(prev) = HIGHLIGHT.with(|h| h.take()) {
            r.remove_line(prev);
        }

        let (tk, mesh, indices) = resolved?;

        let world = r
            .transforms
            .get_world(tk)
            .copied()
            .unwrap_or(Mat4::IDENTITY);

        // Marker half-length: a small fraction of the mesh's bbox diagonal so
        // it reads at any scale, clamped to a sane minimum.
        let s = marker_size(&mesh.positions);

        let mut colors: Vec<Vec4> = Vec::new();
        for &i in &indices {
            let Some(p_local) = mesh.positions.get(i as usize) else {
                continue; // index out of range — skip, never panic.
            };
            let p = world.transform_point3(Vec3::from_array(*p_local));
            // Three line *segments* (a 3-axis cross): X, Y, Z through `p`.
            for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                positions.push(p - axis * s);
                positions.push(p + axis * s);
                colors.push(HIGHLIGHT_COLOR);
                colors.push(HIGHLIGHT_COLOR);
            }
        }

        if positions.is_empty() {
            return None;
        }

        // `false` = respect depth (markers occlude behind geometry like the
        // collider wireframe overlay does).
        match r.add_line_segments(&positions, &colors, 1.5, false) {
            Ok(key) => key,
            Err(err) => {
                tracing::warn!("vertex_highlight: add_line_segments failed: {err}");
                None
            }
        }
    })
    .await;

    HIGHLIGHT.with(|h| h.set(line_key));
}

/// Marker half-length from the mesh's local-space bounding-box diagonal.
/// `0.02 * diag`, clamped to a small visible minimum.
fn marker_size(positions: &[[f32; 3]]) -> f32 {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for p in positions {
        let v = Vec3::from_array(*p);
        min = min.min(v);
        max = max.max(v);
    }
    let diag = (max - min).length();
    (0.02 * diag).max(0.01)
}
