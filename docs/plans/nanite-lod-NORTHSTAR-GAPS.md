# North-Star Gaps — docs/nanite-lod.md not yet fully met

Honest status of the cluster-LOD / virtual-geometry implementation vs. the
permanent spec `docs/nanite-lod.md`, per the acceptance checklist
`docs/plans/nanite-lod-acceptance.md`.

**Verified: 2 / 6 headline claims** (A4, A5) — each with a committed deterministic
test AND cited on-device evidence. **A1 DOWNGRADED (see 🚨 P0 below):** its CPU
bake/cut test still passes, but its on-device "watertight GPU draw" evidence is now
CONTRADICTED — the GPU cluster cut emits 0 triangles on-device.

## 🚨 P0 — the GPU cluster cut selects 0 clusters on-device (cut shader ≠ CPU reference)

Found iter 24 via a periodic `draw_args.index_count` readback (render.rs, fires
frame 5 then every 30 — was one-shot frame-1, which hid this). On a subdivided-sphere
cluster bundle, steadily across thousands of frames:
`cluster compaction (GPU): draw_args.index_count = 0 (0 tris)` — in BOTH `?vg`
(non-paging, 13065 real-error pages) AND `?vg&paging` (785 clamped frontier). The
readback is reliable (the `copy_buffer_to_buffer(draw_args)` is recorded after the
compaction compute pass in the same encoder ⇒ WebGPU auto-barriers it).

**Decisive cross-check:** the CPU `paging_update` (step 20a) logs `desired cut = 187
clusters` using the SAME camera (`cam.position_world`) the GPU cut reads — so the
camera is NOT degenerate and the tested CPU `select_cut_per_cluster` selects 187.
The GPU cut selecting 0 vs the CPU reference's 187 ⇒ a bug in the **GPU cluster-cut
shader** (`cluster_cut.wgsl`) or its **params/page upload** (`ClusterCutParams` /
`ClusterPage` GPU layout), NOT the camera, NOT paging (reproduces with paging off).

**Consequence:** the cluster-LOD GPU draw has been rendering NOTHING on-device.
A1's "on-device subdivided sphere watertight under ?vg" evidence is false/regressed.
ALL of Gap B (A2 streaming) is moot until the GPU cut draws the CPU-reference cut.

**Next (top priority):** root-cause the GPU-cut-vs-CPU-reference divergence. Add a
one-shot log of the cut's `selected` count (not just compaction draw_args) to split
cut-bug vs compaction-bug; dump `ClusterCutParams` bytes + a couple of `ClusterPage`
GPU records vs the CPU structs to check the std430 layout the shader reads; re-derive
the cut predicate in `cluster_cut.wgsl` against `select_cut_per_cluster`. Fix so the
GPU `index_count` ≈ the CPU `desired cut` (e.g. ~187 → ~561 indices… actually 187
clusters × their tri counts), and a real screenshot shows the sphere. Only then
resume Gap B. Re-verify A1 on-device after the fix.

---


