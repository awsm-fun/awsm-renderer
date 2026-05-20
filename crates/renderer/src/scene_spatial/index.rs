//! `SceneSpatial` — the renderer's single-source-of-truth BVH over mesh AABBs.

use glam::Vec3;
use rstar::RTree;
use slotmap::SecondaryMap;

use crate::{bounds::Aabb, frustum::Frustum, meshes::MeshKey};

use super::{
    frustum_selector::{frustum_intersects_node, FrustumSelector, Leaf},
    node::{aabb_to_rstar_envelope, aabb_to_rstar_rect, SceneNode, SceneNodeFlags},
    query::{NodeFilter, SpatialQuery},
};

/// Knobs that scale with mesh count. The renderer picks these once at
/// construction and they're read-only thereafter; tune in profiling.
#[derive(Debug, Clone, Copy)]
pub struct SceneSpatialConfig {
    /// Full-tree rebuild threshold (dirties accumulated since last rebuild).
    pub rebuild_dirty_threshold: u32,
    /// Periodic rebuild cadence in frames, regardless of dirty count.
    pub rebuild_period_frames: u32,
}

impl Default for SceneSpatialConfig {
    fn default() -> Self {
        // Sized for the 1k-mesh tier. Cluster 1.7 adapts these by total
        // static-node count once the dynamic-sidecar policy lands.
        Self {
            rebuild_dirty_threshold: 200,
            rebuild_period_frames: 600,
        }
    }
}

/// Spatial index over every mesh's world-space AABB.
///
/// The R*-tree holds the "static" set (everything that doesn't move every
/// frame). Meshes flagged `dynamic` live in `dynamic` instead — they are
/// linearly scanned per query, which is strictly cheaper than churning
/// the tree with their per-frame remove+reinsert cost. Either set can be
/// transitioned via [`SceneSpatial::set_dynamic`].
pub struct SceneSpatial {
    rtree: RTree<Leaf>,
    dynamic: Vec<SceneNode>,
    /// Mirror of every mesh's authoritative AABB + flags. Used to:
    ///   * look up the previous envelope for an in-place update (rstar
    ///     has no `update`, so we re-key by remove+insert),
    ///   * answer flag queries without dereferencing through `Meshes`,
    ///   * support `query_envelope` / `nearest` from the trait.
    nodes: SecondaryMap<MeshKey, SceneNode>,
    /// Tracks which `mesh_key` lives in the dynamic sidecar (vs the tree).
    /// Stored separately from `SceneNode::flags.dynamic` so we can rely on
    /// it during transitions without re-reading the node.
    in_dynamic: SecondaryMap<MeshKey, ()>,
    /// Dirty count since the last full rebuild. Drives the periodic
    /// `RTree::bulk_load` refresh.
    dirty_since_rebuild: u32,
    /// Frame counter for the periodic rebuild cadence. Bumped once per
    /// `rebuild_if_needed` call.
    frames_since_rebuild: u32,
    config: SceneSpatialConfig,
}

impl Default for SceneSpatial {
    fn default() -> Self {
        Self::new(SceneSpatialConfig::default())
    }
}

impl SceneSpatial {
    pub fn new(config: SceneSpatialConfig) -> Self {
        Self {
            rtree: RTree::new(),
            dynamic: Vec::new(),
            nodes: SecondaryMap::new(),
            in_dynamic: SecondaryMap::new(),
            dirty_since_rebuild: 0,
            frames_since_rebuild: 0,
            config,
        }
    }

