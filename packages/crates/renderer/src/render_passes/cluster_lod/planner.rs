//! Gap-B dynamic-paging planner (20b-iv-b-2c): the pure per-frame residency
//! decision engine.
//!
//! [`plan`] takes the frame's desired antichain (the camera-adaptive CPU cut)
//! plus the page-pool residency state and emits an ordered op list
//! ([`PagingOp`]) — `Load{cluster, slot}` / `Evict{slot}` — that
//! `stream_paging` then applies through the existing GPU-write paths. The
//! planner has no GPU/IO dependencies (deterministic, unit-tested natively);
//! it applies the CPU bookkeeping (`resident` / `slot_cluster` /
//! `slot_last_used`) itself as it plans, so every rule's justification is
//! checked against the LIVE intra-frame state (no two rules can evict each
//! other's cover).
//!
//! **The crack-free invariant.** Every occupied slot is always-drawn (its GPU
//! page is clamped), so the drawn set == the occupied slots. The planner
//! guarantees the occupied clusters remain a surface COVER at every frame
//! boundary (a frame's ops land atomically before the next draw): an `Evict`
//! is only ever emitted when the evictee's region is provably covered by other
//! residents, and a coarsen's write-over loads land in the same frame batch as
//! the child evicts they justify.
//!
//! **The rules**, in per-frame order:
//! - **a. Keep-warm** — desired ∧ resident slots get their LRU stamp.
//! - **c-up (child retirement)** — a resident, non-desired cluster whose
//!   group's parents are ALL resident is covered by those (coarser) parents ⇒
//!   evict. (Runs first so when both directions are redundant we keep the
//!   coarser side — fewer slots.) A parentless group (the bake simplified it
//!   to nothing) never vacuously justifies this.
//! - **c-down (parent retirement)** — a resident, non-desired cluster that is
//!   a PARENT of a group whose children are ALL resident is tiled exactly by
//!   those children ⇒ evict. This is what un-wedges recycling per-region while
//!   the global cut is still streaming (no global gate).
//! - **b. Refine** — desired ∧ non-resident clusters load into FREE slots
//!   (stream-into-free ⇒ coverage only grows), capped per frame.
//! - **c-global** — when the WHOLE desired cut is resident, any remaining
//!   non-desired resident is redundant (desired alone is a complete antichain
//!   cover) ⇒ evict. This is the legacy 20b-iv-b-2b sweep, kept so zoom-out
//!   still drops residency to exactly the antichain.
//! - **d. Coarsen-cold-groups under pressure** — when the pool is exhausted
//!   and desired clusters remain missing, pick groups whose children are ALL
//!   resident, cold (LRU stamp ≤ frame − [`PlannerCaps::cold_frames`]) and
//!   non-desired; load each missing parent INTO one of the children's slots
//!   (write-over) and evict the remaining child slots. Coldest groups first;
//!   capped per frame. Net slots freed = |children| − |missing parents|, and
//!   at the frame boundary the group's region is exactly covered by its (now
//!   fully resident) parents.
//!
//! Zero steady-state heap allocation: all scratch lives in [`PlannerScratch`]
//! (pooled by the caller), and [`GroupGraph`] is built once at paging init.

use crate::cluster_lod::ClusterPage;

/// The bake's root sentinel (`lod-bake` `dag.rs` `ROOT_PARENT_ERROR`): a root
/// cluster (never simplified further) carries `parent_error == f32::MAX`. The
/// value rides through the JSON bundle bit-exactly (serde_json round-trips f32),
/// so `>=` here identifies exactly the bake's roots.
pub const ROOT_PARENT_ERROR: f32 = f32::MAX;

/// Frames a slot must go unused (not in any desired cut) before rule d may
/// treat it as cold and coarsen its group away.
pub const COLD_FRAMES: u64 = 30;

/// One planned paging operation, in application order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagingOp {
    /// Stream `cluster`'s geometry into `slot` (write the exploded verts, the
    /// clamped always-draw GPU page, the slot-aligned source indices, and the
    /// slot's residency entry). The slot may be OCCUPIED (rule d write-over):
    /// the load replaces the previous occupant.
    Load { cluster: u32, slot: u32 },
    /// Free `slot` (GPU residency entry → `-1`; the cut skips it).
    Evict { slot: u32 },
}

/// Per-frame op caps (hitch control) + the coldness horizon.
#[derive(Clone, Copy, Debug)]
pub struct PlannerCaps {
    /// Max rule-b refine loads per frame (a camera jump must not hitch).
    pub max_loads: usize,
    /// Max rule-c evicts per frame.
    pub max_evicts: usize,
    /// Max rule-d group coarsens per frame (each is ≤ group-size ops).
    pub max_coarsens: usize,
    /// Rule-d cold horizon (frames since a slot was last in the desired cut).
    pub cold_frames: u64,
}

impl Default for PlannerCaps {
    fn default() -> Self {
        Self {
            max_loads: 96,
            max_evicts: 96,
            max_coarsens: 8,
            cold_frames: COLD_FRAMES,
        }
    }
}

