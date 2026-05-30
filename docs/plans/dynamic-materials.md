# Materials system overhaul — many small opaque materials, first-class

**Status**: in implementation. Every open design question is resolved
(see **Locked decisions**). This file is the operational handoff +
live tracker.

### Implementation status (2026-05-30, branch `dynamic-materials-plan`)

Foundational + verifiable phases landed; the deep shader-rewrite core
remains. Branch is green (renderer suite 153 pass, clippy clean).

| Phase | State | Commit | Verification |
|---|---|---|---|
| F.0 baseline fixture + numbers | ✅ done | `f067c41` | captured in real Chrome |
| D.5 registry regression tests | ✅ done | `c6593fb` | 9 native tests pass |
| B.4 extensible bucket cap (`MAX_BUCKET_WORDS`) | ✅ done | `45932d0` | real Chrome: n_words 1≡baseline, 2 renders clean |
| D.1 transactional `register_materials` + `validate_batch` | ✅ done | `4c5e1da` | 6 native tests pass |
| A.2 `compile_progress()` aggregate (pull half) | ✅ partial | `a01fcd9` | compiles; native |
| B.2 layer 1: `PbrFeatures` derive + feature-hash | ✅ done | `c1c37d2` | 5 native tests |
| B.2 step A: thread `pbr_features` through opaque cache key + template | ✅ done | `3916324` | renders identical (no-op at `all()`) |
| B.2 step B: gate sheen+clearcoat brdf lobes (opaque/edge/transparent) | ✅ done | `d21ff92` | **pixel-identical** at `all()` (checksum `2985139072`) |
| B.2 step C: pass scene feature-union to activate stripping | ⬜ todo | — | needs consistent union across 4 PBR cache-key sites |
| A.1 compile-engine unification | ⬜ todo | — | — |
| A.2 phase-tagged `StatusEvent`s (push half) | ⬜ todo | — | — |
| D.2 transaction boundary (one final-layout reconcile) | ⬜ todo | — | — |
| D.3 `compile_materials(set).await` warmup | ⬜ todo | — | — |
| B.2 step B: gate the PBR WGSL on `pbr_features` | ⬜ todo | — | **verification-blocked** (see below) |
| B.2 step C: pass the scene feature-union | ⬜ todo | — | depends on step B |
| B.1/B.3 on-demand + per-feature-set buckets + routing | ⬜ todo | — | the architectural core |
| C GLTF auto-minimize + `AWSM_material_none` | ⬜ todo | — | depends on B |
| E transparency uber wiring | ⬜ todo | — | depends on B.2 |
| F.1/F.2/F.3 benchmarks + re-measure + extension dropdown | ⬜ todo | — | after B/C/E |

### ⚠️ KEY FINDING — the uber PBR shader already runtime-guards almost everything

While implementing B.2 the actual `brdf.wgsl` was read end-to-end. The
result reshapes the expected payoff of the whole opaque-specialization
effort:

**The uber PBR shader already self-optimizes via runtime `if` guards for
nearly every extension** — iridescence (`if color.iridescence > 0.0`),
anisotropy (`if color.anisotropy_strength != 0.0`, with an explicit "we
don't pay for tangent math on every shading point" comment),
diffuse_transmission, clearcoat-IBL, and the expensive part of sheen-IBL
all skip themselves when the feature is absent. The `material_color_calc`
texture samplers likewise all `if exists` guard. **The ONLY extension
lobes that run unconditionally per shading point are sheen + clearcoat in
the *direct* lighting path.**

Consequence: **per-feature compile-time specialization buys far less
per-frame GPU time than the "skip all unused extension code" framing
assumed** — the runtime guards already capture most of it. The genuine
per-frame win is (a) the sheen+clearcoat direct lobes (now gateable, see
B.2 step B), and (b) shader binary size / compile time. The broader
"many small buckets" value is the visibility-buffer batching + compile
parallelism, **not** dramatically cheaper per-pixel shading. Re-weight
B.3's cost/benefit accordingly before committing to the full
per-feature-set bucket routing.

**What landed + how it was verified.** `PbrFeatures` (layer 1), the
cache-key/template plumbing (step A), and the sheen+clearcoat brdf gating
across all three `brdf.wgsl` includers (step B) are done and **pixel-
verified in real Chrome**: at `all()` the gates are all-on and
`tuning-50-materials` reproduces the baseline checksum `2985139072`
exactly — the risky shared-lighting change is a proven no-op. Step C
(pass the scene feature-union so sheen/clearcoat actually strip) is the
remaining activation; it must keep the union consistent across all four
PBR opaque cache-key sites (lib.rs prewarm, pipeline.rs builder,
launch.rs ×2) or a key mismatch drops the pipeline.

**Measurement** (per owner): the metric that matters is **overall FPS on
the many-materials scene**, measured directly via rAF frame-counting
(no GPU-timestamp wiring needed) — but the scene must be made GPU-bound
(higher res / more instances) first, since at 1308×759 it is vsync-capped
at 60 fps and shading-cost deltas won't show.

**Thesis**: our biggest performance advantage is the visibility-buffer +
per-bucket classify/shade architecture, which makes *many small opaque
materials in one view* cheap. Everything below leans into that:

- Specialize opaque shaders down to exactly the features a material uses
  — unused features emit **no code** (no dead branches).
- Make every distinct opaque feature-set its own small bucket, keyed by a
  unique `shader_id` (the existing sole bucket discriminator).
- Compile only what a scene needs, on demand, batched for maximum
  concurrency, with granular progress reporting.
- Treat transparency as a *separate* budget: no visibility-buffer
  benefit, order-sensitive, so it stays an uber-shader.

When the work is done, **the last commit deletes this file.** The
architecture lives in the module docs + tests; this plan has no role
once shipped.

---

## Locked decisions

Resolved with the project owner. These are **not** open questions —
implement to them directly.

### Architecture

1. **Feature-specialized PBR → its own bucket per feature-set.** A
   distinct opaque PBR feature-set (e.g. `base+normal+metallic_roughness`)
   is its own small opaque bucket / `shader_id`, routed by classify to
   its own tiny specialized pipeline.
2. **`shader_id` stays the sole bucket key.** Code confirms `shader_id`
   is the first u32 of every material payload, classify routes on it
   ([`material_classify_wgsl/compute.wgsl:86`](../../crates/renderer/src/render_passes/material_classify/shader/material_classify_wgsl/compute.wgsl#L86)),
   and opaque pipelines are already per-`shader_id`. **No payload-format
   change, no secondary discriminator.** Each opaque feature-set gets its
   own `shader_id`.
3. **Per-feature-set `shader_id`s come from the existing dynamic
   registry**, keyed by a **deterministic feature-hash** so the same
   feature-set always resolves to the same `shader_id`/bucket within a
   build (one bucket per distinct feature-set, shared across all materials
   that use it). PBR's canonical base id
   ([`MaterialShaderId::PBR`](../../crates/materials/src/shader_id.rs)) is
   the **full-feature / force-uber** variant.
4. **Specialize PBR *and* Toon.** Unlit and Flipbook stay single-bucket
   (already minimal).
5. **Any feature-set is its own bucket; exceeding the active-bucket
   budget is a HARD ERROR** (no silent uber fallback). This pairs with
   (6): if you hit the cap, widen it.
6. **The bucket cap is an Askama template variable** (`N_WORDS`), default
   **32** (one `atomic<u32>` word), **trivially bumped to 64+** by raising
   `N_WORDS` — a near-zero-cost change (see Workstream B.4).

### glTF

7. **Skip-PBR is a per-material custom glTF extension named
   `AWSM_material_none`.** Per-material only (no whole-file flag).
8. **`AWSM_material_none` primitives render via a single shared flat/unlit
   bucket** (one bucket regardless of count; visible, cheap, no PBR
   compile).
9. **Standard glTF (no extension): auto-minimize by default.** Derive the
   minimal opaque feature-set from the material's actual content and route
   to its specialized bucket. A **global renderer config flag**
   (working name `pbr_specialization: Auto | ForceUber`) forces the full
   uber-PBR bucket for compat/debugging.
10. **Transparency always forces uber.** Any PBR material that renders in
    the transparency pass — `alpha_mode == Blend`, `alpha_mode == Mask`
    (cutoff/discard), or transmission — uses the single full-feature uber
    transparent PBR pipeline, never a specialized bucket. Opaque
    specialization is opaque-only.

### API & lifecycle

11. **One batch registration method, rich result.**
    `register_materials(Vec<MaterialRegistration>)` returns a result
    carrying both `shader_id`s **and** readiness handles per item. The
    single-item `register_material` becomes a thin wrapper.
12. **Batches are all-or-nothing / transactional.** Any invalid entry
    (duplicate name, would exceed budget, malformed layout) rejects the
    whole batch; nothing is registered or compiled.
13. **One async scene-load method**: `compile_materials(set).await`
    registers the final set and resolves only when **all** pipelines are
    `Ready`. Built on `prewarm_pipelines` internally.
14. **Progress: aggregate query + phase-tagged events.** Add
    `compile_progress() -> {total, done, failed, in_flight}` (pull, for
    progress bars) AND tag status events with a job phase
    (classify/opaque/edge/transparent) (push, for per-phase breakdown +
    bottleneck diagnosis).
15. **Transparent PBR is always uber; no runtime-branching tunability
    knob.** Specialization is template/compile-time only.
16. **Transparent custom/dynamic materials stay per-material
    forward-rendered, as today** — a separate budget from the opaque
    bucket architecture, not bucketed or specialized.

### Testing & verification

17. **Logic tests are native `#[test]`** (matches the existing ~138
    tests, run via `cargo test --all-features` in CI). Bucket-count,
    idempotency, cache-refresh, and feature-hash routing are all non-GPU
    and tested natively.
18. **GPU verification uses the scene-editor frontend in real Chrome via
    the Claude-in-Chrome plugin**, following
    [`docs/DEBUGGING-PREVIEW.md`](../DEBUGGING-PREVIEW.md) (NOT the in-app
    preview — it crashes on heavy WebGPU scenes). Create test scenes as
    needed alongside the existing tuning scenes in
    [`assets/world/`](../../assets/world/).
19. **Benchmarks: native warmup-count assertions where they give the
    coverage** (compiles launched, bucket math, no-N×-churn); **per-frame
    GPU timing via custom editor scenes in real Chrome** per (18).

---

## Current state (accurate snapshot)

### What's already good

- Geometry decoupled from material shading
  ([`geometry/`](../../crates/renderer/src/render_passes/geometry/),
  [`material_classify/`](../../crates/renderer/src/render_passes/material_classify/),
  [`material_opaque/`](../../crates/renderer/src/render_passes/material_opaque/)).
- Opaque shading bucketed by `shader_id`; classify writes per-bucket tile
  lists, opaque runs one compute pipeline per active bucket. Pipeline
  switches scale with active buckets, not mesh count.
- Async compile scheduler
  ([`pipeline_scheduler/`](../../crates/renderer/src/pipeline_scheduler/))
  issues `createComputePipelineAsync` promises back-to-back (Dawn
  parallelizes WGSL→MSL), with cross-call waiter dedup
  ([`inflight_compute_cache_waiters`](../../crates/renderer/src/pipeline_scheduler/mod.rs))
  and stale-generation guards.
- MSAA edge handling split out; each material author writes a single-
  sample shading body.
- `prewarm_pipelines`
  ([`lib.rs:539`](../../crates/renderer/src/lib.rs#L539)) batches
  shader+pipeline warmup in three phases (collect keys → one
  `shaders.ensure_keys` → one `pipelines.ensure_keys`).

### What's missing / wrong for the new direction

- **PBR is an uber-shader.** All ~12 KHR extensions are always compiled
  and disabled at runtime via `index == 0u` sentinels
  ([`pbr_material.wgsl`](../../crates/materials/src/wgsl/pbr/pbr_material.wgsl),
  [`material_color_calc.wgsl`](../../crates/renderer/src/render_passes/material_opaque/shader/material_opaque_wgsl/helpers/material_color_calc.wgsl)).
  The only Askama specialization today is mipmap mode (`_grad` vs
  `_no_mips`). No `{% if has_normal_map %}` gating.
- **All first-party materials are baked into the opaque template** by
  [`build_materials_wgsl()`](../../crates/materials/src/registry.rs#L64)
  — always present, never on-demand.
- **`shader_id` is fixed per material *type*** (PBR=1 always,
  [`materials.rs:109`](../../crates/renderer/src/materials.rs#L109)); only
  `Material::Custom` carries a per-registration id.
- **GLTF always maps to PBR** unless `KHR_materials_unlit`
  ([`renderer-gltf/src/populate/material.rs:31`](../../crates/renderer-gltf/src/populate/material.rs#L31)).
  No geometry-only path.
- **`MAX_BUCKET_ENTRIES = 32`** hard cap from a single
  `tile_mask: atomic<u32>`
  ([`compute.wgsl:52`](../../crates/renderer/src/render_passes/material_classify/shader/material_classify_wgsl/compute.wgsl#L52)).
- **Registration churn relaunches everything.**
  [`register_material`](../../crates/renderer/src/dynamic_materials/mod.rs#L569)
  re-marks and relaunches *all* registered materials on every single
  registration. No batch API.
- **Progress is coarse.** `StatusEvent { id, status }` carries only
  Pending/Ready/Failed
  ([`pipeline_scheduler/mod.rs:168`](../../crates/renderer/src/pipeline_scheduler/mod.rs#L168));
  `pending_subcompiles` tracks a per-material counter but there's no
  aggregate or per-phase view. Material-editor just counts Pending events.
- **No tests** under `dynamic_materials/`; **no benchmark** separating
  registration cost from steady-state cost.

---

## The feature-hash contract (implementer's spec)

This defines what "a feature-set" is — the input to the deterministic
feature-hash that selects a bucket `shader_id` (Decision 3). It is the
single source of truth for B.2/B.3.

A **feature** is any code path that can be compiled out of the opaque
shader. For PBR, derived directly from the `Option<_>` fields of
[`PbrMaterial`](../../crates/materials/src/pbr.rs) + the texture-slot
presence flags:

- Per texture map present: base_color, metallic_roughness, normal,
  occlusion, emissive (each independently gateable).
- vertex_color present.
- Each KHR extension present: emissive_strength, ior, specular,
  transmission, diffuse_transmission, volume, clearcoat, sheen,
  dispersion, anisotropy, iridescence.

**Not** part of the feature-hash (and therefore not a bucket dimension):

- `double_sided` — raster state; already part of the *pipeline* key, not
  the shader/bucket. Two feature-identical materials differing only in
  `double_sided` share a bucket but get different pipeline variants
  (existing mechanism).
- `alpha_mode` — `Opaque` → specialized opaque bucket; `Mask`/`Blend`/
  transmission → transparent uber path (Decision 10), never an opaque
  bucket.
- mipmap mode / MSAA — already orthogonal pipeline-variant dimensions.

Toon uses the same contract over [`ToonMaterial`](../../crates/materials/src/toon.rs)'s
optional fields.

The feature-hash MUST be stable for a given feature-set within a build
(so the same combination always maps to the same bucket). The bucket's
WGSL-safe name is derived from the base material name + a short hash
suffix (e.g. `pbr_a1b2`), reusing the
[`BucketEntry`](../../crates/renderer/src/dynamic_materials/mod.rs#L177)
naming helpers.

---

## Workstream A — First-class compilation batching + progress

### A.1 — Unified batch compile engine

- [ ] One internal "compile this final material set" engine that launches
      all compiles once against the **final** bucket layout (no per-item
      relaunch). Shared substrate under GLTF load, dynamic registration,
      and prewarm.
- [ ] Make the three-phase batched model (collect keys → one
      `shaders.ensure_keys` → one `pipelines.ensure_keys`) the **only**
      compile path; retire per-item serial launch loops.
- [ ] Confirm cross-call waiter dedup
      ([`inflight_compute_cache_waiters`](../../crates/renderer/src/pipeline_scheduler/mod.rs))
      covers the batch path so shared pipelines (classify, edge chain)
      compile once per batch, not once per material.

### A.2 — Granular progress (Decision 14)

- [ ] Add `compile_progress() -> { total, done, failed, in_flight }`
      aggregate query on `AwsmRenderer`.
- [ ] Tag status events with a job phase enum
      (`Classify | Opaque | EdgeResolve | Transparent | Geometry`); extend
      `StatusEvent`
      ([`pipeline_scheduler/mod.rs:168`](../../crates/renderer/src/pipeline_scheduler/mod.rs#L168))
      accordingly. Emit on launch and on resolve.
- [ ] Derive `total` from the per-material sub-compile counts already
      tracked by `pending_subcompiles`
      ([`pipeline_scheduler/mod.rs`](../../crates/renderer/src/pipeline_scheduler/mod.rs)).
- [ ] Keep per-phase `tracing` spans so bottlenecks are diagnosable in
      the browser console (per
      [`DEBUGGING-PREVIEW.md`](../DEBUGGING-PREVIEW.md) §"Tracing logs").
- [ ] Update the material-editor's `compile_pending` counter
      ([`material-editor/src/main.rs`](../../crates/frontend/material-editor/src/main.rs))
      to drive a real progress bar from `compile_progress()`.

### A.3 — Startup vs runtime workflows (Decision 13)

- [ ] **Scene-load**: `compile_materials(set).await` — registers the
      final set, resolves when all `Ready`. No partially-missing
      materials. Built on `prewarm_pipelines`.
- [ ] **Runtime / hot-reload**: tolerate `Pending`; expose
      `pipeline_groups_ready()` + a documented "render loading frame"
      loop. Document both patterns side by side.

---

## Workstream B — On-demand built-ins + opaque PBR/Toon specialization

### B.1 — Built-ins compiled on demand

- [ ] Stop unconditionally baking every `enabled_materials()` fragment
      into the opaque compute shader. A material's WGSL/pipeline compiles
      only when a scene contains a material of that base type / feature-set
      (or an explicit prewarm requests it).
- [ ] Keep `enabled_materials()` as the *capability* registry (what
      *can* compile), decoupled from *what is currently compiled*.
- [ ] Verify a scene using only Unlit compiles no PBR/Toon/Flipbook code.

### B.2 — Templatize PBR (and Toon) per feature (Decision 4)

- [ ] Convert PBR WGSL from "all extensions always present" to Askama
      `{% if %}` feature gating driven by a `PbrFeatures` flag struct
      built from the feature-hash contract above. Unused features emit
      **no code**.
- [ ] Gate `PbrMaterialGradients` fields + per-feature loader calls in
      [`material_color_calc.wgsl`](../../crates/renderer/src/render_passes/material_opaque/shader/material_opaque_wgsl/helpers/material_color_calc.wgsl)
      (today all unconditional).
- [ ] Derive `PbrFeatures` from the actual `PbrMaterial` (`Option<_>`
      fields already encode presence).
- [ ] Apply the same to Toon over `ToonMaterial`.
- [ ] Keep `index == 0u` runtime sentinels working as defense-in-depth.

### B.3 — One bucket per distinct opaque feature-set (Decisions 1–3, 5)

- [ ] At material creation, compute the feature-hash and resolve it to a
      `shader_id` via the dynamic registry (deterministic: same
      feature-set → same id → same bucket). The opaque material payload
      writes **that** `shader_id` as its first u32 (no format change).
- [ ] Generalize so a single base material name (PBR/Toon) expands into
      multiple `BucketEntry`s
      ([`mod.rs:177`](../../crates/renderer/src/dynamic_materials/mod.rs#L177)),
      one per feature-hash, each with a unique WGSL-safe name (`pbr_a1b2`).
- [ ] Classify `SHADER_ID_*`/`BUCKET_BIT_*` generation + routing
      ([`compute.wgsl:87`](../../crates/renderer/src/render_passes/material_classify/shader/material_classify_wgsl/compute.wgsl#L87))
      handle many buckets sharing a base material family — routing stays
      pure `shader_id` equality, so no new per-pixel logic.
- [ ] Opaque per-bucket pipeline compiles with only its feature-set's
      WGSL (B.2 flags set from the bucket's feature-hash).
- [ ] **Hard error on budget overflow** (Decision 5): registering a
      feature-set that would exceed `N_WORDS * 32` active buckets returns
      a `BucketCapExceeded`-style error; no silent fallback.
- [ ] **Force-uber** (Decision 9): when the global
      `pbr_specialization == ForceUber`, route opaque PBR to the base PBR
      `shader_id` (single uber bucket) instead of feature-hash buckets.

### B.4 — Extensible bucket cap (Decision 6)

**Cost analysis.** The cap is 32 only because classify uses one
`var<workgroup> tile_mask: atomic<u32>` with `BUCKET_BIT_<NAME> = 1u <<
index`
([`compute.wgsl:49-52`](../../crates/renderer/src/render_passes/material_classify/shader/material_classify_wgsl/compute.wgsl#L49)).
Widening:

- `tile_mask` → `array<atomic<u32>, N_WORDS>`; `BUCKET_BIT` becomes a
  `(word, bit)` pair; the per-pixel OR becomes
  `atomicOr(&tile_mask[word], bit)`; the per-bucket extract loop
  ([`compute.wgsl:112`](../../crates/renderer/src/render_passes/material_classify/shader/material_classify_wgsl/compute.wgsl#L112))
  indexes `tile_mask[word]`.
- **Direct cost is negligible**: `N_WORDS` extra atomic zeroes per
  dispatch (lane 0 only), `4 * N_WORDS` bytes of workgroup memory, a
  word-index lookup per bucket. All `O(bucket_count)` loops + buffers
  (indirect args, edge buffers) already scale dynamically — no new
  scaling.
- **The cost that matters** is *active-in-view* bucket fanout: each
  active bucket = one opaque dispatch + one classify extract arm +
  (MSAA) one edge-resolve dispatch. That scales with distinct buckets
  *visible in a frame*, independent of the cap. Measure that
  (Workstream F), not the mask width.

Checklist:

- [ ] Replace `tile_mask: atomic<u32>` with `array<atomic<u32>, N_WORDS>`;
      template the `(word, bit)` derivation per `BucketEntry`.
- [ ] Make `N_WORDS` an Askama template variable; `MAX_BUCKET_ENTRIES =
      N_WORDS * 32`; **default `N_WORDS = 1` (32)**, trivially raised.
- [ ] Audit every `1u << index` / 32-assuming site (classify edge
      `slot_map`,
      [`MaterialEdgeBuffers`](../../crates/renderer/src/render_passes/material_opaque/edge_buffers.rs),
      indirect-args strides) and make them word-count-driven.
- [ ] Document the active-bucket budget as a product decision, backed by
      Workstream F.

---

## Workstream C — GLTF: drop "always PBR", add `AWSM_material_none`

- [ ] Replace the implicit "every glTF material is PBR" assumption in
      [`pbr_material_mapper`](../../crates/renderer-gltf/src/populate/material.rs#L31)
      with **auto-minimization** (Decision 9): map a standard glTF
      material → the minimal opaque feature-set it actually needs (feeds
      B.3's feature-hash). `KHR_materials_unlit` → Unlit (unchanged).
- [ ] Add the **`AWSM_material_none`** per-material extension (Decision 7);
      when present, skip PBR material creation and route the primitive to
      the **shared flat/unlit bucket** (Decision 8) — assert **zero** PBR
      shader compiles for a material-none-only load (provable via A.2
      counters).
- [ ] Transparency auto-forces uber (Decision 10): a glTF material that
      lands in the transparency pass (`Blend`/`Mask`/transmission) maps to
      the uber transparent PBR path, never a specialized opaque bucket.
- [ ] Honor the global `pbr_specialization: ForceUber` flag (Decision 9)
      as the compat escape.
- [ ] Batch all materials in one glTF load through Workstream A's engine
      (one batch submit, not one-per-material —
      [`populate/mesh.rs:195-314`](../../crates/renderer-gltf/src/populate/mesh.rs#L195)).
- [ ] Emit GLTF-load progress through the A.2 stream.

---

## Workstream D — Dynamic materials, first-class (folds in old plan)

### D.1 — Batch registration API (Decisions 11, 12)

- [ ] `register_materials(Vec<MaterialRegistration>) -> RichResult`
      (shader_ids + readiness handles), **all-or-nothing**. Single-item
      `register_material` / `submit_dynamic_material` become wrappers.
- [ ] Internal order: validate whole batch → cap-check against the
      **final** count → apply all inserts → refresh
      `bucket_entries_cache` + `dispatch_hash_cache` once → resize
      `ClassifyBuffers` once → resize `MaterialEdgeBuffers` (MSAA) once →
      rebuild edge-layout uniform once → clear layout-dependent pipeline
      caches once → mark affected scheduler entries `Pending` once →
      launch compiles against the final layout once → return in input
      order.
- [ ] **Invariant**: never launch compiles for intermediate bucket
      layouts.

### D.2 — Internal transaction boundary

- [ ] Separate mutation from compile-scheduling internally:
      `apply_registry_mutations` (sync, idempotent) →
      `reconcile_material_runtime_state` (caches/buffers/bind groups) →
      `launch_pipeline_compiles_for_final_state`. Keeps registry,
      caches, `ClassifyBuffers.bucket_count`,
      `MaterialEdgeBuffers.bucket_count`, edge-layout uniform, bind-group
      marks, scheduler state, and pipeline caches in sync.

### D.3 — Startup warmup (Decision 13)

- [ ] Scene-load path = `compile_materials(set).await` (shared with A.3).
- [ ] Hot-reload path = `pipeline_groups_ready()` + loading frames.

### D.4 — Honest cost-model docs

- [ ] Replace "existing materials are unaffected" claims on the
      [`dynamic_materials` module doc](../../crates/renderer/src/dynamic_materials/mod.rs)
      with the accurate model: steady-state cost scales with active
      buckets; bucket-set changes can invalidate/relaunch layout-dependent
      pipelines. Frame the active-bucket budget as a product budget;
      state the opaque-only optimization scope.

### D.5 — Regression tests (Decision 17 — native `#[test]`)

- [ ] Register 1st dynamic material → bucket count `first_party + 1`.
- [ ] Register 2nd → `first_party + 2`.
- [ ] Re-register identical → unchanged count, same `shader_id`. **Must
      work at saturation** (idempotency before cap check).
- [ ] Register to cap → `BucketCapExceeded`.
- [ ] `unregister_material` → caches refresh.
- [ ] Register after removal → no stale pipeline keys reused.
- [ ] MSAA on → `MaterialEdgeBuffers` + edge-layout uniform resize.
- [ ] Pending/Ready → materials don't report `Ready` while their pipeline
      cache is cleared and replacement compiles are in flight.
- [ ] **Feature-hash routing**: two materials with the same feature-set
      share one bucket/`shader_id`; differing feature-sets get distinct
      buckets; force-uber collapses both to the base PBR id.
- [ ] **Batch transactional**: a batch with one invalid entry registers
      **nothing** (Decision 12).
- [ ] All native `#[test]` / `#[cfg(test)]`, run via `cargo test
      --all-features`.

---

## Workstream E — Transparency (uber, Decisions 10, 15, 16)

- [ ] Keep transparent PBR a single full-feature uber-shader
      ([`material_transparent/`](../../crates/renderer/src/render_passes/material_transparent/)),
      runtime-branched on material data. **No runtime-branching
      specialization knob.**
- [ ] Wire B.2 so the *opaque* path specializes but the *transparent*
      path requests the full feature-set (all `{% if %}` on) — one
      transparent PBR pipeline.
- [ ] Transparent custom/dynamic materials stay per-material
      forward-rendered (Decision 16); document as a separate budget.
- [ ] Verify transparent draws hit warm cache after `prewarm` (cache key
      includes `texture_pool_arrays_len`,
      [`lib.rs:499`](../../crates/renderer/src/lib.rs#L499)).

---

## Workstream F — Benchmarks & verification (Decisions 18, 19)

### F.0 — Baseline fixture `tuning-50-materials` + before/after procedure

A self-contained 50-mesh scene exists for the headline before/after:
[`assets/world/tuning-50-materials/`](../../assets/world/tuning-50-materials/)
(generated by
[`generate_tuning_scenes.rs`](../../crates/scene-schema/examples/generate_tuning_scenes.rs)
→ `cargo run --example generate_tuning_scenes -p awsm-scene-schema`).

Composition: **50 meshes, each its own material** — 36 PBR across **23
distinct feature-sets** (varying texture-slot presence + vertex colors,
all referencing one tiny placeholder texture since the feature-hash keys
on slot *presence*), 8 Toon (varied bands), 6 Unlit. **Today all 36 PBR
collapse to one uber PBR bucket; after the overhaul they fan out to ~23
specialized buckets** (plus Toon) — landing near the 32-bucket cap, so
this fixture also exercises B.4. Custom/dynamic materials are
intentionally absent: the scene-editor's live materialization
(`node_sync.rs`) doesn't yet consume a primitive's `custom_material`
field, so they'd measure nothing here (tracked separately).

**Launch + measure** (real Chrome, per
[`docs/DEBUGGING-PREVIEW.md`](../DEBUGGING-PREVIEW.md) §"Scene-editor:
load a tuning scene + read timings non-interactively"):

```
task scene-editor:dev                                  # http://localhost:9081
# real Chrome, FOREGROUND visible tab (rAF pauses when hidden):
#   navigate → http://localhost:9081/?trace=sub-frame
await window.wasmBindings.load_scene_by_path("tuning-50-materials")
performance.clearMeasures(); // accumulate ~6s of frames, then:
JSON.parse(window.wasmBindings.read_render_pass_timings(30))
```

**Baseline (BEFORE)** — branch `dynamic-materials-plan` pre-overhaul,
2026-05-30, default editor settings, 1308×759, 360 frames (~6s),
CPU-span wall-clock (not GPU timestamps; noisy — compare means):

| Pass | mean ms | p50 | p95 |
|---|---|---|---|
| Render (total) | 2.249 | 2.2 | 3.4 |
| Geometry | 0.178 | 0.2 | 0.3 |
| Material Classify | 0.035 | 0.0 | 0.1 |
| **Material Opaque** | **0.107** | 0.1 | 0.2 |
| Material Transparent | 0.026 | 0.0 | 0.1 |
| Shadow Generation | 0.128 | 0.1 | 0.3 |
| Light Culling | 0.039 | 0.0 | 0.1 |

The **Material Opaque** mean + total **Render** are the headline
before/after signal. After the overhaul, re-measure with **identical
settings/camera/canvas** (Phase 9). Expectation: not a large regression,
likely an opaque-pass improvement (specialized PBR shaders skip the
unused-extension code the uber shader carries). Watch for a Classify
regression from increased bucket fanout (~3→~25 active buckets).

> **AA caveat**: baseline was captured at the editor's default AA state.
> The after-measurement MUST use the same AA state (toggle MSAA off↔4×
> changes the edge-resolve chain and is a separate axis).

### F.1 — Native warmup-count assertions

- [ ] Native test/bench asserting warmup **counts**: registering/loading
      N materials fires **one** bucket-count update, **one** classify
      resize, **one** edge resize, and **N** pipeline compiles — not `N×`
      of any. Plus bucket-math correctness across the matrix below. No
      GPU; runs in CI.

### F.2 — In-browser per-frame timing (real Chrome, per `DEBUGGING-PREVIEW.md`)

- [ ] **Prerequisite — wire GPU-timestamp render-pass timing.**
      `read_render_pass_timings` is CPU-span (`performance.measure`) and
      measures CPU *encode* time, not GPU shading. PBR specialization's
      win is GPU-side, so it is invisible to the current numbers. The
      `GpuQuerySet` primitives already exist in `renderer-core`
      (`create_query_set`, `resolve_query_set`); wire a timestamp query
      pair around the opaque pass (and others) and surface it next to the
      CPU spans so the *shading* before/after is actually measurable.
      Without this, F.2 only validates that bucket-fanout CPU overhead
      stays small (the regression check), not the win.
- [ ] Build custom scene-editor scenes (alongside
      [`assets/world/`](../../assets/world/) tuning scenes) exercising the
      matrix; capture per-frame GPU cost in **real Chrome via the
      Claude-in-Chrome plugin**, following
      [`docs/DEBUGGING-PREVIEW.md`](../DEBUGGING-PREVIEW.md) (real Chrome,
      `?cam=` repro, tracing-log compile counts; **never** the in-app
      preview).
- [ ] Matrix:
      - opaque feature-set/bucket count: 4, 8, 16, 28, 56 (exercise the
        widened cap)
      - layouts: A spatially-separated, B checkerboard/mixed, C many tiny
        meshes+materials, D large meshes+few islands
      - AA: off, 4×
      - registration mode: one-by-one, batch, prewarmed-before-first-
        frame, hot-reload-while-rendering
- [ ] Per-frame metrics (from `tracing` spans in console): total render,
      Material Classify, Material Opaque, Edge Resolve, Geometry, active
      buckets, recorded dispatches.
- [ ] Use F's active-bucket-fanout numbers to set whether to raise the
      default `N_WORDS` (B.4) and document the active-bucket budget.

### F.3 — glTF extension regression check (model-viewer dropdown)

Because Workstream C drops the "always PBR" assumption and Workstream B
specializes PBR per feature-set, the full set of glTF material
extensions (KHR_materials_clearcoat, sheen, transmission, volume,
specular, ior, anisotropy, iridescence, emissive_strength,
diffuse_transmission, unlit, …) MUST be re-verified to still render
correctly **after everything lands**.

- [ ] In the **model-tests / model-viewer** frontend, step through every
      entry of the **extensions dropdown** and confirm each extension's
      sample model still renders correctly (compare against `main` /
      pre-overhaul, real Chrome per `DEBUGGING-PREVIEW.md` — `getImageData`
      pixel reads or user confirmation, not screenshots).
- [ ] Pay special attention to: transparent/transmission materials (must
      route to the uber transparent path, Decision 10), and any material
      whose feature-set is now compiled into a specialized opaque bucket
      (the templatized `{% if %}` gating must include every extension the
      uber shader previously carried).

---

## Phasing

| Order | Workstream | Notes |
|---|---|---|
| 1 | D.5 regression tests | Lock current dynamic-material behaviour before refactoring (native `#[test]`). |
| 2 | A.1/A.2 batch engine + progress | Shared substrate everything routes through. |
| 3 | D.1/D.2/D.3 batch registration + transaction boundary + warmup | Wraps the engine in the dynamic-material public surface. |
| 4 | B.4 extensible bucket cap | Mechanical, unblocks B.3. Cheap; benchmark after. |
| 5 | B.2 PBR + Toon per-feature templatization | Largest shader-side change. |
| 6 | B.1/B.3 on-demand built-ins + one-bucket-per-feature-set | Depends on B.2 + B.4. |
| 7 | C GLTF auto-minimize + `AWSM_material_none` | Depends on B.2/B.3 feature derivation. |
| 8 | E transparency uber wiring | Depends on B.2 (request full feature-set). |
| 9 | F benchmarks + docs (A.3/D.4) | Validates the cap budget; finalize honest docs. |

---

## Acceptance criteria

1. One batch-compile engine drives GLTF load, dynamic registration, and
   prewarm. Registering/loading N materials fires **one** bucket-count
   update, **one** classify resize, **one** edge resize, and **N**
   pipeline compiles — not `N×` of any.
2. `compile_materials(set).await` lets a frontend register every material,
   await full readiness, and only then render — no partially-missing
   materials. A hot-reload path tolerates `Pending`.
3. `compile_progress()` returns `{total, done, failed, in_flight}` and
   events are phase-tagged; the material-editor drives a real progress bar
   from it.
4. Opaque PBR + Toon are per-feature specialized: a material with no
   normal map compiles no normal-map code; each distinct feature-set is
   its own bucket/`shader_id`, deterministically shared across materials.
5. Built-ins compile on demand: a scene using only one material compiles
   only that material.
6. A glTF with `AWSM_material_none` materials loads with **zero** PBR
   compiles (provable from A.2 counters); those primitives render via the
   shared flat/unlit bucket.
7. Exceeding the active-bucket budget is a hard error; `N_WORDS` is an
   Askama variable (default 32) trivially raised to 64+.
8. Transparency (Blend/Mask/transmission) always uses the uber transparent
   PBR pipeline; no runtime-branching specialization knob was added;
   transparent custom materials stay per-material forward.
9. `pbr_specialization: ForceUber` collapses opaque PBR to the base uber
   bucket.
10. D.5 tests pass as native `#[test]` via `cargo test --all-features`.
11. F.1 native count-benchmark distinguishes warmup cost from per-frame
    cost; F.2 timing is captured in real Chrome per `DEBUGGING-PREVIEW.md`.
12. Docs no longer claim "existing materials are unaffected"
    unconditionally; opaque-only scope + active-bucket budget stated
    honestly.
13. Every entry of the model-viewer **extensions dropdown** still renders
    correctly after the overhaul (F.3) — the specialized/templatized PBR
    shaders preserve every KHR-extension result the uber shader produced,
    and transmission/transparent extensions route to the uber transparent
    path.
14. The `tuning-50-materials` after-measurement is recorded next to the
    F.0 baseline with identical settings; the opaque/total deltas are
    within the "no large regression" expectation (or the regression is
    explained).

Then delete this file (`git rm docs/plans/dynamic-materials.md`) in the
final commit.

---

## Things explicitly NOT to do

- **Don't rewrite the steady-state render path.** Visibility-buffer +
  per-bucket classify/shade is the right architecture. This work
  *deepens* it (more, smaller, specialized buckets).
- **Don't add a payload-format discriminator for routing.** `shader_id`
  stays the sole bucket key (Decision 2). Feature-sets get distinct
  `shader_id`s, not a secondary field.
- **Don't add runtime branching for tunability.** Specialization is
  template/compile-time; the transparent path is uber (Decisions 10, 15).
- **Don't add a silent uber fallback on budget overflow.** It's a hard
  error (Decision 5); widen `N_WORDS` instead (Decision 6).
- **Don't bucket transparent materials like opaque.** Order-sensitive,
  no visibility-buffer benefit — separate forward-rendered budget.
- **Don't use the in-app preview for GPU verification.** Real Chrome via
  the Claude plugin only, per
  [`docs/DEBUGGING-PREVIEW.md`](../DEBUGGING-PREVIEW.md).
- **Don't conflate "compile fired" with "material visible".** A `Pending`
  material must not report `Ready` just because the call returned (D.5).
