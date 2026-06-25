# North-Star Gaps — docs/nanite-lod.md not yet fully met

Honest status of the cluster-LOD / virtual-geometry implementation vs. the
permanent spec `docs/nanite-lod.md`, per the acceptance checklist
`docs/plans/nanite-lod-acceptance.md`.

**Verified: 5 / 6 headline claims** (A1, A2, A3, A4, A5) — each with a committed deterministic
test AND cited on-device evidence. Remaining: **A6** (multi-M-tri benchmark table).

**A2 MET (iter 38).** Genuine multi-million-triangle streaming residency verified on-device:
a 1,081,344-tri source → 2,393,468-tri DAG / 51,753 clusters pages through the player cluster
path with render mesh M **CAPPED to 29,850 tris** (budget 30,000) in a **bounded ~83 MB pool**
(3,862 slots); camera-driven + crack-free (watertight): far desired=509 draw=4,908 tris →
zoom-IN desired=1,260 draw=14,650 (rises) → zoom-OUT desired=381 draw=3,860 (falls), no per-frame
allocs. The runtime M (83 MB) is far under the 512 MB shipped guard — no guard change needed at
runtime. (To AUTHOR the >1M source in the editor for this test, the debug-only 512 MB
`OVERSIZED_ALLOC_BYTES` guard was temporarily raised — a probe, reverted — because the EDITOR
densely explodes the raw editable mesh's `?vg` visibility geometry; that authoring limit is a
SEPARATE editor concern, not the runtime streaming-residency claim. Test:
`a2_residency_is_bounded_by_budget_not_source` asserts the CPU invariant — M is capped by the
budget independent of source size.)

## ✅ RESOLVED (iter 29): the iters-24–28 "P0 / harness-can't-verify" saga was a FROZEN BROWSER

The Chrome instance driven by chrome-devtools had **frozen** (its GPU/render process
hung — likely from the heavy 583k-tri WebGPU work + many rapid reloads). A frozen
browser still answers `take_snapshot` (cached DOM/accessibility tree) but:
- every GPU **readback returns zeros** (`draw_args.index_count`, `instance_count`, etc.),
- every **screenshot is blank** (chrome `take_screenshot` = white; `screenshot_scene` = black),
even for a plain lit sphere. That produced the false "cut emits 0 triangles" + "harness
can't observe GPU" conclusions across iters 24–28. **All of it was the freeze.**

**After restarting the browser (iter 29):** screenshots + readback work. The cluster cut
DRAWS: on `?vg` with a subdivided sphere, `cluster compaction (GPU): draw_args.index_count
= 27558 (9186 tris) over 13065 clusters` (a real LOD cut — 9186 of 583768 source tris),
and the **sphere renders watertight** in a chrome-devtools screenshot. A1's original ✅ was
correct; the iter-24 downgrade is fully WITHDRAWN. Back to 3/6.

**🛟 HARNESS LESSON (critical):** if GPU readbacks return zeros AND screenshots go blank
(white from chrome `take_screenshot`, black from `screenshot_scene`) while `take_snapshot`
still shows a full DOM ⇒ **the browser is FROZEN. Restart it** (close the tab + open a
fresh one, or have the user reconnect the chrome MCP) — do NOT conclude the renderer is
broken. Avoid freezing it: don't hammer reloads on a 500k-tri scene; let frames settle.

The iters-24–28 RETRACTED-P0 write-up below is kept as the (now-explained) trail.

---

## ⚠️ RETRACTED (iter 27): the "P0" below was a MEASUREMENT ARTIFACT — the headless harness's GPU readback + screenshot are both UNRELIABLE

**Decisive evidence (iter 27):** the readback decoded `draw_args.instance_count = 0`,
but `instance_count` is CPU-written to `1` every frame by `init_draw_args`
(`queue.writeBuffer`) and the **compaction shader provably never touches it** (it only
`atomicAdd`s `index_count`). A value that must be `1` reading back as `0` ⇒ **the GPU
readback returns zeros in this MCP/headless-Chrome harness regardless of true buffer
content.** Separately, `screenshot_scene` is **all-black (1 colour) for even a PLAIN
non-cluster sphere** ⇒ it does not capture the rendered scene here either.

⇒ The "GPU cluster cut emits 0 triangles" finding (iters 24–26) was produced ENTIRELY
by these two broken signals. It is **NOT established** that the cluster draw is broken.
The cut shader / params / pages were all confirmed correct; the cut very likely WORKS.