/// What [`plan`] did this frame (drives the paging log line).
#[derive(Clone, Copy, Debug, Default)]
pub struct PlanStats {
    /// Total `Load` ops (rule-b refines + rule-d parent write-overs).
    pub streamed: usize,
    /// Total `Evict` ops (rules c + the tail of rule d).
    pub evicted: usize,
    /// Rule-d groups coarsened.
    pub coarsened: usize,
    /// Every desired cluster is resident after this frame's loads.
    pub all_desired_resident: bool,
}

/// Diagnostic counts from [`GroupGraph::build`] (logged once at paging init).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GroupGraphStats {
    pub groups: usize,
    /// Clusters with the root sentinel `parent_error` (never simplified away).
    pub roots: usize,
    /// Finest clusters (parents of no group).
    pub leaves: usize,
    /// Groups whose simplification produced no clusters (the bake simplified
    /// them to nothing) — they can never be coarsened into (rules c-up/d skip
    /// them). Rare but real (a shipped bake has 2 of 702).
    pub parentless_groups: usize,
    /// Non-leaf clusters (`lod_error > 0`) whose lod key matched no group —
    /// should be 0 (the bake writes group spheres by copy, so keys are
    /// bit-exact); anything else means the keying assumption broke.
    pub unmatched_nonleaf: usize,
}

/// The DAG's simplification-group graph, reconstructed from the cluster pages
/// (no bake-format change): the bake guarantees all clusters simplified
/// together share an IDENTICAL group sphere+error, written by copy — so exact
/// f32-bit keys on `(bounds_center, bounds_radius, error)` recover the groups.
/// A group's `children` carry the key as their `parent_*`; its `parents` (the
/// coarser clusters its simplification produced) carry it as their `lod_*`.
/// Built once at paging init.
pub struct GroupGraph {
    /// `group_children[g]` = the clusters group `g` simplifies away. Never empty.
    pub group_children: Vec<Vec<u32>>,
    /// `group_parents[g]` = the coarser clusters produced by simplifying `g`
    /// (they tile exactly the children's region). Empty only for a
    /// simplified-to-nothing group.
    pub group_parents: Vec<Vec<u32>>,
    /// `child_group[c]` = the group that simplifies cluster `c` away, or `-1`
    /// for a root.
    pub child_group: Vec<i32>,
    /// `parent_group[c]` = the group cluster `c` is a parent OF (whose
    /// simplification created `c`), or `-1` for a leaf.
    pub parent_group: Vec<i32>,
    stats: GroupGraphStats,
}

/// Exact-bit group key: `(sphere center, sphere radius, error)`.
type GroupKey = [u32; 5];

#[inline]
fn group_key(center: [f32; 3], radius: f32, error: f32) -> GroupKey {
    [
        center[0].to_bits(),
        center[1].to_bits(),
        center[2].to_bits(),
        radius.to_bits(),
        error.to_bits(),
    ]
}

impl GroupGraph {
    /// Reconstruct the group graph from the FULL un-clamped pages (init-time;
    /// allocation is fine here). See the type docs for the keying argument.
    pub fn build(pages: &[ClusterPage]) -> Self {
        use std::collections::HashMap;
        let mut ids: HashMap<GroupKey, u32> = HashMap::new();
        let mut group_children: Vec<Vec<u32>> = Vec::new();
        let mut child_group = vec![-1i32; pages.len()];
        let mut roots = 0usize;
        for (c, p) in pages.iter().enumerate() {
            if p.parent_error >= ROOT_PARENT_ERROR {
                roots += 1;
                continue;
            }
            let key = group_key(
                p.parent_bounds_center,
                p.parent_bounds_radius,
                p.parent_error,
            );
            let gid = *ids.entry(key).or_insert_with(|| {
                group_children.push(Vec::new());
                (group_children.len() - 1) as u32
            });
            group_children[gid as usize].push(c as u32);
            child_group[c] = gid as i32;
        }
        let mut group_parents: Vec<Vec<u32>> = vec![Vec::new(); group_children.len()];
        let mut parent_group = vec![-1i32; pages.len()];
        let mut leaves = 0usize;
        let mut unmatched_nonleaf = 0usize;
        for (c, p) in pages.iter().enumerate() {
            // A leaf's lod key is (own bounds, 0.0) — group errors are always
            // > 0 (the bake adds ε), so a leaf key can never match a group.
            let key = group_key(p.lod_bounds_center, p.lod_bounds_radius, p.lod_error);
            if let Some(&gid) = ids.get(&key) {
                group_parents[gid as usize].push(c as u32);
                parent_group[c] = gid as i32;
            } else if p.lod_error > 0.0 {
                unmatched_nonleaf += 1;
            } else {
                leaves += 1;
            }
        }
        let stats = GroupGraphStats {
            groups: group_children.len(),
            roots,
            leaves,
            parentless_groups: group_parents.iter().filter(|p| p.is_empty()).count(),
            unmatched_nonleaf,
        };
        Self {
            group_children,
            group_parents,
            child_group,
            parent_group,
            stats,
        }
    }

