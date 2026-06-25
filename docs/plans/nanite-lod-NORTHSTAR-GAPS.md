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
2. **Slot-relative geometry.** Pack resident clusters into `CLUSTER_PAGE_VERTS`-padded
   pool slots; compaction emits `slot*PAGE_VERTS`-relative indices. Identical render
   under full residency.
3. **Feedback + readback.** `feedback` buffer (atomicOr/append wanted-but-absent
   cluster ids) + async readback (reuse `render.rs:1661-1686 extract_buffer_vec`) +
   CPU upload-into-slot (grow-only); crack-free coarse fallback to nearest resident
   ancestor.
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