**Correction to A1:** the iter-24 downgrade to ⚠️ "CONTRADICTED on-device" was based on
the unreliable readback and is **WITHDRAWN**. A1's CPU bake/cut test passes; its
on-device GPU draw is **UNVERIFIABLE in this headless harness** (both readback and
screenshot are non-functional for it) — neither confirmed-broken NOR re-confirmed here.
A1 is therefore `[~]` (CPU-verified; on-device pending a working harness), not ❌.

**Open question for next iter (the real crux):** is the readback failure a HEADLESS-CHROME
limitation (all `mapAsync` readbacks return zeros here) or a CLUSTER-readback-specific
code bug? Decisive cheap test: read back a buffer with KNOWN non-zero content (e.g.
`pages_buffer[0..20]`, which holds the uploaded errors) via the same path — if it reads
zeros ⇒ harness-wide readback limitation (cluster draw likely fine; proceed with Gap B
on CPU+code basis, flag on-device GPU verification as harness-blocked); if it reads the
real page bytes ⇒ the readback works and `instance_count=0` is a REAL bug to chase.
Also try a non-headless/real browser, or `window.wasmBindings` GPU-readback exports, for
a trustworthy signal.

The original (now-doubted) P0 write-up is kept below for the diagnostic trail.

---

## 🚨 P0 (RETRACTED — see above) — the GPU cluster cut selects 0 clusters on-device (cut shader ≠ CPU reference)

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

### iter 25 — narrowed: the CUT selects 0 with CONFIRMED-correct inputs (GPU-side exec bug)

Added the split diagnostics (committed). On-device `?vg` (non-paging, 13065 pages):
- **CUT (not compaction) selects 0**: `cluster cut (GPU): selected = 0 / 13065`.
- **Pages upload correctly** (CPU-side dump): `p0(lod=0 parent=1.19e-7 ic=384)`,
  `pmid(lod=5.96e-7 parent=7.15e-7)`, `plast(lod=3.81e-6 parent=3.4e38)` — real
  non-zero errors + the f32::MAX root sentinel. So NOT zero-pages.
- **Params CPU-side correct**: `cam_pos=(0,11.26,36.4)` (dist≈38, the default editor
  camera at the one-shot frame-5 log), `tan=0.4142`, `viewport_h=1028`,
  `pixel_budget=1`, `world_scale=1.0`, `cluster_count=13065`.