    /// Total leaf count (static + dynamic). The debug invariant in
    /// `update.rs` asserts this matches the meshes-with-world-aabb count.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Inserts a node. If a node already exists for `mesh_key`, its prior
    /// envelope is removed first (the caller can just call `insert` to
    /// replace; `update` is identical for changed-AABB cases).
    pub fn insert(&mut self, node: SceneNode) {
        if self.nodes.contains_key(node.mesh_key) {
            self.remove(node.mesh_key);
        }
        let is_dynamic = node.flags.dynamic;
        if is_dynamic {
            self.dynamic.push(node.clone());
            self.in_dynamic.insert(node.mesh_key, ());
        } else {
            self.rtree
                .insert(Leaf::new(node.rstar_rect(), node.mesh_key));
        }
        self.nodes.insert(node.mesh_key, node);
        self.dirty_since_rebuild = self.dirty_since_rebuild.saturating_add(1);
    }

    /// Replaces the AABB of an existing node. Inserts the node if it
    /// doesn't yet exist (callers can pass the full `SceneNode` through
    /// `insert` instead — `update` is the lighter-weight path when only
    /// the AABB has changed).
    pub fn update(&mut self, mesh_key: MeshKey, aabb: Aabb) {
        let in_dynamic = self.in_dynamic.contains_key(mesh_key);
        if !self.nodes.contains_key(mesh_key) {
            return;
        }

        if in_dynamic {
            if let Some(slot) = self.dynamic.iter_mut().find(|n| n.mesh_key == mesh_key) {
                slot.aabb = aabb.clone();
            }
        } else {
            let prev_rect = self
                .nodes
                .get(mesh_key)
                .map(|n| n.rstar_rect())
                .expect("nodes.contains_key was just checked");
            // rstar has no in-place mutation; locate the leaf by exact
            // envelope + data, then reinsert with the new envelope.
            self.rtree.remove(&Leaf::new(prev_rect, mesh_key));
            self.rtree
                .insert(Leaf::new(aabb_to_rstar_rect(&aabb), mesh_key));
            self.dirty_since_rebuild = self.dirty_since_rebuild.saturating_add(1);
        }

        if let Some(node) = self.nodes.get_mut(mesh_key) {
            node.aabb = aabb;
        }
    }

    /// Removes a node. No-op if the key is unknown.
    pub fn remove(&mut self, mesh_key: MeshKey) {
        let Some(node) = self.nodes.remove(mesh_key) else {
            return;
        };
        if self.in_dynamic.remove(mesh_key).is_some() {
            self.dynamic.retain(|n| n.mesh_key != mesh_key);
        } else {
            self.rtree
                .remove(&Leaf::new(node.rstar_rect(), mesh_key));
            self.dirty_since_rebuild = self.dirty_since_rebuild.saturating_add(1);
        }
    }

    /// Sets flags on a live node without re-keying it. The `dynamic`
    /// flag is intentionally NOT honored here — moving between the tree
    /// and the sidecar happens through [`set_dynamic`] so the caller is
    /// explicit about the migration.
    pub fn set_flags(&mut self, mesh_key: MeshKey, flags: SceneNodeFlags) {
        if let Some(node) = self.nodes.get_mut(mesh_key) {
            let preserve_dynamic = node.flags.dynamic;
            node.flags = SceneNodeFlags {
                dynamic: preserve_dynamic,
                ..flags
            };
        }
        if let Some(slot) = self.dynamic.iter_mut().find(|n| n.mesh_key == mesh_key) {
            let preserve_dynamic = slot.flags.dynamic;
            slot.flags = SceneNodeFlags {
                dynamic: preserve_dynamic,
                ..flags
            };
        }
    }

