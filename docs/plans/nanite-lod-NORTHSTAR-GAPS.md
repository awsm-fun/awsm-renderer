# North-Star Gaps — docs/nanite-lod.md not yet fully met

Honest status of the cluster-LOD / virtual-geometry implementation vs. the
permanent spec `docs/nanite-lod.md`, per the acceptance checklist
`docs/plans/nanite-lod-acceptance.md`.

**Verified: 3 / 6 headline claims** (A1, A4, A5) — each with a committed
deterministic test AND cited on-device evidence.

| Claim | Status | Evidence |
|---|---|---|
| **A1** crack-free per-cluster cut incl. non-watertight/subdivided, full-detail + capped | ✅ | `cb3b1ac8` bake weld+lock_boundaries, `73984b4b` capped complete-antichain; on-device subdivided sphere watertight under `?vg` and `?streambudget=8000` |
| **A2** dynamic camera-driven streaming residency (multi-M-tri, bounded VRAM, LRU, crack-free fallback, no per-frame allocs) | ❌ **UNMET** | Gap B foundation only (see below) |
| **A3** drawn (cut) tri count bounded by screen res, not source size (benchmark across scales) | ❌ **UNMET** | partial evidence only (1696 drawn vs 583768 source at one scale); needs the A2 multi-scale benchmark |
| **A4** deforming → discrete chain, per-instance, skin/morph carried | ✅ | `c58abfd9` carry-through test + on-device mixed CesiumMan/MorphCube/Sphere routing |
| **A5** flags off ⇒ byte-identical | ✅ | `1f5dba9d` defaults test + on-device no-cluster-pipelines-when-off |
| **A6** final multi-M-tri benchmark TABLE (1080p+4K, per-pass + cut-vs-source + VRAM) in docs | ❌ **UNMET** | blocked on A2 |

---

## A2 — dynamic per-frame paging (Gap B). UNMET.