| Claim | Status | Evidence |
|---|---|---|
| **A1** crack-free per-cluster cut incl. non-watertight/subdivided, full-detail + capped | ⚠️ **CONTRADICTED on-device** | CPU bake/cut test still passes (`cb3b1ac8` weld+lock_boundaries, `73984b4b` antichain). BUT the on-device watertight claim is FALSE as of iter 24 — the GPU cut emits 0 tris (🚨 P0 above). Re-verify after the GPU-cut fix. |
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
   `plan_stream_evict` (LRU stream/evict, covers 3d+4).

   **Step 20a DONE (per-frame paging manager scaffold).** `ClusterPaging` now lives on
   `ClusterLodRenderPass` (`renderer/src/render_passes/cluster_lod/render_pass.rs`),
   armed at load with the FULL un-clamped DAG via `init_cluster_paging` (scene-loader,
   only under `cluster_paging`). `AwsmRenderer::update_cluster_paging` (render.rs, called
   before `ctx` is built — `ctx` borrows `self.render_passes`, so the per-frame mutation
   must precede it) runs the CPU per-cluster cut over the full DAG each frame into pooled
   scratch and logs it on change. On-device (`?vg&paging`, subdivided sphere): the manager
   armed with **13065 clusters**, and the per-frame cut logged **`desired cut = 187
   clusters (full DAG = 13065, resident frontier = 785)`** — the camera-driven CPU cut
   runs live. Gated default-off ⇒ byte-identical (no GPU/draw-path change; log + CPU only).

   **REMAINING for A2 — step 20b: geometry streaming into slots.** The decisive
   capability: page a cluster into a reused slot by overwriting that slot's EXPLODED
   vertex sub-range in M's visibility-geometry data buffer (the buffer + per-mesh offset
   exist — `meshes::visibility_geometry_data_gpu_buffer` + `..._buffer_offset`; needs a
   renderer API to `queue.writeBuffer` a `[slot*PAGE_VERTS, +PAGE_VERTS)` sub-range of
   exploded attrs) plus rewrite that slot's `source_indices` span + the resident table.
   Then drive it per-frame from the desired cut via `plan_stream_evict`. Manager must hold
   the cluster geometry CPU-side (cm) to build a slot's exploded verts. After that:
   on-device dolly-in refine (crack-free) + `?stress=N` no-per-frame-allocs → A2.
   (Harness note: `load_player_bundle` is a reset-to-empty round-trip self-test ⇒ the
   scene tree ends empty; getting a *visible* cluster screenshot to confirm refinement
   needs a persistent scene path — resolve as part of 20b's on-device verify.)

   **Step 20b-i DONE (slot exploded-vertex byte builder).** `mesh_pack::pack_visibility_slot_bytes`
   packs ONE page-pool slot's `PAGE_VERTS` exploded 56-B visibility records from a cluster's
   triangle-order index slice, with slot-relative `triangle_index` (`pool_slot*(PAGE_VERTS/3)
   + local_tri`) so the visibility-resolve's per-triangle corner fetch stays self-consistent
   after a slot is overwritten. Unit-tested (`slot_pack_matches_full_packer_except_triangle_index`):
   slot 0 is byte-identical to `pack_visibility_bytes`; slot N differs ONLY in `triangle_index`;
   the `out` buffer is reused (no per-frame alloc). Synthetic tangents (cluster material has no
   normal map ⇒ the full packer also used synthetic — matched). Pure + unwired ⇒ byte-identical.

   **IMPORTANT model finding (do NOT skip — drives 20b-ii/iii):** step 2's GPU upload is a
   FIXED 785-cluster *frontier* (clamped errors, identity resident table). True dynamic paging
   needs a different data model: upload **all ~13k DAG pages** to the cut (un-clamped real
   `[lod_error,parent_error)`; 13065×64B≈836KB, trivial) + a **full-DAG resident table**
   (cluster_id→slot, −1=absent; ~52KB) so ANY cluster can occupy a slot over time. Crack-free
   fallback = the deepest-resident cluster on each path stays CLAMPED always-draw (lod_error0/
   parent_errorMAX); when finer clusters stream in, un-clamp the parent + clamp the new finer
   leaves (re-upload the pages' error fields or a parallel clamp array). So per frame the
   manager updates: (a) residency/slots (writeBuffer slot vertex data via `pack_visibility_slot_bytes`
   + slot `source_indices` span), (b) per-page clamp state, (c) the resident table. This is a
   real redesign of the `cluster_paging` load path (currently the bounded 785-frontier), to be
   landed gated so flag-off stays byte-identical and flag-on stays watertight at each step.
   Remaining 20b sub-steps: (ii) renderer writeBuffer API for a slot's data sub-range + source_indices
   span + resident entry (+ byte-math test); (iii) load path uploads all pages + full-DAG resident
   table, init residency = the coarse antichain in slots (verify still watertight); (iv) per-frame
   stream/evict + re-clamp driven by `plan_stream_evict` → dolly-in refine on-device → A2.

   **Step 20b-iii DONE (load-path manager enrichment — prep, no GPU/draw change).** The
   `cluster_paging` load path now seeds the manager fully: `select_resident_clusters` also returns
   the chosen cm-cluster ids (slot order); `ClusterPagingInit { pages(full DAG), positions, normals,
   indices, slot_cluster }` arms `ClusterPaging` with the CPU geometry the streamer gathers slot
   verts from + the residency bookkeeping (`resident[]` full-DAG, `slot_cluster[]`, `slot_last_used[]`,
   `pool_slots`). The GPU upload is UNCHANGED from step 2 (same 785-frontier pages / identity resident
   table / 785-slot M), so the rendered state is byte-identical to step 2; the new manager fields are
   `#[allow(dead_code)]` until the per-frame streamer (20b-iv) consumes them. On-device (`?vg&paging`):
   cluster mesh loads with NO PANIC (13065 clusters / 785 resident), page pool builds (785 slots/29322
   tris), and the manager fires (`desired cut = 187`). Gate green; flag-off byte-identical.

   **🚨 BLOCKER (must resolve before 20b-iv / A2 — elevated this iter):** the one-shot GPU readback
   logs `cluster compaction (GPU): draw_args.index_count = 0 (0 tris) over 785 clusters` on frame 1,
   and `load_player_bundle` resets the scene to empty so `screenshot_scene` shows the (empty) editor —
   i.e. there is currently NO positive on-device signal that the cluster draw is non-zero / pixels
   appear. The clamped frontier pages (lod_error0/parent_errorMAX) should pass the cut at any camera,
   so 0 is most likely a frame-1 transient (resident table / bind group not yet effective for that
   first cut), but THIS IS UNPROVEN. Next iteration MUST settle it FIRST: e.g. make the cut/compaction
   count log on a LATER/steady frame (not one-shot frame-1), or find a persistent viewable cluster
   scene (frame_node / non-reset load), and confirm draw_args.index_count ≈ 29322*3 + a visible
   sphere. A2's dolly-in-refine demo is impossible to verify without this. (Pre-existing — not caused
   by 20a/20b-i/ii/iii, which add no draw-path change — but blocking.)

   **Step 20b-ii DONE (renderer slot-write API).** `AwsmRenderer::write_cluster_slot(slot, &[u8])`
   `queue.writeBuffer`s one slot's exploded records into M's visibility-data section of the merged
   geometry pool (`COPY_DST` confirmed) at `mesh_data_offset + slot*slot_bytes` (pure helper
   `cluster_slot_data_offset`, unit-tested: contiguous, non-overlapping slots).
   `write_cluster_source_indices_span(first_index, &[u32])` + `write_cluster_resident_entry(cluster_id,
   slot)` overwrite a page's slot-relative draw indices + a single residency entry in place
   (`ClusterLodBuffers::write_source_indices_span` / `write_resident_entry`). Pure + UNWIRED (no
   per-frame call) ⇒ byte-identical; tests + wasm build green. (Per-frame caller in 20b-iv pools the
   source_indices serialization — noted in the API.)

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