    /// Moves a node between the static R*-tree and the dynamic sidecar.
    /// Cluster 1.7 calls this automatically for skinned/instanced meshes.
    pub fn set_dynamic(&mut self, mesh_key: MeshKey, dynamic: bool) {
        let Some(node) = self.nodes.get(mesh_key).cloned() else {
            return;
        };
        let already_dynamic = self.in_dynamic.contains_key(mesh_key);
        if already_dynamic == dynamic {
            if let Some(node_mut) = self.nodes.get_mut(mesh_key) {
                node_mut.flags.dynamic = dynamic;
            }
            return;
        }

        if dynamic {
            self.rtree
                .remove(&Leaf::new(node.rstar_rect(), mesh_key));
            let mut moved = node;
            moved.flags.dynamic = true;
            self.dynamic.push(moved.clone());
            self.in_dynamic.insert(mesh_key, ());
            if let Some(stored) = self.nodes.get_mut(mesh_key) {
                stored.flags.dynamic = true;
            }
        } else {
            self.dynamic.retain(|n| n.mesh_key != mesh_key);
            self.in_dynamic.remove(mesh_key);
            self.rtree
                .insert(Leaf::new(node.rstar_rect(), mesh_key));
            if let Some(stored) = self.nodes.get_mut(mesh_key) {
                stored.flags.dynamic = false;
            }
            self.dirty_since_rebuild = self.dirty_since_rebuild.saturating_add(1);
        }
    }

    /// Whether `mesh_key` is currently routed through the dynamic sidecar.
    pub fn is_dynamic(&self, mesh_key: MeshKey) -> bool {
        self.in_dynamic.contains_key(mesh_key)
    }

    /// Force a full `RTree::bulk_load` rebuild on the next
    /// [`rebuild_if_needed`] call. Called by the renderer when a large
    /// batch of inserts has just landed (asset stream-in, scene swap).
    pub fn mark_rebuild_needed(&mut self) {
        self.dirty_since_rebuild = u32::MAX;
    }

    /// Single per-frame maintenance entry point. Called once after
    /// `update_transforms` in `update_all`. Rebuilds the tree from
    /// scratch when dirty pressure crosses the configured threshold,
    /// which restores R*-tree query quality that successive remove+
    /// insert pairs have eroded.
    pub fn rebuild_if_needed(&mut self) {
        self.frames_since_rebuild = self.frames_since_rebuild.saturating_add(1);
        let cadence_due = self.frames_since_rebuild >= self.config.rebuild_period_frames;
        let dirty_due = self.dirty_since_rebuild >= self.config.rebuild_dirty_threshold;
        if !cadence_due && !dirty_due {
            return;
        }
        let leaves: Vec<Leaf> = self
            .nodes
            .iter()
            .filter(|(key, _)| !self.in_dynamic.contains_key(*key))
            .map(|(key, node)| Leaf::new(node.rstar_rect(), key))
            .collect();
        self.rtree = RTree::bulk_load(leaves);
        self.dirty_since_rebuild = 0;
        self.frames_since_rebuild = 0;
    }

    /// Iterate over every node currently in the dynamic sidecar.
    pub fn dynamic_iter(&self) -> impl Iterator<Item = &SceneNode> {
        self.dynamic.iter()
    }

    /// Iterate every node in the index (tree + sidecar). Order is
    /// unspecified.
    pub fn iter_all(&self) -> impl Iterator<Item = &SceneNode> {
        self.nodes.values()
    }

    /// Borrow the node for a mesh, if present.
    pub fn get(&self, mesh_key: MeshKey) -> Option<&SceneNode> {
        self.nodes.get(mesh_key)
    }

    // ── Internal queries used by both the trait and the borrowing API ──

