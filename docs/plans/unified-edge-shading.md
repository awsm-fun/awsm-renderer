# Unified edge shading — one kernel for interior + edge (kill the cs_edge split)

**Status: COMPLETE** (material-increase, 2026-06-17). All stages U0–U3 landed + GPU byte-parity verified
(max-diff 0) at every step; U4 updated `uber-shader.md` to reflect the outcome (commit 962cfd97). Composes
with the prep pass (`deferred-shared-prep-pass.md`) — does not depend on it.

**Outcome:** MSAA shading is now ONE `cs_shade` kernel per material pipeline (interior sample-0 →
`opaque_tex`; edge samples → per-sample `accumulator`, write-target branch OUTSIDE the coloring) +
`final_blend`. The legacy `cs_opaque`+`cs_edge`+`skybox_primary`+`skybox_edge_resolve`+`render_edge_resolve`
MSAA path, the per-bucket edge-sample lists, and the `with_unified_edge` toggle are all gone.
- **Pipelines/material under MSAA: 3 → 2** (cs_opaque [no-MSAA only] + cs_shade); the global
  skybox_edge_resolve pipeline removed.
- **Memory:** edge `data_buffer` ~56 MB → 36 MB (sample-list pool dropped, bucket=5/budget=524288);
  `args_buffer` 128 B → 32 B; `EdgeBufferLayout` uniform 10 u32 → 5.
- **Classify:** 4 `append_edge_sample` calls per edge pixel removed (atomic traffic).
- **Byte-parity:** MetalRoughSpheres + SheenChair (multi-material + self-shadow + sky edges), MSAA, prep
  ON and OFF — all max-diff 0 vs the pre-refactor baseline.

