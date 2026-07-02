//! `SceneSpatial` — the renderer's single source-of-truth spatial index
//! over mesh AABBs, backed by parry's dynamic `Bvh`.
//!
//! The tree handles static AND per-frame-moving meshes uniformly: leaf
//! updates go through `insert_or_update_partially` (a fattening margin
//! absorbs small motion entirely), and once per frame [`SceneSpatial::maintain`]
//! runs `refit` + `optimize_incremental` — the same broadphase workflow
//! rapier uses, designed for scenes where most objects move every frame.
//! This replaced an R*-tree (`rstar`) whose per-mover remove+reinsert cost
//! forced a linear-scan "dynamic sidecar" plus periodic full rebuilds; the
//! parry tree needs neither.
//!
//! **Ordering contract:** mutations (insert / update / remove) may leave
//! internal tree bounds stale until the next [`SceneSpatial::maintain`], which
//! `update_all` runs after the transform sync and before any query. Tree
//! queries `debug_assert` that contract; `iter_all` / `iter_filtered` /
//! `get` read the exact-AABB mirror and are always safe.

use parry3d::bounding_volume::BoundingVolume;
use parry3d::partitioning::{Bvh, BvhBuildStrategy, BvhWorkspace};
use slotmap::SecondaryMap;

use crate::{bounds::Aabb, frustum::Frustum, meshes::MeshKey};

use super::{
    node::{
        frustum_intersects, frustum_intersects_parry, to_parry_aabb, SceneNode, SceneNodeFlags,
    },
    query::NodeFilter,
};

/// Knobs picked once at construction (read-only thereafter).
#[derive(Debug, Clone, Copy)]
pub struct SceneSpatialConfig {
    /// World-space fattening margin (metres) applied to a leaf's AABB on
    /// update. Motion that stays inside the fattened box costs nothing
    /// (no tree surgery); larger margins mean fewer updates but looser
    /// tree bounds (queries always re-test candidates against the exact
    /// AABB, so this trades CPU for CPU, never correctness).
    pub change_detection_margin: f32,
}

impl Default for SceneSpatialConfig {
    fn default() -> Self {
        Self {
            change_detection_margin: 0.05,
        }
    }
}