    pub fn group_count(&self) -> usize {
        self.group_children.len()
    }

    pub fn stats(&self) -> GroupGraphStats {
        self.stats
    }
}

/// Pooled planner scratch (held by `ClusterPaging`; zero per-frame allocation
/// once warm).
pub struct PlannerScratch {
    /// `desired_flag[cluster]` — membership test for this frame's desired cut.
    /// All-false between frames (set + cleared from `desired`, never a full
    /// clear).
    desired_flag: Vec<bool>,
    /// This frame's desired ∧ non-resident clusters, in desired order.
    missing: Vec<u32>,
    /// Rule-d victim candidates: `(newest child LRU stamp, group)` — sorted so
    /// the coldest group coarsens first.
    victims: Vec<(u64, u32)>,
    /// One group's child slots during a coarsen.
    victim_slots: Vec<u32>,
}

impl PlannerScratch {
    pub fn new(cluster_count: usize) -> Self {
        Self {
            desired_flag: vec![false; cluster_count],
            missing: Vec::new(),
            victims: Vec::new(),
            victim_slots: Vec::new(),
        }
    }
}

#[inline]
fn all_resident(clusters: &[u32], resident: &[i32]) -> bool {
    clusters.iter().all(|&c| resident[c as usize] >= 0)
}

/// Apply a (possibly write-over) load's CPU bookkeeping + emit the op.
#[inline]
fn do_load(
    ops: &mut Vec<PagingOp>,
    resident: &mut [i32],
    slot_cluster: &mut [i32],
    slot_last_used: &mut [u64],
    cluster: u32,
    slot: usize,
    frame: u64,
) {
    let old = slot_cluster[slot];
    if old >= 0 {
        resident[old as usize] = -1;
    }
    resident[cluster as usize] = slot as i32;
    slot_cluster[slot] = cluster as i32;
    slot_last_used[slot] = frame;
    ops.push(PagingOp::Load {
        cluster,
        slot: slot as u32,
    });
}

/// Apply an evict's CPU bookkeeping + emit the op.
#[inline]
fn do_evict(ops: &mut Vec<PagingOp>, resident: &mut [i32], slot_cluster: &mut [i32], slot: usize) {
    let c = slot_cluster[slot];
    debug_assert!(c >= 0, "evicting an already-free slot");
    if c >= 0 {
        resident[c as usize] = -1;
    }
    slot_cluster[slot] = -1;
    ops.push(PagingOp::Evict { slot: slot as u32 });
}

