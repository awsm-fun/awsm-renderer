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

2. **Per-pixel `edge_id_tex` replaces the per-bucket edge-sample lists.** Classify already detects edge
   pixels (≥2 distinct sample materials) and allocates a compact `edge_pixel_id` (`edge_to_xy`). Add a
   screen-sized `edge_id_tex` (R32uint, 1 word/pixel: the `edge_pixel_id` or `U32_MAX`). The unified kernel
   reads it directly at its pixel — no reverse lookup, no per-bucket lists. **DROP:** `append_edge_sample`
   + the per-bucket sample lists + their per-bucket indirect args + `edge_slot_map` (see #3). Memory win:
   the sample lists were `bucket_count × sample_entries_per_bucket` (large at 1024 buckets); `edge_id_tex`
   is one screen-sized R32uint (~33 MB @4K — note: trade a per-bucket-scaling buffer for a fixed
   screen-sized one; at high bucket counts this is a net win, at low counts a wash. Could pack into an
   existing channel later if it matters).

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

## Stages (each its own commit; each BYTE-PARITY gated vs the pre-refactor baseline)

**Capture the baseline FIRST** (before any change): screenshot MSAA-ON renders of several models incl.
multi-material silhouettes (SheenChair, MetalRoughSpheres, a 2-material scene), with prep OFF *and* ON, at
the current HEAD. Every stage below must reproduce these **byte-identically** (exclude the UI sidebar).

- **U0 — baseline + scaffolding.** Capture baselines. Add `edge_id_tex` (R32uint screen texture, gated/
  allocated; classify writes it during edge detection, alongside the existing `edge_to_xy`). Add the
  any-sample `tile_mask` **behind a build-time toggle** so the old sample-0 path still runs by default.
  Inert; old pipelines still used. Verify no change.
- **U1 — unified kernel, behind a toggle.** Add `cs_shade` as a new entry point that does the full
  interior+edge logic (per-sample accumulator). Wire a parallel dispatch path + resolve that uses it,
  selectable by the toggle, WITHOUT removing `cs_opaque`/`cs_edge`. With the toggle ON, GPU-verify
  byte-parity vs the baselines (MSAA on, prep on AND off, all models, multi-material edges). This is the
  hard parity gate — iterate here.
- **U2 — flip + delete.** Make `cs_shade` the only path: classify emits any-sample `tile_mask` +
  `edge_id_tex` only; drop `append_edge_sample` + per-bucket edge-sample lists + their indirect args +
  `edge_slot_map`; delete `cs_opaque`/`cs_edge`/`skybox_primary`/`skybox_edge_resolve` entry points + their
  pipelines; resolve reads per-sample accumulator. Re-verify byte-parity. Measure: pipelines-per-material
  halved, classify memory/atomic traffic dropped, MSAA shader surface collapsed.
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