/// Spatial index over every mesh's world-space AABB.
///
/// The parry `Bvh` stores margin-fattened leaf boxes keyed by a dense
/// `u32` slot; `nodes` mirrors every mesh's **exact** AABB + flags. Every
/// tree query re-tests candidates against the exact mirror before
/// yielding, so fattening never changes a query result.
pub struct SceneSpatial {
    bvh: Bvh,
    /// Scratch reused by `refit` / `optimize_incremental` (avoids
    /// per-frame allocations in the maintenance pass).
    workspace: BvhWorkspace,
    /// Exact AABB + flags per mesh — the authoritative mirror.
    nodes: SecondaryMap<MeshKey, SceneNode>,
    /// Mesh → BVH leaf slot.
    leaf_of: SecondaryMap<MeshKey, u32>,
    /// BVH leaf slot → mesh (`None` = freed slot awaiting reuse).
    key_of: Vec<Option<MeshKey>>,
    /// Freed leaf slots for reuse (keeps `key_of` dense-ish).
    free_leaves: Vec<u32>,
    /// Set by mutations whose tree-bound propagation is deferred;
    /// cleared by [`Self::maintain`]. Tree queries assert this is false.
    needs_maintain: bool,
    /// Set by [`Self::mark_rebuild_needed`]; the next [`Self::maintain`]
    /// does a fresh binned build instead of an incremental pass.
    full_rebuild: bool,
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
            bvh: Bvh::new(),
            workspace: BvhWorkspace::default(),
            nodes: SecondaryMap::new(),
            leaf_of: SecondaryMap::new(),
            key_of: Vec::new(),
            free_leaves: Vec::new(),
            needs_maintain: false,
            full_rebuild: false,
            config,
        }
    }

    /// Returns the config picked at construction.
    pub fn config(&self) -> SceneSpatialConfig {
        self.config
    }

    /// Total leaf count. The debug invariant in `update.rs` asserts this
    /// matches the meshes-with-world-aabb count.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Inserts a node, replacing any existing entry for the same mesh.
    pub fn insert(&mut self, node: SceneNode) {
        let mesh_key = node.mesh_key;
        if let Some(&leaf) = self.leaf_of.get(mesh_key) {
            // Existing mesh: in-place leaf update (same slot).
            self.bvh.insert_or_update_partially(
                to_parry_aabb(&node.aabb),
                leaf,
                self.config.change_detection_margin,
            );
            self.needs_maintain = true;
        } else {
            let leaf = self.alloc_leaf(mesh_key);
            // Fresh leaf: full insert keeps ancestor bounds correct
            // immediately (no deferred refit needed for pure inserts).
            self.bvh.insert(to_parry_aabb(&node.aabb), leaf);
        }
        self.nodes.insert(mesh_key, node);
    }

    /// Replaces the AABB of an existing node. No-op if the key is
    /// unknown (callers insert through [`Self::insert`]).
    pub fn update(&mut self, mesh_key: MeshKey, aabb: Aabb) {
        let Some(&leaf) = self.leaf_of.get(mesh_key) else {
            return;
        };
        self.bvh.insert_or_update_partially(
            to_parry_aabb(&aabb),
            leaf,
            self.config.change_detection_margin,
        );
        self.needs_maintain = true;
        if let Some(node) = self.nodes.get_mut(mesh_key) {
            node.aabb = aabb;
        }
    }

    /// Removes a node. No-op if the key is unknown.
    pub fn remove(&mut self, mesh_key: MeshKey) {
        if self.nodes.remove(mesh_key).is_none() {
            return;
        }
        let Some(leaf) = self.leaf_of.remove(mesh_key) else {
            return;
        };
        self.bvh.remove(leaf);
        self.key_of[leaf as usize] = None;
        self.free_leaves.push(leaf);
        // Removal never grows a stale bound (only loosens tree quality),
        // so queries stay correct; optimize on the next maintain pass.
        self.needs_maintain = true;
    }

    /// Sets flags on a live node. Flags live only in the exact mirror —
    /// the tree never needs touching for a flag flip.
    pub fn set_flags(&mut self, mesh_key: MeshKey, flags: SceneNodeFlags) {
        if let Some(node) = self.nodes.get_mut(mesh_key) {
            node.flags = flags;
        }
    }

    /// Force a fresh binned build on the next [`Self::maintain`] call.
    /// Called when a large batch of inserts has just landed (asset
    /// stream-in, scene swap) — one good build beats many incremental
    /// optimization passes.
    pub fn mark_rebuild_needed(&mut self) {
        self.full_rebuild = true;
    }

    /// Single per-frame maintenance entry point. Called once after
    /// `update_transforms` in `update_all`, BEFORE any tree query runs.
    /// Propagates deferred leaf updates (`refit`) and incrementally
    /// rebalances (`optimize_incremental`); a pending
    /// [`Self::mark_rebuild_needed`] does a fresh binned build instead.
    /// Idle scenes (no mutations since the last call) pay nothing.
    pub fn maintain(&mut self) {
        if self.full_rebuild {
            self.full_rebuild = false;
            self.needs_maintain = false;
            self.bvh = Bvh::from_iter(
                BvhBuildStrategy::Binned,
                self.nodes.iter().map(|(key, node)| {
                    let leaf = self.leaf_of[key] as usize;
                    (leaf, to_parry_aabb(&node.aabb))
                }),
            );
            return;
        }
        if self.needs_maintain {
            self.needs_maintain = false;
            self.bvh.refit(&mut self.workspace);
            self.bvh.optimize_incremental(&mut self.workspace);
        }
    }

    /// Iterate every node in the index. Order is unspecified. Reads the
    /// exact mirror — safe regardless of maintenance state.
    pub fn iter_all(&self) -> impl Iterator<Item = &SceneNode> {
        self.nodes.values()
    }

    /// Borrow the node for a mesh, if present.
    pub fn get(&self, mesh_key: MeshKey) -> Option<&SceneNode> {
        self.nodes.get(mesh_key)
    }

    /// Iterate every node that survives `filter`, regardless of frustum.
    /// Used by the directional-cascade fit pass where the "frustum" is
    /// the union of all cascade frusta. Reads the exact mirror.
    pub fn iter_filtered<'a>(
        &'a self,
        filter: NodeFilter,
    ) -> impl Iterator<Item = &'a SceneNode> + 'a {
        self.nodes.values().filter(move |node| filter.matches(node))
    }

    /// Frustum query returning borrowed `SceneNode` references. The tree
    /// prunes on fattened bounds; every candidate is re-tested against
    /// its exact AABB, so the surviving set is identical to a linear scan.
    pub fn query_frustum<'a>(
        &'a self,
        frustum: &Frustum,
        filter: NodeFilter,
    ) -> impl Iterator<Item = &'a SceneNode> + 'a {
        debug_assert!(
            !self.needs_maintain,
            "scene_spatial tree query before maintain() — update_all must run \
             maintenance between mutations and queries"
        );
        let frustum = *frustum;
        self.bvh
            .leaves(move |node| frustum_intersects_parry(&frustum, &node.aabb()))
            .filter_map(move |leaf| self.node_for_leaf(leaf))
            .filter(move |node| {
                frustum_intersects(&frustum, node.aabb.min, node.aabb.max) && filter.matches(node)
            })
    }

    /// Frustum query without a flag filter.
    pub fn query_frustum_raw<'a>(
        &'a self,
        frustum: &'a Frustum,
    ) -> impl Iterator<Item = &'a SceneNode> + 'a {
        self.query_frustum(frustum, NodeFilter::default())
    }

    /// AABB-overlap query returning borrowed `SceneNode` references.
    /// Fattened-tree candidates, exact-tested before yielding.
    pub fn query_envelope<'a>(&'a self, aabb: &Aabb) -> impl Iterator<Item = &'a SceneNode> + 'a {
        debug_assert!(
            !self.needs_maintain,
            "scene_spatial tree query before maintain() — update_all must run \
             maintenance between mutations and queries"
        );
        let query = to_parry_aabb(aabb);
        let min = aabb.min;
        let max = aabb.max;
        self.bvh
            // Inlined `intersect_aabb` with an owned query (the upstream
            // helper borrows the query for the iterator's lifetime).
            .leaves(move |node| node.aabb().intersects(&query))
            .filter_map(move |leaf| self.node_for_leaf(leaf))
            .filter(move |node| {
                min.x <= node.aabb.max.x
                    && max.x >= node.aabb.min.x
                    && min.y <= node.aabb.max.y
                    && max.y >= node.aabb.min.y
                    && min.z <= node.aabb.max.z
                    && max.z >= node.aabb.min.z
            })
    }

    fn alloc_leaf(&mut self, mesh_key: MeshKey) -> u32 {
        let leaf = match self.free_leaves.pop() {
            Some(leaf) => {
                self.key_of[leaf as usize] = Some(mesh_key);
                leaf
            }
            None => {
                let leaf = self.key_of.len() as u32;
                self.key_of.push(Some(mesh_key));
                leaf
            }
        };
        self.leaf_of.insert(mesh_key, leaf);
        leaf
    }

    fn node_for_leaf(&self, leaf: u32) -> Option<&SceneNode> {
        self.key_of
            .get(leaf as usize)
            .copied()
            .flatten()
            .and_then(|key| self.nodes.get(key))
    }
}