/// Plan one frame of paging. Mutates the CPU residency bookkeeping in place
/// (so every rule checks the live intra-frame state) and fills `ops` (cleared
/// first) with the GPU operations, in application order. Pure CPU —
/// deterministic in its inputs, no allocation once `scratch`/`ops` are warm.
///
/// `desired` must be the frame's complete antichain cut (`select_cut_per_cluster`
/// over the full DAG) — rule c-global's safety leans on desired alone being a
/// surface cover.
#[allow(clippy::too_many_arguments)]
pub fn plan(
    desired: &[u32],
    frame: u64,
    graph: &GroupGraph,
    resident: &mut [i32],
    slot_cluster: &mut [i32],
    slot_last_used: &mut [u64],
    caps: &PlannerCaps,
    scratch: &mut PlannerScratch,
    ops: &mut Vec<PagingOp>,
) -> PlanStats {
    ops.clear();
    let pool_slots = slot_cluster.len();
    let mut stats = PlanStats::default();

    // ── rule a: keep-warm + collect the missing set ──
    scratch.missing.clear();
    for &c in desired {
        scratch.desired_flag[c as usize] = true;
    }
    for &c in desired {
        let s = resident[c as usize];
        if s >= 0 {
            slot_last_used[s as usize] = frame;
        } else {
            scratch.missing.push(c);
        }
    }

    let mut evicts = 0usize;

    // ── rule c-up (child retirement): evict a non-desired resident whose
    // group's parents are ALL resident (the coarser parents cover it exactly).
    // Before c-down so a simultaneously-redundant pair keeps the coarser side.
    for slot in 0..pool_slots {
        if evicts >= caps.max_evicts {
            break;
        }
        let c = slot_cluster[slot];
        if c < 0 || scratch.desired_flag[c as usize] {
            continue;
        }
        let g = graph.child_group[c as usize];
        if g < 0 {
            continue;
        }
        let parents = &graph.group_parents[g as usize];
        // A parentless group's region has NO coarser representation — never
        // vacuously true.
        if !parents.is_empty() && all_resident(parents, resident) {
            do_evict(ops, resident, slot_cluster, slot);
            evicts += 1;
        }
    }

    // ── rule c-down (parent retirement): evict a non-desired resident that is
    // a parent of a group whose children are ALL (still) resident — the
    // children tile its region. This replaces the old global
    // `all_desired_resident` gate: one straggler region no longer blocks
    // recycling everywhere else.
    for slot in 0..pool_slots {
        if evicts >= caps.max_evicts {
            break;
        }
        let c = slot_cluster[slot];
        if c < 0 || scratch.desired_flag[c as usize] {
            continue;
        }
        let g = graph.parent_group[c as usize];
        if g < 0 {
            continue;
        }
        if all_resident(&graph.group_children[g as usize], resident) {
            do_evict(ops, resident, slot_cluster, slot);
            evicts += 1;
        }
    }

    // ── rule b: refine — stream desired∧non-resident into FREE slots (the
    // evicts above already freed what they could), capped per frame. ──
    let mut next_free = 0usize;
    let mut loaded = 0usize;
    let mut pool_exhausted = false;
    for i in 0..scratch.missing.len() {
        if loaded >= caps.max_loads {
            break;
        }
        while next_free < pool_slots && slot_cluster[next_free] >= 0 {
            next_free += 1;
        }
        if next_free >= pool_slots {
            pool_exhausted = true;
            break;
        }
        let c = scratch.missing[i];
        do_load(
            ops,
            resident,
            slot_cluster,
            slot_last_used,
            c,
            next_free,
            frame,
        );
        loaded += 1;
    }
    let missing_remaining = scratch.missing.len() - loaded;
    stats.all_desired_resident = missing_remaining == 0;

    // ── rule c-global: the whole desired antichain is resident ⇒ it alone
    // covers the surface ⇒ every remaining non-desired resident is redundant
    // (this is what makes the draw FALL on zoom-out). Per-op safe: residents
    // stay a superset of desired throughout the sweep. ──
    if stats.all_desired_resident {
        for slot in 0..pool_slots {
            if evicts >= caps.max_evicts {
                break;
            }
            let c = slot_cluster[slot];
            if c >= 0 && !scratch.desired_flag[c as usize] {
                do_evict(ops, resident, slot_cluster, slot);
                evicts += 1;
            }
        }
    }

    // ── rule d: coarsen cold groups under pressure — the pool is exhausted
    // with desired clusters still missing, so free slots by swapping whole
    // cold groups for their (fewer) parents. Write-over loads take the
    // children's own slots, so a coarsen needs NO free slots; at the frame
    // boundary the group's region is exactly covered by its parents. ──
    let mut coarsened = 0usize;
    if pool_exhausted && missing_remaining > 0 && caps.max_coarsens > 0 {
        // Candidates: every child resident + cold + non-desired; ≥1 parent
        // (parentless groups can't be coarsened into); the missing parents fit
        // the children's slots. Coldest first (newest child stamp, oldest wins).
        scratch.victims.clear();
        for g in 0..graph.group_count() {
            let parents = &graph.group_parents[g];
            if parents.is_empty() {
                continue;
            }
            let children = &graph.group_children[g];
            let mut newest = 0u64;
            let mut ok = true;
            for &ch in children {
                let s = resident[ch as usize];
                if s < 0 || scratch.desired_flag[ch as usize] {
                    ok = false;
                    break;
                }
                let lu = slot_last_used[s as usize];
                if lu + caps.cold_frames > frame {
                    ok = false;
                    break;
                }
                newest = newest.max(lu);
            }
            if !ok {
                continue;
            }
            let need = parents
                .iter()
                .filter(|&&pc| resident[pc as usize] < 0)
                .count();
            if need > children.len() {
                continue; // net-negative overflow (rare); skip
            }
            scratch.victims.push((newest, g as u32));
        }
        scratch.victims.sort_unstable();
        for vi in 0..scratch.victims.len() {
            if coarsened >= caps.max_coarsens {
                break;
            }
            let g = scratch.victims[vi].1 as usize;
            // Re-validate against the LIVE state: an earlier coarsen this frame
            // may have loaded one of this group's children back in as a parent
            // of another group (fresh stamp ⇒ no longer cold) — skip then.
            let parents = &graph.group_parents[g];
            let children = &graph.group_children[g];
            let mut ok = true;
            for &ch in children {
                let s = resident[ch as usize];
                if s < 0
                    || scratch.desired_flag[ch as usize]
                    || slot_last_used[s as usize] + caps.cold_frames > frame
                {
                    ok = false;
                    break;
                }
            }
            if !ok {
                continue;
            }
            let need = parents
                .iter()
                .filter(|&&pc| resident[pc as usize] < 0)
                .count();
            if need > children.len() {
                continue;
            }
            // Victim slots = the children's slots. Missing parents write over
            // the first `need`; the rest evict. Same frame batch ⇒ crack-free.
            scratch.victim_slots.clear();
            for &ch in children {
                scratch.victim_slots.push(resident[ch as usize] as u32);
            }
            let mut vs = 0usize;
            for &pc in parents {
                if resident[pc as usize] >= 0 {
                    continue;
                }
                let slot = scratch.victim_slots[vs] as usize;
                vs += 1;
                do_load(ops, resident, slot_cluster, slot_last_used, pc, slot, frame);
            }
            for k in vs..scratch.victim_slots.len() {
                do_evict(
                    ops,
                    resident,
                    slot_cluster,
                    scratch.victim_slots[k] as usize,
                );
            }
            coarsened += 1;
        }
    }
    stats.coarsened = coarsened;

    // Clear this frame's desired marks (keep `desired_flag` all-false between
    // frames without a full-vector reset).
    for &c in desired {
        scratch.desired_flag[c as usize] = false;
    }

    stats.streamed = ops
        .iter()
        .filter(|op| matches!(op, PagingOp::Load { .. }))
        .count();
    stats.evicted = ops.len() - stats.streamed;
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The synthetic 3-level DAG from the task spec: one root group `GR`
    /// (parents: root R; children: mids M0, M1) over two leaf groups `GA`
    /// (parents: M0; children: L0, L1) and `GB` (parents: M1; children: L2,
    /// L3). Distinct spheres per group; keys are exact-bit as in the bake.
    ///
    /// ids: 0..=3 leaves L0..L3, 4 = M0, 5 = M1, 6 = R.
    fn synthetic_pages() -> Vec<ClusterPage> {
        // (lod_error, lod_sphere) + (parent_error, parent_sphere)
        let mk =
            |lod_error: f32, lod_c: f32, lod_r: f32, parent_error: f32, par_c: f32, par_r: f32| {
                ClusterPage {
                    center: [lod_c, 0.0, 0.0],
                    radius: lod_r,
                    lod_error,
                    parent_error,
                    lod_bounds_center: [lod_c, 0.0, 0.0],
                    lod_bounds_radius: lod_r,
                    parent_bounds_center: [par_c, 0.0, 0.0],
                    parent_bounds_radius: par_r,
                    first_index: 0,
                    index_count: 3,
                }
            };
        // GA sphere: (10, 1, err 1.0); GB: (20, 1, err 1.0); GR: (15, 4, err 2.0).
        vec![
            mk(0.0, 9.5, 0.4, 1.0, 10.0, 1.0),  // 0: L0 (child of GA)
            mk(0.0, 10.5, 0.4, 1.0, 10.0, 1.0), // 1: L1 (child of GA)
            mk(0.0, 19.5, 0.4, 1.0, 20.0, 1.0), // 2: L2 (child of GB)
            mk(0.0, 20.5, 0.4, 1.0, 20.0, 1.0), // 3: L3 (child of GB)
            mk(1.0, 10.0, 1.0, 2.0, 15.0, 4.0), // 4: M0 (parent of GA, child of GR)
            mk(1.0, 20.0, 1.0, 2.0, 15.0, 4.0), // 5: M1 (parent of GB, child of GR)
            mk(2.0, 15.0, 4.0, ROOT_PARENT_ERROR, 15.0, 4.0), // 6: R (parent of GR, root)
        ]
    }

    /// (i) Group-graph reconstruction on the synthetic DAG: exact groups,
    /// child/parent membership, roots and leaves.
    #[test]
    fn group_graph_reconstruction_synthetic() {
        let pages = synthetic_pages();
        let g = GroupGraph::build(&pages);
        assert_eq!(g.group_count(), 3);
        // gid order follows first child encountered: GA=0 (L0), GB=1 (L2), GR=2 (M0).
        assert_eq!(g.group_children[0], vec![0, 1]);
        assert_eq!(g.group_children[1], vec![2, 3]);
        assert_eq!(g.group_children[2], vec![4, 5]);
        assert_eq!(g.group_parents[0], vec![4]);
        assert_eq!(g.group_parents[1], vec![5]);
        assert_eq!(g.group_parents[2], vec![6]);
        assert_eq!(g.child_group, vec![0, 0, 1, 1, 2, 2, -1]);
        assert_eq!(g.parent_group, vec![-1, -1, -1, -1, 0, 1, 2]);
        let s = g.stats();
        assert_eq!(
            (
                s.groups,
                s.roots,
                s.leaves,
                s.parentless_groups,
                s.unmatched_nonleaf
            ),
            (3, 1, 4, 0, 0)
        );
        // Every non-root lands in exactly one group.
        let n_in_groups: usize = g.group_children.iter().map(|c| c.len()).sum();
        assert_eq!(n_in_groups, pages.len() - s.roots);
    }

    /// (i) Group-graph reconstruction over a REAL DAG (lod-bake over a grid):
    /// the bake writes the shared group sphere by copy, so exact-bit keys must
    /// match every non-leaf lod key to a group, and every non-root to exactly
    /// one group.
    #[test]
    fn group_graph_reconstruction_real_dag() {
        let (pos, indices) = grid_mesh(32);
        let dag = awsm_renderer_lod_bake::build_cluster_dag(
            &pos,
            &indices,
            &awsm_renderer_lod_bake::DagOptions::default(),
        );
        let pages: Vec<ClusterPage> = dag
            .clusters
            .iter()
            .map(|c| ClusterPage {
                center: c.center,
                radius: c.radius,
                lod_error: c.lod_error,
                parent_error: c.parent_error,
                lod_bounds_center: c.lod_bounds_center,
                lod_bounds_radius: c.lod_bounds_radius,
                parent_bounds_center: c.parent_bounds_center,
                parent_bounds_radius: c.parent_bounds_radius,
                first_index: 0,
                index_count: 3,
            })
            .collect();
        assert!(pages.len() > 10, "grid DAG should have several clusters");
        let g = GroupGraph::build(&pages);
        let s = g.stats();
        assert!(s.groups > 0);
        assert_eq!(
            s.unmatched_nonleaf, 0,
            "bit-exact keying must match every non-leaf lod key to a group"
        );
        // Every non-root cluster is a child of exactly one group.
        let n_in_groups: usize = g.group_children.iter().map(|c| c.len()).sum();
        assert_eq!(n_in_groups, pages.len() - s.roots);
        for (c, p) in pages.iter().enumerate() {
            if p.parent_error < ROOT_PARENT_ERROR {
                assert!(g.child_group[c] >= 0, "non-root {c} must have a group");
            } else {
                assert_eq!(g.child_group[c], -1, "root {c} must have no group");
            }
        }
        // Groups have ≥1 parent unless the bake simplified them to nothing —
        // which a plain welded grid never does.
        assert_eq!(
            s.parentless_groups, 0,
            "grid bake groups must all have parent clusters"
        );
    }

    fn grid_mesh(n: usize) -> (Vec<[f32; 3]>, Vec<u32>) {
        let mut pos = Vec::new();
        for y in 0..=n {
            for x in 0..=n {
                pos.push([x as f32, y as f32, 0.0]);
            }
        }
        let idx = |x: usize, y: usize| (y * (n + 1) + x) as u32;
        let mut indices = Vec::new();
        for y in 0..n {
            for x in 0..n {
                indices.extend_from_slice(&[idx(x, y), idx(x + 1, y), idx(x + 1, y + 1)]);
                indices.extend_from_slice(&[idx(x, y), idx(x + 1, y + 1), idx(x, y + 1)]);
            }
        }
        (pos, indices)
    }

    /// Frame-by-frame planner simulator: drives `plan`, replays the emitted
    /// ops into an independent shadow residency (asserting the planner's own
    /// bookkeeping matches), and asserts the surface-cover invariant after
    /// every frame's op batch (leaf-descendant accounting).
    struct Sim {
        graph: GroupGraph,
        resident: Vec<i32>,
        slot_cluster: Vec<i32>,
        slot_last_used: Vec<u64>,
        frame: u64,
        caps: PlannerCaps,
        scratch: PlannerScratch,
        ops: Vec<PagingOp>,
        shadow_resident: Vec<i32>,
        shadow_slot_cluster: Vec<i32>,
        /// `leaf_sets[c]` = the leaf clusters whose region `c` covers.
        leaf_sets: Vec<Vec<u32>>,
    }

    impl Sim {
        fn new(pages: &[ClusterPage], pool_slots: usize, initial: &[u32]) -> Self {
            let graph = GroupGraph::build(pages);
            let mut resident = vec![-1i32; pages.len()];
            let mut slot_cluster = vec![-1i32; pool_slots];
            for (slot, &c) in initial.iter().enumerate() {
                resident[c as usize] = slot as i32;
                slot_cluster[slot] = c as i32;
            }
            let leaf_sets = (0..pages.len())
                .map(|c| {
                    let mut out = Vec::new();
                    collect_leaves(&graph, c as u32, &mut out);
                    out.sort_unstable();
                    out
                })
                .collect();
            Self {
                scratch: PlannerScratch::new(pages.len()),
                shadow_resident: resident.clone(),
                shadow_slot_cluster: slot_cluster.clone(),
                slot_last_used: vec![0u64; pool_slots],
                graph,
                resident,
                slot_cluster,
                frame: 0,
                caps: PlannerCaps::default(),
                ops: Vec::new(),
                leaf_sets,
            }
        }

        fn step(&mut self, desired: &[u32]) -> PlanStats {
            self.frame += 1;
            let stats = plan(
                desired,
                self.frame,
                &self.graph,
                &mut self.resident,
                &mut self.slot_cluster,
                &mut self.slot_last_used,
                &self.caps,
                &mut self.scratch,
                &mut self.ops,
            );
            // Replay ops into the shadow state — the ops alone must reproduce
            // the planner's bookkeeping.
            for op in &self.ops {
                match *op {
                    PagingOp::Load { cluster, slot } => {
                        let old = self.shadow_slot_cluster[slot as usize];
                        if old >= 0 {
                            self.shadow_resident[old as usize] = -1;
                        }
                        self.shadow_resident[cluster as usize] = slot as i32;
                        self.shadow_slot_cluster[slot as usize] = cluster as i32;
                    }
                    PagingOp::Evict { slot } => {
                        let c = self.shadow_slot_cluster[slot as usize];
                        assert!(c >= 0, "evicting a free slot");
                        self.shadow_resident[c as usize] = -1;
                        self.shadow_slot_cluster[slot as usize] = -1;
                    }
                }
            }
            assert_eq!(self.shadow_resident, self.resident, "ops ≠ bookkeeping");
            assert_eq!(self.shadow_slot_cluster, self.slot_cluster);
            // Residency tables are mutually consistent.
            for (slot, &c) in self.slot_cluster.iter().enumerate() {
                if c >= 0 {
                    assert_eq!(self.resident[c as usize], slot as i32);
                }
            }
            self.assert_cover();
            stats
        }

        /// Run `desired` for `frames` frames (stamping each frame).
        fn run(&mut self, desired: &[u32], frames: usize) {
            for _ in 0..frames {
                self.step(desired);
            }
        }

        fn resident_set(&self) -> Vec<u32> {
            let mut out: Vec<u32> = self
                .slot_cluster
                .iter()
                .filter(|&&c| c >= 0)
                .map(|&c| c as u32)
                .collect();
            out.sort_unstable();
            out
        }

        /// Cover invariant: every leaf's region is covered by ≥1 resident
        /// ancestor-or-self.
        fn assert_cover(&self) {
            let leaves: Vec<u32> = (0..self.graph.parent_group.len() as u32)
                .filter(|&c| self.graph.parent_group[c as usize] < 0)
                .collect();
            for &leaf in &leaves {
                let covered = self
                    .slot_cluster
                    .iter()
                    .any(|&c| c >= 0 && self.leaf_sets[c as usize].binary_search(&leaf).is_ok());
                assert!(
                    covered,
                    "HOLE at frame {}: leaf {leaf} uncovered (resident {:?})",
                    self.frame,
                    self.resident_set()
                );
            }
        }
    }

    fn collect_leaves(graph: &GroupGraph, c: u32, out: &mut Vec<u32>) {
        let g = graph.parent_group[c as usize];
        if g < 0 {
            out.push(c);
            return;
        }
        for &ch in &graph.group_children[g as usize] {
            collect_leaves(graph, ch, out);
        }
    }

    const A_FINE: &[u32] = &[0, 1, 5]; // L0, L1 fine; region B coarse (M1)
    const B_FINE: &[u32] = &[4, 2, 3]; // region A coarse (M0); L2, L3 fine

    /// (ii) Wedge-freedom: pool sized for ONE region's fine antichain (3) + 1.
    /// A-fine becomes fully resident; the camera moves to B-fine; the planner
    /// must recycle A's slots and converge to exactly B's antichain within
    /// bounded frames, never exceeding the pool. (The old global gate wedged
    /// here forever.)
    #[test]
    fn wedge_freedom_pool_saturated_camera_move() {
        let pages = synthetic_pages();
        let mut sim = Sim::new(&pages, 4, &[6]); // seed: root R
        sim.run(A_FINE, 5);
        assert_eq!(sim.resident_set(), vec![0, 1, 5], "A fine fully resident");
        // Camera moves to region B. Bounded convergence: well under COLD_FRAMES
        // (rule c-up/c-down un-wedge without waiting for coldness here).
        let mut converged = None;
        for f in 0..(COLD_FRAMES as usize + 20) {
            sim.step(B_FINE);
            if sim.resident_set() == vec![2, 3, 4] {
                converged = Some(f + 1);
                break;
            }
        }
        assert_eq!(
            sim.resident_set(),
            vec![2, 3, 4],
            "planner must converge to B's fine antichain"
        );
        let converged = converged.expect("converged");
        assert!(
            converged <= 5,
            "per-group retirement should converge fast, took {converged} frames"
        );
    }

    /// (ii-b) Rule-d coarsening under pressure: the pool (3) is completely full
    /// of A-fine detail, the camera zooms far out (desired = root), and NO
    /// per-group retirement applies (the parents aren't resident and can't
    /// load — zero free slots). Only coarsen-cold-groups can free space: after
    /// COLD_FRAMES the planner must swap group GA's children for M0 (write-over,
    /// no free slots needed), then load R, then retire the mids.
    #[test]
    fn coarsen_cold_groups_under_pressure() {
        let pages = synthetic_pages();
        let mut sim = Sim::new(&pages, 3, &[0, 1, 5]); // A-fine, pool FULL
        let desired = &[6u32]; // zoom out: root only
        let mut coarsen_frame = None;
        for f in 0..(COLD_FRAMES as usize + 10) {
            let stats = sim.step(desired);
            if stats.coarsened > 0 && coarsen_frame.is_none() {
                coarsen_frame = Some((f + 1, stats.coarsened));
            }
            if sim.resident_set() == vec![6] {
                break;
            }
        }
        let (cf, n) = coarsen_frame.expect("rule d must fire");
        assert_eq!(n, 1, "exactly one group (GA) coarsens");
        assert_eq!(
            cf, COLD_FRAMES as usize,
            "coarsen fires exactly when the slots go cold"
        );
        assert_eq!(sim.resident_set(), vec![6], "converged to the root");
        // Pool bound was never exceeded (Sim asserts consistency each frame; the
        // pool is 3 slots by construction).
    }

    /// (iii) Cover invariant under sustained thrash: cycle camera targets
    /// (fine A, fine B, root, full fine, mids) with a small pool; the Sim
    /// asserts the leaf-cover after EVERY frame's op batch, and each held
    /// target must converge to exactly its antichain.
    #[test]
    fn cover_invariant_thrash_cycles() {
        let pages = synthetic_pages();
        let mut sim = Sim::new(&pages, 5, &[6]);
        let hold = COLD_FRAMES as usize + 15;
        let targets: &[&[u32]] = &[A_FINE, B_FINE, &[6], &[0, 1, 2, 3], &[4, 5], A_FINE, &[6]];
        for target in targets {
            sim.run(target, hold);
            let mut want = target.to_vec();
            want.sort_unstable();
            assert_eq!(
                sim.resident_set(),
                want,
                "held target must converge exactly"
            );
        }
    }

    /// (iv) No-op parity when the mesh fits the pool: loads happen once, then
    /// the planner is silent — and the coarse root is NEVER evicted while any
    /// of the desired refinement is still missing (the old crack-free gate's
    /// behavior, now per-region).
    #[test]
    fn no_op_parity_when_pool_fits() {
        let pages = synthetic_pages();
        let mut sim = Sim::new(&pages, 16, &[6]);
        let desired = &[0u32, 1, 2, 3];
        let stats = sim.step(desired);
        assert_eq!(stats.streamed, 4, "all four leaves stream in frame 1");
        assert_eq!(stats.evicted, 1, "the root retires the same frame");
        assert_eq!(sim.resident_set(), vec![0, 1, 2, 3]);
        for _ in 0..100 {
            let stats = sim.step(desired);
            assert_eq!(stats.streamed, 0, "steady state must be silent");
            assert_eq!(stats.evicted, 0);
            assert_eq!(stats.coarsened, 0);
            assert!(sim.ops.is_empty());
        }
    }

    /// (iv-b) Never evict a coarse parent while its replacement children are
    /// missing: with the per-frame load cap at 2, frame 1 loads only L0+L1 —
    /// the root R must survive (it still covers L2/L3's region). It retires
    /// only after frame 2 completes the antichain.
    #[test]
    fn parent_survives_until_children_resident() {
        let pages = synthetic_pages();
        let mut sim = Sim::new(&pages, 16, &[6]);
        sim.caps.max_loads = 2;
        let desired = &[0u32, 1, 2, 3];
        let stats = sim.step(desired);
        assert_eq!(stats.streamed, 2);
        assert_eq!(stats.evicted, 0, "R must survive while leaves are missing");
        assert!(sim.resident_set().contains(&6));
        let stats = sim.step(desired);
        assert_eq!(stats.streamed, 2);
        assert_eq!(stats.evicted, 1, "R retires once the antichain completes");
        assert_eq!(sim.resident_set(), vec![0, 1, 2, 3]);
    }

    /// A parentless group (the bake simplified it to nothing) must never
    /// vacuously justify retiring its children (rule c-up) nor be a rule-d
    /// victim — its region has no coarser representation.
    #[test]
    fn parentless_group_children_never_evicted() {
        let mk =
            |lod_error: f32, lod_c: f32, lod_r: f32, parent_error: f32, par_c: f32, par_r: f32| {
                ClusterPage {
                    center: [lod_c, 0.0, 0.0],
                    radius: lod_r,
                    lod_error,
                    parent_error,
                    lod_bounds_center: [lod_c, 0.0, 0.0],
                    lod_bounds_radius: lod_r,
                    parent_bounds_center: [par_c, 0.0, 0.0],
                    parent_bounds_radius: par_r,
                    first_index: 0,
                    index_count: 3,
                }
            };
        let pages = vec![
            // 0,1: leaves of a group whose sphere (30,1,err 0.5) matches NO
            // cluster's lod key — a simplify-to-nothing group (their region has
            // no coarser representation; the cut drops it past err 0.5).
            mk(0.0, 29.5, 0.4, 0.5, 30.0, 1.0),
            mk(0.0, 30.5, 0.4, 0.5, 30.0, 1.0),
            // 2: a root that is the sole parent of group G1 (sphere (50,1,0.8)).
            mk(0.8, 50.0, 1.0, ROOT_PARENT_ERROR, 50.0, 1.0),
            // 3: G1's sole child — desired but never loadable (pool pressure).
            mk(0.0, 50.2, 0.4, 0.8, 50.0, 1.0),
        ];
        let g = GroupGraph::build(&pages);
        assert_eq!(g.stats().parentless_groups, 1);
        // Pool FULL with {0, 1, 2}; the camera wants {3} (region 0/1 vanished
        // from the cut, region 2 refined to its child 3). Nothing can move:
        // 3 can't load (no free slot), 2 can't retire (its child 3 isn't
        // resident), and 0/1 must not be victimized despite going ice-cold —
        // their group has no parents to coarsen into. Wedging CONSERVATIVELY
        // (stale detail, zero holes) is the correct outcome here.
        let mut sim = Sim::new(&pages, 3, &[0, 1, 2]);
        for _ in 0..(COLD_FRAMES as usize * 3) {
            let stats = sim.step(&[3]);
            assert_eq!(stats.coarsened, 0, "parentless group must not coarsen");
            assert_eq!(stats.evicted, 0, "nothing is safely evictable");
        }
        assert_eq!(
            sim.resident_set(),
            vec![0, 1, 2],
            "children of a parentless group must stay resident"
        );
    }
}
