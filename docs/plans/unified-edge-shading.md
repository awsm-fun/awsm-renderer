# Unified edge shading — one kernel for interior + edge (kill the cs_edge split)

**Status:** implementation-ready spec. Do this **before** `uber-shader.md` (it stands alone and shrinks
the surface the uber-shader later branches over). Composes with the prep pass
(`deferred-shared-prep-pass.md`) — does not depend on it.

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
  - **U2b-2 — delete the dead legacy pipeline build + entry points.** Remove the cs_edge per-shader pipeline
    build + skybox_edge_resolve pipeline + their layout/cache keys + scheduler-launch entries + the `cs_edge`
    entry point (material_opaque compute.wgsl) + skybox_edge_resolve.wgsl + skybox_edge_bind_groups.wgsl +
    edge_template/edge_cache_key entries for them. Pure dead-code (not dispatched). Keep cs_opaque +
    skybox_primary (no-MSAA) + cs_shade + final_blend. Verify still compiles + renders == baseline.
  - **U2b-3 — buffer-layout surgery (memory win).** Drop `append_edge_sample` + the per-bucket + skybox
    sample-list regions + per_shader/skybox indirect args from classify compute.wgsl + edge_buffers.rs
    `EdgeBufferLayout` (per_shader_count_base, skybox_count_index, per_shader_sample_list_base,
    skybox_sample_list_base, sample_entries_per_bucket) + the edge_layout uniform + bind groups. Keep:
    edge_count, edge_to_xy, edge_slot_map, accumulator (+ its clear), final_blend args, edge_overflow_count.
    The kept-region offsets shift — classify (writer) + cs_shade + final_blend all read them via the
    edge_layout uniform, so parity holds iff the layout calc + upload stay consistent. GPU-verify offsets
    (MSAA, prep on+off, all models). Measure classify atomic/memory traffic dropped + record in spec.
- **U2 (orig) — flip + delete.** Make `cs_shade` the only path: classify emits any-sample `tile_mask` +
  `edge_id_tex` only; drop `append_edge_sample` + per-bucket edge-sample lists + their per-bucket indirect
  args; delete `cs_opaque`/`cs_edge`/`skybox_primary`/`skybox_edge_resolve` entry points + their pipelines.
  Keep `final_blend` + `edge_slot_map` + the accumulator (still used by `cs_shade`/resolve). Re-verify
  byte-parity. Measure: pipelines-per-material halved, classify memory/atomic traffic dropped, MSAA shader
  surface collapsed. (The per-sample-accumulator + `edge_slot_map` removal can be a separate optional
  follow-up after this lands.)
- **U3 — cleanup.** Remove the toggle + any dead edge_buffers fields (sample lists, slot_map). Update
  size_regression + naga tests. Final byte-parity sweep.

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