**Why unmet.** The shipped streaming is **static** (Step 1 / `cluster_streaming`):
it caps residency once at load to a crack-free complete-antichain frontier (now
crack-free after A1's fix). The north star requires **camera-driven** residency: a
fixed GPU page pool whose slots stream in finer pages near the camera and evict
cold ones (LRU) within a bounded VRAM budget, with a crack-free coarse fallback
while a wanted page is still loading — and no per-frame heap allocations.

**What's done (committed, gated default-off, byte-identical — `9f52aa6a`, `c47e91fb`):**
- `cluster_paging` renderer feature flag (features.rs; asserted off by
  `default_features_are_all_off`).
- Pure, unit-tested CPU page-pool planner `plan_page_pool` (scene-loader):
  cluster→slot `resident` table + occupancy/overflow; consts
  `CLUSTER_PAGE_VERTS=384`, `CLUSTER_PAGE_POOL_SLOTS=8192`. 3 unit tests.
- `?paging` editor URL flag; on-device `?vg&paging` is **byte-identical** to `?vg`
  (same cut counts, hole-free) and the planner logs occupancy
  (`13065 resident → 8192 slots, overflow 4873` on the subdivided sphere).

**What remains (each a gated/tested/on-device step; this is a large, high-risk,
multi-file GPU build — realistically multi-day):**
1. **GPU resident table + cut variant.** A `resident: array<i32>` GPU buffer
   (cluster→slot, −1 = absent) uploaded from `plan_page_pool`, bound into the cut
   as a **shader variant** (cache_key `paging:bool` → template branch → conditional
   `@binding`), reading `resident[i]` (skip if <0). Strictly gated so flag-off keeps
   the shipped single-pipeline cut byte-identical. Touches: `buffers.rs`,
   `bind_group.rs`, `shader/{cache_key,template}.rs` + `cluster_cut.wgsl`,
   `pipeline.rs`, `render.rs`, scene-loader. (Started here; reverted as a single
   slice was too wide to land + verify safely in one step.)
2. **Slot-relative geometry — ✅ DONE (bounded pool).** `cluster_paging` now implies a
   residency budget (`CLUSTER_PAGING_BUDGET_TRIS=30k`, `?streambudget=N` overrides) so
   the resident set is bounded; `build_slot_geometry` packs it into a fixed
   `CLUSTER_PAGE_VERTS`-slot pool (sized to the resident count), M = the slot buffer,
   compaction emits slot-relative indices, resident table uploaded. On-device
   (`?vg&paging`): 785 slots × 384 verts (29322 capped tris), **watertight, no OOM** —
   the cluster geometry now lives in independently-swappable slots.
   *(History: a first attempt at FULL residency blew the 512 MB GPU buffer cap (~1 GiB,
   loud-panic) — full-residency-through-fixed-slots is infeasible; the pool must be
   bounded, hence the budget. The renderer's guard caught it; reverted then re-done.)*
3. **Dynamic streaming (the A2 core — multi-iteration). PIVOTED to CPU-driven (simpler).**

   **PIVOT (2026-06-25):** drop the GPU feedback/readback loop. At our scale (≤~80k
   clusters for a 5–10M-tri asset) the CPU can run the cut itself each frame
   (sub-ms) and diff the desired resident set against current residency — GPU feedback
   only pays off at 100s-of-millions of clusters. This removes the feedback buffer +
   atomic + async readback + cut-shader-write entirely. Plan: a per-frame CPU "paging
   update" (has camera + DAG + residency state) computes the desired cut
   (`cluster_lod::select_cut_per_cluster`), then `plan_stream_evict` (DONE, tested)
   decides loads/evicts (free slots first, then coldest non-desired LRU, capped per
   step); the CPU `writeBuffer`s each loaded cluster's geometry into its slot, updates
   `resident[]`, re-clamps the deepest-resident frontier (always-drawn ⇒ crack-free),
   re-uploads the resident table. The GPU cut is unchanged (draws the resident
   frontier, step 2). CPU bricks DONE + unit-tested: `cluster_finer_group` (3a),
   `plan_stream_evict` (LRU stream/evict, covers 3d+4). REMAINING: the per-frame paging
   manager (persistent residency state + camera hook in the renderer + per-slot
   writeBuffer + re-frontier), then on-device dolly-in refine verify + `?stress=N`
   no-per-frame-allocs → A2.

   *(Superseded GPU-feedback design (A) kept below for reference.)*

   **Key constraint (found analysing it):** the GPU cut CANNOT "walk up to the nearest
   resident ancestor" when a wanted cluster is absent — `ClusterPage` has bounds/errors
   but NO parent/child cluster *indices*. So crack-free fallback must NOT be a GPU
   parent-walk. Two viable designs:

   **(A) CPU-managed always-drawn frontier (preferred — no bake format change, no GPU
   parent-walk).** Keep the resident set a COMPLETE ANTICHAIN ("frontier") whose leaves
   are clamped always-drawn (lod_error=0/parent_error=MAX, as Step-1 does) ⇒ always
   crack-free. Make it *camera-adaptive* by streaming:
   - Upload ALL cluster pages to the cut (not just the resident subset) so the cut can
     evaluate finer-than-frontier clusters. Add each resident frontier leaf's ORIGINAL
     lod_error (a field) so the cut can tell when the camera out-resolves it.
   - Cut, per resident frontier leaf F: if `projected(F.original_lod_error) > budget`
     (camera wants finer than F), append F to a `feedback` buffer (atomicAdd counter +
     id list, capped). Still draw F (clamped) this frame ⇒ crack-free now.
   - CPU (one-frame-latent, pooled readback — no per-frame alloc): for each fed-back F,
     stream F's CHILDREN into free slots, set their resident slots, make them the new
     frontier leaves (clamp them), un-clamp/remove F from the drawn set. Frontier stays
     a complete antichain ⇒ crack-free across the transition; refines over a frame/two.
   - Needs CPU **DAG group links**. The DAG is GROUP-based: clusters simplified
     together share a group sphere (`lod_bounds`) and flip together (crack-free), so
     the unit of refinement is the GROUP, not one cluster. The finer clusters whose
     group produced F satisfy `c.parent_bounds == F.lod_bounds && c.parent_error ==
     F.lod_error` (exact f32 — the bake assigns the same group sphere/error to both
     sides, so an exact-bits match works, no epsilon). Refining F streams ALL those
     finer clusters in as a group (and the whole frontier group F belongs to refines
     together) ⇒ the new frontier stays a valid antichain ⇒ crack-free. Build this on
     the ORIGINAL bake `cm.clusters` (NOT the post-`select_resident_clusters` pages,
     whose lod_error/parent_error are clamped to 0/MAX). Cleaner alternative: emit
     explicit group/child ids in the bake (a lod-bake format change + re-bake).
   - Eviction (step 4): when the camera pulls back, fed-back-stale leaves coarsen —
     evict their slots (LRU) and re-clamp the parent. 

   **(B) Encode parent/child ids in the bake** so the GPU cut walks to the nearest
   resident ancestor directly. Simpler shader logic but a bake format change + re-bake
   and a per-cluster GPU walk. Heavier; only if (A) proves insufficient.

   Implement (A) as small gated/tested/on-device commits: (3a) CPU DAG-links +
   frontier-refine planner (pure, unit-tested); (3b) feedback buffer + cut writes
   too-coarse leaves (bind into paging variant); (3c) pooled async readback; (3d) CPU
   stream children into slots (writeBuffer at slot offset) + re-frontier; verify
   on-device that dollying in REFINES detail crack-free with no per-frame allocs.
4. **LRU eviction** (slot `last_used_frame`, skip slots used this frame) + per-frame
   upload byte budget so a camera jump doesn't hitch.
5. **Multi-million-tri on-device verify** (subdivide to ≥5–10M source tris or instance
   many distinct datasets): full detail near camera, bounded VRAM, stream-in/evict
   while dollying, crack-free; `?stress=N` + `?trace=sub-frame` ⇒ **no per-frame heap
   allocs** (pool the readback + upload staging — see
   `avoid-per-frame-allocations-standard`).

## A3 — cut bounded by screen res, not source size. UNMET.

Partial evidence exists (the cut drew 1696 tris of a 583768-tri DAG at one
resolution/budget). The claim requires showing the cut stays ~flat as the **source**
scales (1M → 10M → …) at fixed resolution + error budget — i.e. the A6 benchmark
across several source densities. Blocked on A2 (need a genuine multi-M-tri asset
streaming) for the upper scales.

## A6 — final multi-million-tri benchmark TABLE. UNMET.

Requires real numbers at 1080p + 4K (`?trace=sub-frame`): total frame + per-pass
(cut / compaction / geometry / shading); cut-size-vs-source; page-pool occupancy +
eviction churn while dollying; peak VRAM. Blocked on A2.

---

## Next concrete step

Resume Gap B at step 1 above (GPU resident table + cut shader variant), as a
sequence of **small** gated/tested/on-device commits (resident-buffer alloc+upload;
then bind+variant; then verify identical), rather than one wide change. Update
`docs/plans/nanite-lod-acceptance.md` as each lands and delete this file once A2/A3/A6
are all ✅.