**Follow-up flagged for David (NOT done — out of scope, his call):** the MSAA opaque module still emits
`cs_opaque` (the no-MSAA interior entry) DEAD alongside `cs_shade` (render_shade dispatches only cs_shade
under MSAA). Gating `cs_opaque` to non-MSAA + dropping the unused MSAA `cs_opaque` pipeline build would
fully collapse the MSAA shader surface (and reverse the size-ceiling raise U3a took) — but it's
GPU-affecting pipeline surgery, so left for David. Also: a few now-dead `pub` offset helpers + the
`data_buffer` per-bucket count header region remain (harmless; kept so `edge_to_xy_base` doesn't shift).

---

**(original spec header)** Do this **before** `uber-shader.md` (it stands alone and shrinks
the surface the uber-shader later branches over).

## Motivation

Today MSAA shading is **two pipelines per material** — `cs_opaque` (primary/sample-0, dispatched over a
sample-0-keyed tile list → writes `opaque_tex`) and `cs_edge` (dispatched over a per-bucket compact
edge-**sample** list → writes the per-edge accumulator) — plus `final_blend` to resolve. The shading body
is the *same* in both; the **only** real difference is **where the color is written** (direct output vs an
accumulator slot). The split exists purely as a thread-packing micro-opt for the sparse edge work.

That micro-opt does not pay for itself. Analysis (David, 2026-06-17):
- **Interior-only material:** identical in both designs.
- **Edge+interior material (the common case):** unified rides edge work on the threads already running
  for sample 0; current spins a *separate* `cs_edge` dispatch (tail waste + per-bucket indirect-dispatch
  overhead — at 1024 buckets, ~1 near-empty workgroup *per bucket with any edge*).
- **Edge-only material (rare):** current packs tighter (entry-granular list vs tile-granular dispatch),
  but the "waste" in unified is only **redundant cheap reads + check-and-skip lanes** on sparse edge
  tiles that are *already being dispatched by the sample-0 owner anyway* — microseconds. It's the same
  mixed-tile overlap `cs_opaque` already accepts for primary samples.
- **Absent material:** identical (empty tile list → 0 workgroups).

So the split buys microseconds of packing and costs **2× pipelines per material + the entire per-bucket
edge-sample-list machinery (memory + classify atomic traffic) + a permanent divergent codepath to
maintain**. Net negative. Unify it.

**Crucially, the write-target is a branch OUTSIDE the coloring** — built-in and custom/dynamic shaders
just return a color for `(pixel, sample)`; the kernel decides output-vs-accumulator. So unifying costs
the shading code (and the custom-material contract) **nothing**.

## End state

**One** compute entry point per material (`cs_shade`, replacing `cs_opaque` + `cs_edge`):

```
dispatch over the bucket's ANY-sample tile list (8x8 tile = workgroup, 1 thread/pixel):
  let edge_id = edge_id_tex[pixel]                     // U32_MAX sentinel = not an edge pixel
  if edge_id == NONE {
      if this material owns sample 0 { write opaque_tex[pixel] = shade(pixel, sample 0) }
      // else: another material owns this pixel — skip (same as today's per-pixel shader_id guard)
  } else {
      for s in 0..msaa_count {
          if this material owns sample s { accumulator[edge_id*4 + s] = shade(pixel, sample s) }
      }
  }
resolve pass (over edge_to_xy): opaque_tex[xy] = average(accumulator[edge_id*4 + 0..4])
```

`shade(pixel, sample)` is the existing per-material color path, unchanged — incl. the prep `PrepReadContext`
(PRIMARY for sample 0, EDGE for edge samples) from `deferred-shared-prep-pass.md` 5a/5b.

## Design decisions

1. **Classify `tile_mask` becomes ANY-sample.** OR each of the 4 samples' bucket indices into `tile_mask`
   (today: sample 0 only, classify compute.wgsl line ~133/157). A bucket's tile list is then "tiles where
   it appears at *any* sample" — so an edge-only material's tiles are covered by its own `cs_shade`
   dispatch. Cost: 4× a cheap workgroup-shared atomicOr; the dense-material tile count barely changes.

2. **Per-pixel `edge_id_tex` replaces the per-bucket edge-sample lists.** Add a screen-sized `edge_id_tex`
   (R32uint, 1 word/pixel: the compact `edge_pixel_id` or `U32_MAX`). Classify writes it during edge
   detection, using the **EXACT same edge criteria the current path uses** (material-distinct across
   samples AND whatever depth/normal/coverage variance the current `cs_edge`/edge-mask path keys on —
   reproduce it verbatim, since byte-parity depends on shading per-sample for *exactly* the same pixels).
   The unified kernel reads `edge_id_tex` **once** at its pixel: `U32_MAX` → interior (shade sample 0 →
   output); else → edge (use it as the accumulator base). It carries **both** is-edge and the accumulator
   index — the kernel does NOT re-derive edge-ness (it needs the compact index from classify regardless,
   so one read is strictly better than recomputing). No reverse lookup, no per-bucket lists. **DROP:**
   `append_edge_sample` + the per-bucket sample lists + their per-bucket indirect args + `edge_slot_map`
   (see #3). Memory: the sample lists were `bucket_count × sample_entries_per_bucket` (large at 1024
   buckets); `edge_id_tex` is one screen-sized R32uint (~33 MB @4K — trade a per-bucket-scaling buffer for
   a fixed screen-sized one; net win at high bucket counts, wash at low. `edge_pixel_id` needs ~20 bits
   (512K budget); could pack into spare bits of an existing per-pixel texture later to reclaim the 33 MB).

3. **Accumulator becomes PER-SAMPLE, not per-material.** `accumulator[edge_id*4 + sample] = that sample's
   shaded color`. Each sample has exactly one owning material, which writes it; resolve averages the 4
   (coverage-weighted as today). This **drops `edge_slot_map`** (no material→slot search — the slot *is*
   the sample index) and is simpler + correct for multi-material edges. Same 4-slot footprint as today.

4. **Skybox participates as a normal bucket.** Sky/uncovered samples at an edge pixel must land in the
   accumulator too (for correct edge blend against sky). The skybox bucket's `cs_shade` writes its owned
   (uncovered) samples; at non-edge sky pixels it writes `opaque_tex` directly (the current
   `skybox_primary` behavior). Fold `skybox_primary` + `skybox_edge_resolve` into this uniform model.

5. **Resolve (`final_blend`) stays**, simplified to the per-sample accumulator: over `edge_to_xy`, average
   the 4 sample slots → `opaque_tex[xy]`. Keep its indirect dispatch over `edge_count`.

6. **Invariant (no clear needed):** every sample of every edge pixel has exactly one owning bucket whose
   `cs_shade` writes it (sky included), so the accumulator is fully written each frame — same invariant the
   current accumulator relies on.

7. **Composes with prep.** `cs_shade` reads prep arrays (sample 0, PRIMARY) + the 5b-shadow edge-shadow
   buffer (edge samples, EDGE) via the existing `PrepReadContext`. **5b-shadow is NOT wasted** — its
   `cs_prep_edge` + compact edge-shadow buffer become "the edge-sample shadow source the unified kernel
   reads." The unification only collapses the *consumer* (two kernels → one); prep is orthogonal. Works
   with prep OFF too (the RECOMPUTE path).

8. **Efficiency (David: "I want an efficient renderer"):** shading is done **exactly once per (sample,
   owning material)** — same as today; the unification removes dispatch/memory overhead, not shading work.
   In `cs_shade`: (a) **read the per-sample visibility ONCE** (the 4 sample loads needed for ownership) and
   reuse them for shading — do not re-fetch per sample; (b) the interior path shades **only sample 0** (no
   per-sample loop), so the per-sample loop runs only at the sparse edge pixels; (c) skybox's bucket
   kernel is templated lean (cubemap sample + write-branch — no lighting/prep includes), so folding it in
   costs nothing per sample while removing 2 pipelines. The residual cost vs today is a sliver of warp
   divergence at edge-straddling tiles — far below the dropped per-bucket `cs_edge` dispatch + edge-list
   overhead. (The remaining "N material dispatches sweep shared tiles" redundancy is the uber-shader's to
   remove — this refactor sets that up; see "Relationship to uber-shader".)

## Stages (each its own commit; each BYTE-PARITY gated vs the pre-refactor baseline)

**Capture the baseline FIRST** (before any change): screenshot MSAA-ON renders of several models incl.
multi-material silhouettes (SheenChair, MetalRoughSpheres, a 2-material scene), with prep OFF *and* ON, at
the current HEAD. Every stage below must reproduce these **byte-identically** (exclude the UI sidebar).

- **U0 [DONE] — baseline + scaffolding.** `with_unified_edge(bool)` toggle (default false, threaded like
  PrepPassConfig); `edge_id_tex` (R32Uint screen texture, gated alloc); classify (gated `{% if unified_edge %}`)
  writes the compact edge_pixel_id / U32_MAX sentinel into edge_id_tex during the existing edge-detection
  block (criteria unchanged) + builds an any-sample `tile_mask`; the old sample-0 path + edge-sample lists
  + cs_opaque/cs_edge are untouched. Inert (edge_id_tex unread). 259+34 green; default render GPU-verified
  BYTE-IDENTICAL to the HEAD baseline (MetalRoughSpheres). Note: false-path classify WGSL differs from HEAD
  only in whitespace (askama `minimize` artifact) — behaviorally inert, naga-validated both toggle states.
- **U1 [DONE] — unified kernel, behind a toggle.** `cs_shade` added (one entry, MSAA-only) merging
  cs_opaque interior + cs_edge edge (reusing shade_sample + the per-material accumulator/edge_slot_map +
  final_blend verbatim) driven by edge_id_tex + the any-sample tile dispatch; skybox folded as a lean
  cs_shade arm (cubemap interior + sky-edge accumulate); a per-bucket cs_shade pipeline + a toggle-gated
  render_shade dispatch path, coexisting with the old cs_opaque/cs_edge/skybox_primary/skybox_edge_resolve.
  261+34 green. **GPU byte-parity VERIFIED (max-channel-diff 0):** toggle-off == U0 anchor (old path
  intact); toggle-on cs_shade == toggle-off on MetalRoughSpheres/SheenChair/MultiUv (prep off) incl
  multi-material + sky edges; and on SheenChair (prep on — prep arrays + edge-shadow buffer). MSAA-off uses
  the old path by construction. ORIGINAL SKETCH BELOW (superseded by this DONE line):
- **U1 (orig) — unified kernel, behind a toggle.** Add `cs_shade` as one new entry point merging cs_opaque
  (interior, sample 0 → opaque_tex) + cs_edge (edge, per-sample) into one body, driven by `edge_id_tex` +
  the any-sample tile dispatch (NOT the edge-sample lists). **CORRECTIONS for byte-parity (vs the original
  sketch):**
  - **Skybox is folded in U1, not U2.** A normal model silhouette is a *sky* edge, so sky-edge byte-parity
    is impossible unless the skybox bucket also goes through `cs_shade`. So U1 builds the COMPLETE unified
    path incl a skybox `cs_shade` arm (interior sky → opaque_tex; sky edge samples → accumulator). The old
    `skybox_primary`/`skybox_edge_resolve` stay for the toggle-OFF path; U2 deletes them.
  - **REUSE the existing per-material accumulator + `edge_slot_map` + `final_blend` resolve EXACTLY** (do
    NOT switch to a per-sample accumulator in U1). `cs_shade`'s edge path writes a material's owned samples
    into that material's accumulator slot (via `edge_slot_map`) identically to today's `cs_edge`, and the
    unchanged `final_blend` resolves it. This keeps the resolve byte-identical (the hard part) and
    minimizes new code. The per-sample-accumulator + `edge_slot_map` removal (spec design #3) is an
    OPTIONAL later cleanup — it is NOT required for the unification's wins (dropping the `cs_edge`
    dispatch + the per-bucket edge-sample *lists*); defer it (or skip).
  - Wire a parallel dispatch path (cs_shade + reuse final_blend) selectable by the toggle, WITHOUT removing
    cs_opaque/cs_edge/skybox_primary/skybox_edge_resolve. Toggle-ON GPU-verify BYTE-IDENTICAL vs toggle-OFF
    across the full matrix (MSAA on/off, prep on/off, all models, multi-material edges, sky edges). Hard
    gate — iterate.
- **U2a [DONE] — flip the default ON.** `with_unified_edge` now defaults `true`, so the default build
  shades through `cs_shade` + `final_blend` under MSAA (legacy cs_opaque+cs_edge+skybox_primary+
  skybox_edge_resolve now reachable only via `with_unified_edge(false)`, kept for A/B until U2b). Two-line
  change: builder default `false`→`true`; edge_id_tex alloc now gated `unified_edge && msaa` (every other
  unified gate was already MSAA-safe — classify's `unified_edge && emit_edge_data` with `emit_edge_data ≡
  MSAA`; material_opaque's edge_id_tex@12 nested under `{% if multisampled_geometry %}`; the cs_shade
  pipeline build early-returns under no-MSAA). 261+34 green. **GPU byte-parity VERIFIED (max-diff 0, 0
  pixels):** default build (cs_shade) == legacy baseline anchors on MetalRoughSpheres (silhouette) and
  SheenChair (multi-material fabric+wood + self-shadow + sky edges), MSAA, prep off.
- **U2b — delete the legacy MSAA kernels** (sub-staged for safety; the only GPU-affecting steps are the
  dispatch flip (U2b-1) and the buffer-offset shifts (U2b-3) — the pipeline-build deletion (U2b-2) is pure
  dead-code removal since the legacy pipelines are no longer dispatched):
  - **U2b-1 [DONE] — dispatch flip.** render.rs: under MSAA always dispatch `render_shade` (dropped the
    `unified_edge &&` condition); no-MSAA → `render()` (cs_opaque + skybox_primary). Deleted the orphaned
    `render_edge_resolve` method (its only caller was the legacy MSAA branch). The legacy
    cs_edge/skybox_edge_resolve pipelines still COMPILE but are never dispatched (dead — removed in U2b-2).
    261+34 green. **GPU byte-parity VERIFIED (max-diff 0, 0 pixels):** SheenChair MSAA prep-off == baseline
    (default-build cs_shade dispatch is byte-identical to U2a by construction; this confirms the
    render_edge_resolve deletion didn't perturb it).
  - **U2b-2 [DONE] — delete the dead legacy pipeline build + entry points.** Removed the cs_edge per-shader
    pipeline build + skybox_edge_resolve pipeline + their layout/cache keys (`edge_resolve_*` +
    `skybox_edge_*` layout keys, `per_shader` map, `EdgePipelineSlot::PerShader`/`::Skybox`,
    `CompileInstallTarget::EdgeResolvePerShader`/`EdgeResolveSkybox`, `PassDef`/`PassKind::EdgeResolveSkybox`,
    `ShaderCacheKeyMaterialSkyboxEdgeResolve`) + scheduler-launch/install arms + the `cs_edge` entry point
    (material_opaque compute.wgsl) + deleted skybox_edge_resolve.wgsl + skybox_edge_bind_groups.wgsl.
    Simplified `build_edge_bind_groups` → returns only the final_blend group. Pure dead-code (not
    dispatched). Kept cs_opaque + skybox_primary (no-MSAA) + cs_shade + final_blend + accumulator +
    edge_slot_map. 14 files touched + 2 WGSL deleted; warning-free build, 261+34 green. **GPU byte-parity
    VERIFIED (max-diff 0, 0 pixels):** SheenChair (multi-material + sky edges, now ALL through cs_shade) and
    MetalRoughSpheres == baseline, MSAA prep-off. (cs_edge + cs_shade share compute.wgsl, so the module text
    changed + recompiled — cs_shade output unchanged.) **Pipelines/material under MSAA: 3 → 2** (cs_opaque +
    cs_edge + cs_shade → cs_opaque [no-MSAA only, still compiled] + cs_shade); skybox_edge_resolve global
    pipeline gone.
  - **U2b-3 [DONE] — drop the dead edge-sample-list machinery (memory + classify-atomic win).** Removed
    `append_edge_sample` (fn + its 4 call sites) from classify compute.wgsl, and truncated the `data_buffer`
    to end right after the accumulator (`data_buffer_bytes = accumulator_offset + accumulator_bytes`) —
    dropping the per-bucket + skybox sample-list pool. **Minimal-risk approach: the kept-region offsets did
    NOT shift** — the counter-mirror header + edge_to_xy + edge_slot_map + accumulator keep byte-identical
    offsets, so cs_shade/final_blend read exactly the same locations (byte-parity is near-trivial, not
    offset-dependent). The 5 now-dead `EdgeBufferLayout` uniform/struct fields (per_shader_count_base,
    skybox_count_index, per_shader_sample_list_base, skybox_sample_list_base, sample_entries_per_bucket —
    mirrored in 4 WGSL files: material_opaque/final_blend/classify/material_prep) + the dead args_buffer
    skybox/per_shader indirect slots now address offsets past the shorter buffer but are READ BY NOBODY
    (append was their only consumer); they are removed in U3 with the toggle (deferred to avoid the 4-way
    struct-mirror lockstep risk in this commit). 261+34 green (updated the U0-era wgsl_validation assertion
    that required append_edge_sample to KEEP → now asserts it is REMOVED + edge_slot_map_base remains).
    **GPU byte-parity VERIFIED (max-diff 0, 0 pixels):** MetalRoughSpheres + SheenChair prep-OFF == baseline;
    SheenChair prep-ON == prep-off baseline (validates prep + U2b-3 together). **Memory: data_buffer
    ~56 MB → 36 MB** at bucket_count=5 / max_edge_budget=524288 (the ~20 MB sample-list pool dropped; scales
    with sample_entries_per_bucket × (buckets+1)). **Classify atomic traffic: 4 append_edge_sample calls per
    edge pixel removed** (each up to ~2 atomicAdd + 1 atomicStore + per-bucket/skybox count atomicAdds).
    **This completes U2.**
- **U2 (orig) — flip + delete.** Make `cs_shade` the only path: classify emits any-sample `tile_mask` +
  `edge_id_tex` only; drop `append_edge_sample` + per-bucket edge-sample lists + their per-bucket indirect
  args; delete `cs_opaque`/`cs_edge`/`skybox_primary`/`skybox_edge_resolve` entry points + their pipelines.
  Keep `final_blend` + `edge_slot_map` + the accumulator (still used by `cs_shade`/resolve). Re-verify
  byte-parity. Measure: pipelines-per-material halved, classify memory/atomic traffic dropped, MSAA shader
  surface collapsed. (The per-sample-accumulator + `edge_slot_map` removal can be a separate optional
  follow-up after this lands.)
- **U3 — cleanup** (sub-staged):
  - **U3a [DONE] — remove the `with_unified_edge` toggle.** Deleted `with_unified_edge` + the `unified_edge`
    field from AwsmRenderer/builder + its threading through 25 files (bind_groups, picker, anti_alias,
    textures, render_textures, render_passes, render.rs RenderContext + recreate ctx, both classify +
    opaque pipeline/cache_key/template, edge_pipeline, launch, shader_completeness, wgsl_validation).
    Collapsed all 7 WGSL gates: `{% if unified_edge && multisampled_geometry %}` → `{% if multisampled_geometry %}`,
    `{% if unified_edge && emit_edge_data %}` → `{% if emit_edge_data %}`, and the bare `{% if unified_edge %}`
    wrappers (edge_id_tex sentinel + compact write in classify; edge_id_tex@12 binding + cs_shade entry in
    opaque; skybox cs_shade arm) → unconditional within their MSAA-gated parents. edge_id_tex alloc + cs_shade
    pipeline build now gated purely on MSAA. Behavior-preserving (the collapsed gates were already
    always-true under MSAA, which is the only path). Dropped the now-impossible `unified_off_opaque_wgsl_unchanged`
    test (260+34 green). **GPU byte-parity VERIFIED (max-diff 0, 0 pixels):** MetalRoughSpheres + SheenChair
    MSAA prep-off == baseline. **size_regression ceilings raised** (EMPTY_MSAA4_MIPS 88K→94K, ALL 120K→132K):
    the old ceilings measured the dead toggle-OFF module (cs_opaque + cs_edge); the shipping MSAA module is
    cs_opaque + cs_shade (cs_shade is a larger merged kernel). NOTE: the MSAA module still carries `cs_opaque`
    (the no-MSAA interior entry) DEAD alongside cs_shade — gating cs_opaque to non-MSAA + dropping the
    unused MSAA cs_opaque pipeline would further collapse the MSAA shader surface (efficiency follow-up,
    flagged for David; out of U3 scope).
  - **U3b [DONE] — remove the dead EdgeBufferLayout fields + args slots** (the 5-way struct lockstep deferred
    from U2b-3). Dropped per_shader_count_base / skybox_count_index / per_shader_sample_list_base /
    skybox_sample_list_base / sample_entries_per_bucket from the Rust `build_edge_layout_uniform_bytes` builder
    AND all FOUR WGSL `EdgeBufferLayout` mirrors (material_opaque, final_blend [RO], classify, material_prep)
    in lockstep → **uniform 10 u32 → 5** (max_edge_budget, edge_count_index, edge_to_xy_base, edge_slot_map_base,
    accumulator_base). The kept `*_base` VALUES are unchanged (`data_header_bytes` + the offset fns left as-is,
    so the kept data_buffer regions did not move) — only the uniform slots compacted. Also dropped the dead
    args_buffer skybox_edge_args + per_shader_edge_args slots (classify `EdgeArgsBuffer` WGSL struct +
    `args_buffer_bytes` + `write_args_header`) → **args_buffer (2+B) slots → 1 (final_blend only); 128 B → 32 B**
    at bucket=5; final_blend_args stays at byte 16. 260+34 green. **GPU byte-parity VERIFIED (max-diff 0, 0
    pixels):** MetalRoughSpheres + SheenChair prep-off == baseline; SheenChair prep-on == prep-off baseline
    (validates the material_prep mirror lockstep too). Liveness confirmed via the alloc log (args_buffer=32 B,
    was 128 B). NOTE: a few now-dead `pub` offset helpers (data_per_shader_count_offset, data_skybox_count_offset,
    sample_entries_offset, skybox_sample_list_offset, sample_entries_per_bucket, skybox_edge_args_offset,
    per_shader_args_offset) + the data_header per-bucket count region remain (harmless, kept so edge_to_xy_base
    doesn't shift; sweepable later). **This completes U3.**

## Testing strategy (David: "tested carefully")

- **Byte-parity is the gate at every stage** — the refactor changes *structure*, not pixels. Diff the 3D
  viewport (exclude sidebar x<215) vs the U0 baseline; require max-channel-diff 0 (modulo any documented
  resolve rounding — investigate if nonzero). Rule out reload non-determinism (diff a state vs itself).
- **Cover the matrix:** MSAA **on** (the whole point) AND off; prep **on** AND off; models with: simple
  silhouettes (MetalRoughSpheres), self-shadow + edges (SheenChair), multi-UV (MultiUv), and a
  **multi-material edge** (two materials meeting at a silhouette — find/load one; this exercises the
  per-sample accumulator across materials, the riskiest case). Skybox edges (model against sky).
- **naga** every config; `cargo test -p awsm-renderer -p awsm-materials --lib` green each commit.
- The U1 toggle lets you A/B old-vs-new in the *same* build for direct comparison before deleting the old
  path — use it.

## Risks / careful points

- **Multi-material edge correctness** (per-sample accumulator across ≥2 materials whose dispatches both
  write the same edge pixel's different slots) — the core new path. Test explicitly.
- **Skybox-at-edge** blend (sky samples must reach the accumulator) — easy to drop; test a model silhouette
  against the sky.
- **Coverage/resolve weighting** must match the current `final_blend` exactly (averaging, partial coverage).
- **`edge_id_tex` is screen-sized** — confirm the memory trade vs the dropped per-bucket lists is
  acceptable (net win at high bucket counts). Consider packing later.
- **The any-sample `tile_mask`** must not change interior output (only adds edge-only tiles to lists).
- This is the renderer's most fragile area; the U0/U1 toggle + byte-parity-vs-baseline discipline is the
  safety net. Do NOT delete the old path (U2) until U1 parity is rock-solid.

## Relationship to uber-shader

Independent and first. After this, edge handling is "one kernel, write-target branch" — so when the
uber-shader later merges *materials* into one branching pipeline, the same write-branch simply lives in the
shared kernel; there is no separate edge story to also merge. This refactor removes the edge surface the
uber-shader would otherwise have to account for.