    /// Frustum query returning borrowed `SceneNode` references.
    /// Reserved for the renderer's own call sites where the per-call
    /// `Vec` allocation matters. External crates use [`SpatialQuery`].
    pub fn query_frustum<'a>(
        &'a self,
        frustum: &'a Frustum,
        filter: NodeFilter,
    ) -> impl Iterator<Item = &'a SceneNode> + 'a {
        let frustum_copy = *frustum;
        let selector = FrustumSelector {
            frustum: frustum_copy,
        };
        let from_tree = self
            .rtree
            .locate_with_selection_function(selector)
            .filter_map(move |leaf| self.nodes.get(leaf.data))
            .filter(move |node| filter.matches(node));
        let from_dyn = self
            .dynamic
            .iter()
            .filter(move |node| frustum_intersects_node(&frustum_copy, node))
            .filter(move |node| filter.matches(node));
        from_tree.chain(from_dyn)
    }

    /// AABB-overlap query returning borrowed `SceneNode` references.
    pub fn query_envelope<'a>(&'a self, aabb: &Aabb) -> impl Iterator<Item = &'a SceneNode> + 'a {
        let envelope = aabb_to_rstar_envelope(aabb);
        let from_tree = self
            .rtree
            .locate_in_envelope_intersecting(&envelope)
            .filter_map(move |leaf| self.nodes.get(leaf.data));
        let aabb_min = aabb.min;
        let aabb_max = aabb.max;
        let from_dyn = self.dynamic.iter().filter(move |node| {
            aabb_overlap(aabb_min, aabb_max, node.aabb.min, node.aabb.max)
        });
        from_tree.chain(from_dyn)
    }

    /// Returns the node whose AABB-center is closest to `point`. Linear
    /// scan over the sidecar plus rstar's accelerated tree nearest.
    pub fn nearest_node(&self, point: Vec3) -> Option<&SceneNode> {
        let tree_candidate = self
            .rtree
            .nearest_neighbor(&point.to_array())
            .and_then(|leaf| self.nodes.get(leaf.data));

        let mut best = tree_candidate;
        let mut best_dist = best.map(|n| (n.aabb.center() - point).length_squared());

        for node in &self.dynamic {
            let d = (node.aabb.center() - point).length_squared();
            if best_dist.map(|cur| d < cur).unwrap_or(true) {
                best = Some(node);
                best_dist = Some(d);
            }
        }
        best
    }

    /// Frustum query restricted to the dynamic sidecar (linear scan).
    pub fn query_frustum_dynamic<'a>(
        &'a self,
        frustum: &'a Frustum,
        filter: NodeFilter,
    ) -> impl Iterator<Item = &'a SceneNode> + 'a {
        let frustum_copy = *frustum;
        self.dynamic
            .iter()
            .filter(move |node| frustum_intersects_node(&frustum_copy, node))
            .filter(move |node| filter.matches(node))
    }

    /// Iterate every node whose AABB intersects the frustum, ignoring
    /// any flag filter. Used by sites that want the raw geometric set.
    pub fn query_frustum_raw<'a>(
        &'a self,
        frustum: &'a Frustum,
    ) -> impl Iterator<Item = &'a SceneNode> + 'a {
        self.query_frustum(frustum, NodeFilter::default())
    }

    /// Iterate every node in the index that survives `filter`, regardless
    /// of frustum. Used by the directional-cascade fit pass where the
    /// "frustum" is the union of all cascade frusta.
    pub fn iter_filtered<'a>(
        &'a self,
        filter: NodeFilter,
    ) -> impl Iterator<Item = &'a SceneNode> + 'a {
        self.nodes.values().filter(move |node| filter.matches(node))
    }
}

impl SpatialQuery for SceneSpatial {
    fn query_frustum(&self, frustum: &Frustum, filter: NodeFilter) -> Vec<MeshKey> {
        SceneSpatial::query_frustum(self, frustum, filter)
            .map(|n| n.mesh_key)
            .collect()
    }

    fn query_envelope(&self, aabb: &Aabb) -> Vec<MeshKey> {
        SceneSpatial::query_envelope(self, aabb)
            .map(|n| n.mesh_key)
            .collect()
    }

    fn nearest(&self, point: Vec3) -> Option<MeshKey> {
        SceneSpatial::nearest_node(self, point).map(|n| n.mesh_key)
    }
}

fn aabb_overlap(a_min: Vec3, a_max: Vec3, b_min: Vec3, b_max: Vec3) -> bool {
    a_min.x <= b_max.x
        && a_max.x >= b_min.x
        && a_min.y <= b_max.y
        && a_max.y >= b_min.y
        && a_min.z <= b_max.z
        && a_max.z >= b_min.z
}