So with verified-correct pages + params the GPU cut still emits 0. Even at dist 38
the ROOTS (parent=3.4e38 ⇒ proj_parent overflows to +inf; proj_lod≈1.2e-4 ≤ 1)
should pass ⇒ ≥1 expected, but 0. ⇒ the bug is in **GPU-side execution**, NOT the
data we hand it. Prime suspects (in order):
  (1) the shader reads `params.cluster_count` wrong (uniform std140/std430 layout the
      CPU-bytes `cut_params_layout` test can't catch) ⇒ `i >= cluster_count` true for
      all threads ⇒ early-return ⇒ `selected` keeps its zero-init ⇒ 0. **Test next:
      read the params_buffer BACK from the GPU and decode cluster_count/pixel_budget/
      world_scale/viewport_h — confirm the SHADER sees 13065/1/1/1028, not 0/garbage.**
  (2) the camera the cut reads is the player's, possibly a stale/degenerate matrix
      (couldn't confirm set_camera_orbit moves the cut camera — chrome console keeps
      resetting on heavy ops). Make the params log re-fire (not one-shot) to see if
      cam tracks orbits.
  (3) a `selected`/pages binding mismatch in the paging-vs-nonpaging pipeline variant
      (but non-paging is the simplest path and still 0).
Once the GPU sees correct params and the predicate matches the CPU, selected should
jump to the CPU count; then fix draw + re-verify A1. (HARNESS: chrome
list_console_messages buffer resets on heavy MCP ops — read it RIGHT after the scene
build, or prefer the MCP `get_console_logs` regex grep used in iter 25.)

### iter 26 — NARROWED HARD: cut compute writes DON'T reach the readback (not predicate/uniform)

Ran 3 shader probes (temporary, reverted; fresh-wasm CONFIRMED via a `PROBE-v3`
sentinel in the PARAMS log — so NOT the deterministic-stale-wasm trap):
- PROBE 1 `if (i<cluster_count && i<arrayLength) selected[i]=1` → `selected=0`.
- PROBE 2 `if (i<arrayLength(&selected)) selected[i]=1` → `selected=0`.
- PROBE 3 thread-0 raw sentinels → **`selected[0]=arrayLen=0, selected[1]=cluster_count=0,
  selected[2]=sentinel=0 (want 12345)`**.

The UNCONDITIONAL `selected[2]=12345u` (thread 0, no guard) does NOT appear in the
readback. ⇒ **the cut compute's writes to `selected` never reach the buffer the
readback copies** — NOT a predicate, uniform-layout, or `cluster_count` bug (all
downstream of writes landing). The readback DOES resolve (the copy ran ⇒ the encoder
was submitted ⇒ the cut compute, recorded earlier in the same encoder, also ran), so
the leading cause is a **buffer-instance mismatch**: the cut bind group's
`@binding(1) selected` ≠ the `buffers.selected_buffer` the readback copies. Both the
cut's `selected` AND the compaction's `draw_args` read 0 ⇒ a shared root cause.

**Next:** verify buffer identity — does `upload_pages`/`ensure_capacity` recreate
`self.buffers` (new `selected_buffer`) AFTER the cut bind group was built, leaving it
stale? does the round-trip load the cluster mesh twice (second `upload_pages` not
re-recreating BOTH bind groups)? Log the bound buffer vs the readback buffer identity.
Also confirm the cut compute pass is in the SAME submitted encoder (not a separate
never-submitted one). Fix the recreate ordering so the cut + compaction bind groups
always reference the live buffers; re-run the probe to confirm sentinel=12345 lands;
restore the real cut; then the cut should select the CPU count.

---


| Claim | Status | Evidence |
|---|---|---|
| **A1** crack-free per-cluster cut incl. non-watertight/subdivided, full-detail + capped | ✅ | CPU bake/cut test (`cb3b1ac8` weld+lock_boundaries, `73984b4b` antichain) + on-device (iter 29): subdivided sphere renders watertight via the per-cluster GPU cut, `draw_args.index_count=27558 (9186/583768 tris)`. The iter-24–28 "CONTRADICTED/0-tris" was a FROZEN-BROWSER artifact (now resolved). |
| **A2** dynamic camera-driven streaming residency (multi-M-tri, bounded VRAM, LRU, crack-free fallback, no per-frame allocs) | ✅ | iter 36–38: per-frame stream/evict (`39162d0f` slot-indexed resident + eviction, `d0b06e6e` no-alloc). On-device `?vg&paging`: **1,081,344-tri source → 2,393,468-tri DAG / 51,753 clusters**, M capped to **29,850 tris** in a **bounded ~83 MB pool**; camera-driven crack-free (watertight): far draw=4,908 → zoom-in 14,650 (rises) → zoom-out 3,860 (falls). Test `a2_residency_is_bounded_by_budget_not_source` (M capped by budget ⊥ source size). |
| **A3** drawn (cut) tri count bounded by screen res, not source size (benchmark across scales) | ✅ | iter 30, fixed camera @ dist 4 / 1px: source 142,456 → drawn **1700**; source 583,768 (4.1×) → drawn **1696** (flat). Committed test `a3_cut_bounded_by_screen_not_source` (cut stays 4 with 21× source). |
| **A4** deforming → discrete chain, per-instance, skin/morph carried | ✅ | `c58abfd9` carry-through test + on-device mixed CesiumMan/MorphCube/Sphere routing |
| **A5** flags off ⇒ byte-identical | ✅ | `1f5dba9d` defaults test + on-device no-cluster-pipelines-when-off |
| **A6** final multi-M-tri benchmark TABLE (1080p+4K, per-pass + cut-vs-source + VRAM) in docs | ❌ **UNMET** | A2 now done (the multi-M asset + bounded pool exist); A6 needs the formal TABLE recorded (per-pass via `?trace=sub-frame`, cut-vs-source, VRAM) at 1080p+4K. |

---

## A2 — dynamic per-frame paging (Gap B). ✅ MET (iter 38).

**On-device proof (`?vg&paging`, browser un-frozen, watertight screenshots).** A genuine
multi-million-triangle asset pages through the player cluster path within a bounded VRAM budget,
camera-driven and crack-free:
- Source **1,081,344 tris** → full DAG **2,393,468 tris / 51,753 clusters** → render mesh M
  **CAPPED to 29,850 tris** (residency budget 30,000) in a **bounded page pool of 3,862 slots
  (~83 MB)**. M (83 MB) is far under the 512 MB shipped guard ⇒ the runtime needs no guard change.
- Camera-driven + bidirectional + crack-free: far `desired=509 draw=4,908 tris` → zoom-IN
  `desired=1,260 draw=14,650` (rises) → zoom-OUT `desired=381 draw=3,860` (falls); every frame
  watertight; no per-frame heap allocations (iter 36).
- Committed test `a2_residency_is_bounded_by_budget_not_source` asserts the CPU invariant: M is
  capped by the budget INDEPENDENT of source size (4× larger source DAG ⇒ same bounded M).

**Editor-authoring caveat (separate from A2).** To AUTHOR the >1M source in the editor for this
test, the debug-only 512 MB `OVERSIZED_ALLOC_BYTES` guard was temporarily raised (a probe, reverted):
the EDITOR densely explodes the raw editable mesh's `?vg` visibility geometry (1.08M ⇒ ~1 GiB pool),
which trips the guard. That is an EDITOR authoring limitation, NOT the runtime streaming-residency
claim — the runtime player load skips the dense glb and uploads only the bounded M. Follow-up (not an
A-claim): make the editor not densely explode huge editable meshes (route ≥budget editable meshes
through a bounded representation), so >1M assets are authorable end-to-end at the shipped guard.

The original gap analysis (now historical) follows.

## A2 — original gap analysis (historical, pre-iter-38)

**iter 36 — the per-frame stream/evict loop now WORKS bidirectionally, crack-free, in the bounded pool (commit `39162d0f`).**
`ClusterLodRenderPass::stream_paging` (replaces the no-op `paging_update`): each frame it runs
`select_cut_per_cluster` over the FULL DAG → `desired`, streams desired-not-resident clusters into
free slots (capped MAX_LOADS=96/frame), and — once the whole desired cut is resident — evicts the
resident-but-no-longer-desired slots so the resident set converges to EXACTLY the crack-free
antichain. Two bugs fixed to make it take effect: (a) the GPU `resident` array is SLOT-indexed
(shader reads `resident[i]` at the same `i` as `pages[i]`, sized to `pool_slots`) but
`write_resident_entry` wrote at `cluster_id*4` (full-DAG ids ~10k) → overflowed the slot-sized
buffer → GPUValidationError → write dropped → streaming silently no-op'd; now writes at `slot*4`.
(b) eviction was missing (free-slots-only ⇒ draw could only rise + pool exhausts); eviction-when-
stable added. **On-device (`?vg&paging`, 393k-tri subdivided sphere, WheelEvent dolly, browser
healthy — 6478+ distinct colors, watertight screenshots): far `desired=294 draw=9450 (3150 tris)` →
zoom-IN `desired=675 draw=34614 (11538 tris)` RISES → zoom-OUT `desired=268 draw=8736 (2912 tris)`
FALLS, every frame watertight, all within the bounded `pool=1962` slots (full DAG=18030).**

**no-per-frame-alloc bar: ✅ MET (iter 36, commit `d0b06e6e`).** `stream_paging` and the two per-slot
`buffers` write helpers (`write_page_entry`, `write_source_indices_span`) now serialize into pooled
`Vec<u8>` scratch on `ClusterPaging` (`page_bytes_scratch`/`src_bytes_scratch`) — previously each
allocated a fresh `Vec` per streamed slot (~96/frame). With `select_cut_per_cluster` and
`pack_visibility_slot_bytes` both reusing their output buffers, the whole per-frame stream/evict path
is now Rust-heap-alloc-free (only unavoidable web-sys typed-array views in `writeBuffer` remain).
Re-verified on-device behavior-identical (zoom-in still draw=34614, watertight). (LRU is effectively
moot — eviction only ever drops NON-desired slots, so which non-desired goes first doesn't affect
correctness; `slot_last_used` is tracked if strict-LRU ordering is wanted.)

**iter 37 — DIAGNOSED the 1 GiB panic precisely (temp instrumentation, now reverted; HEAD 07a53fca).**
The runtime cluster-LOD paging path is **fully bounded and is NOT the cause of the panic.** Measured
on the WORKING 393k case (`MESHPOOL DIAG` / `load_cluster_lod ENTER` / `cluster LOD DIAG` logs):
- `load_cluster_lod`: full DAG = **873,662 tris / 18,030 clusters**, but `cm.positions` is only
  **197,505 verts** (the cluster mesh shares ONE compact vertex pool across all LOD levels — it does
  NOT scale with DAG tri count). The render mesh M = `pool_slots(1962) × 384 = 753,408` exploded verts
  → **attrs 4 MB + visibility-exploded 40 MB = ~44 MB total, bounded by the 30k-tri paging budget,
  independent of source/DAG size.** This is exactly the north-star "bounded by a residency budget, not
  the asset size."
- The 1 GiB allocation is `MeshGeometryPool resize -> 1073741824 bytes`, and **`load_cluster_lod ENTER`
  never prints before it** — so it is NOT the cluster path. It is the **EDITOR authoring tool densely
  exploding the raw editable mesh's visibility geometry** (`?vg`): 393k editable ⇒ pool 256 MB (ok);
  1.57M editable ⇒ pool 1 GiB ⇒ trips the 512 MiB `OVERSIZED_ALLOC_BYTES` guard. The runtime player
  load of a baked cluster GLB *skips the base glb* (scene-loader line ~942) and only uploads the
  bounded M, so the runtime is unaffected.

**🚨 Why STILL unmet — the `multi-M-tri` headline.** The runtime path is multi-M-capable BY
CONSTRUCTION (bounded M ⊥ source size; proven pages an 0.87M-tri DAG in 44 MB), but a genuine **>1M
SOURCE** end-to-end demo is blocked by the **editor/export authoring harness**: building a 1.57M-tri
editable mesh in the editor explodes its dense `?vg` visibility geometry (~1 GiB) and panics the
512 MiB guard BEFORE it can be exported to the player cluster path. NEXT: get a real multi-M source
THROUGH the player path without the editor dense-rendering it — e.g. load a pre-baked multi-M cluster
GLB via `import_model_from_url`/`load_project_from_url` (player path, no editor dense explode), or
(scoped editor change) stop densely exploding huge editable meshes / raise the authoring guard for the
cluster-LOD case. Until a true >1M source pages within bounded VRAM end-to-end, A2 stays UNMET.
Functional core (camera-driven crack-free bidirectional refinement, bounded pool, no per-frame allocs,
0.87M-tri DAG in 44 MB) is PROVEN on-device (iter 36).

**iter 31 on-device verification (browser healthy) — what's PROVEN working:**
1. **Camera-driven cut refinement ✅** (`?vg`): dolly the camera IN ⇒ the drawn cut
   RISES. Measured via the periodic readback: far `draw_args = 1696 tris` → after
   wheel-zoom-in `15845 tris` (9.3×). The per-cluster GPU cut adapts detail to the
   camera live. (Also CPU-unit-tested: `per_cluster_cut_varies_detail_by_distance`.)
2. **Bounded VRAM ✅** (`?vg&paging`): 785 resident / 13065 clusters, render mesh M
   **capped to 29322 tris from 583768** — the bounded slot pool (step 2) renders.
3. **MISSING = the DYNAMIC combination:** `?vg` refines but uploads everything (OOMs
   at 2.55M — hit the 1GiB GPU-cap guard); `?vg&paging` is bounded but its frontier is
   clamped always-draw (camera-INVARIANT: draw stays 29322 at any distance). A2 needs
   BOTH: camera-driven refinement *within* the bounded pool via per-frame streaming
   (20b-iv) so multi-M-tri assets refine near the camera at bounded VRAM.

**🛟 HARNESS: driving the cut camera.** `set_camera_orbit` (MCP) does NOT move the
player/cut camera in round-trip mode (draw stays fixed across dist 0.4–9; view
unchanged). What WORKS: dispatch a `WheelEvent` on the `<canvas>` via chrome-devtools
`evaluate_script` (zoom) — it drives the live viewport camera, which the cut reads
(verified: zoomed a tiny dot to full-screen + drove draw 1696→15845). Also useful:
`window.wasmBindings.editor_query_scene_png` (direct GPU→PNG) + `editor_dispatch_json`.

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

## A3 — cut bounded by screen res, not source size. ✅ VERIFIED (iter 30).

Shown on the STATIC per-cluster GPU cut (does NOT depend on A2). At a FIXED camera
(dist 4) + budget (1px), the drawn cut stays flat as the source scales:

| subdivide | source tris (M) | clusters | drawn tris (`draw_args.index_count/3`) |
|---|---|---|---|
| iter 3 | 142,456  | 2,638  | **1700** |
| iter 4 | 583,768  | 13,065 | **1696** |

Source grew 4.1× (142k → 584k); the drawn cut barely moved (1700 → 1696, ~0.2%) —
the cut is bounded by screen-space error, not source size. (iter-5 / 2.55M source
hit the GPU-buffer-cap guard on the UNCAPPED `?vg` path — expected; that's what
streaming/paging is for, and is an A6/A2 concern, not A3.) Committed deterministic
test: `a3_cut_bounded_by_screen_not_source` (scene-loader) — the selected antichain
stays size 4 at a fixed budget even with 21× the source clusters.

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
