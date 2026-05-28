# Dynamic materials — registration lifecycle cleanup

**Status**: planning only, no code yet. This file is the operational
handoff for tightening up the dynamic-material registration path. The
steady-state opaque-shading architecture is already in good shape — the
work below is lifecycle/API/docs/tests, not a re-architecture.

When the work is done, **the last commit deletes this file.**

The recommendations below originated as an external review of the
`awsm-renderer` dynamic material architecture; this doc adapts them to
the current codebase, points at the exact files/symbols involved, and
adds acceptance criteria. Where the review's claims are already true
or partially landed, that's called out inline.

---

## What's already good

The steady-state opaque path is well-aligned with the goal of
supporting many opaque material archetypes in one scene:

- Geometry is decoupled from material shading
  ([`render_passes/geometry/`](../../crates/renderer/src/render_passes/geometry/),
  [`render_passes/material_classify/`](../../crates/renderer/src/render_passes/material_classify/),
  [`render_passes/material_opaque/`](../../crates/renderer/src/render_passes/material_opaque/)).
- Opaque shading is bucketed by material `shader_id`
  ([`dynamic_materials/mod.rs:164`](../../crates/renderer/src/dynamic_materials/mod.rs#L164)
  — `MAX_BUCKET_ENTRIES = 32`).
- The renderer dispatches per material bucket using classify-generated
  indirect args.
- Pipeline switches scale with active shader buckets, not mesh count
  or material assignment count.
- MSAA edge handling is split out so each material author still writes
  a single-sample shading body.

The follow-up work is not about that path. It's about what happens
**around** it — registration, hot reload, scene-load warmup.

---

## The actual risk: registration churn

When a new dynamic material is registered today via
[`AwsmRenderer::register_material`](../../crates/renderer/src/dynamic_materials/mod.rs#L569)
the renderer has to:

- Grow `bucket_entries`.
- Resize `ClassifyBuffers` and (when MSAA is on) `MaterialEdgeBuffers`.
- Rebuild generated classify / opaque / edge-resolve shader templates.
- Clear per-pass typed pipeline caches keyed on `dispatch_hash`.
- Mark existing material scheduler entries `Pending` and launch
  recompiles so they agree with the new bucket layout.
- Update `bucket_entries_cache` and `dispatch_hash_cache` on
  `DynamicMaterials`.

That's correct for **one** registration. It's also correct for **N**
registrations done one by one — but it does ~N× the work, and most of
the intermediate states are immediately invalidated by the next
registration. Painful workflows where that surfaces:

- Scene load.
- Editor startup.
- Material-editor reconnect / refresh.
- Hot reload of many custom materials.
- Importing a glTF / scene file with many authored material archetypes.

The fix is not architectural — it's a batch API + a transaction boundary
+ an explicit "warm up before showing the scene" entry point + tests +
honest docs.

---

## Plan

### 1. Batch registration API

Add a `register_materials` (or `submit_dynamic_materials` for the async
scheduler-handle path) that takes `Vec<MaterialRegistration>` and
performs the bucket-set + cache + buffer resize + compile launch **once**
for the final bucket layout.

```rust
pub fn register_materials(
    &mut self,
    registrations: Vec<MaterialRegistration>,
) -> Result<Vec<MaterialShaderId>, AwsmDynamicMaterialError>;

// or, for the scheduler-handle path:
pub fn submit_dynamic_materials(
    &mut self,
    registrations: Vec<MaterialRegistration>,
) -> Result<Vec<(MaterialShaderId, MaterialId)>, AwsmError>;
```

Internal order:

1. Idempotency / duplicate-name validation for the entire batch.
2. Bucket-cap check against the **final** count, not per-insert.
3. Apply all non-idempotent inserts.
4. Refresh `bucket_entries_cache` + `dispatch_hash_cache` **once**.
5. Resize `ClassifyBuffers` to the new bucket count **once**.
6. Resize `MaterialEdgeBuffers` (if MSAA on) **once**.
7. Rebuild the edge-layout uniform **once**.
8. Clear bucket-layout-dependent typed pipeline caches **once**.
9. Mark affected scheduler entries `Pending` **once**.
10. Launch compile jobs against the **final** bucket layout **once**.
11. Return shader IDs in input order.

The single most important property: **do not launch compiles for
intermediate bucket layouts.**

The existing single-registration paths
([`register_material`](../../crates/renderer/src/dynamic_materials/mod.rs#L569),
[`submit_dynamic_material`](../../crates/renderer/src/dynamic_materials/mod.rs#L937))
stay — they become thin wrappers over the batch APIs (`register_materials(vec![one])`)
so call sites don't break.

### 2. Internal transaction boundary

Even with the batch API in place, mutation and compile-scheduling are
worth separating internally so the invariants are easier to reason
about. Sketch:

```rust
// Sync, idempotent, no scheduler work:
apply_registry_mutations(&mut self, batch) -> RegistryDelta;

// Reconcile derived state (caches, buffers, bind groups):
reconcile_material_runtime_state(&mut self, delta);

// Launch compiles only after reconcile_material_runtime_state
// converged on the final layout:
launch_pipeline_compiles_for_final_state(&mut self, delta);
```

Public API stays simple; the boundary is just an internal contract
that keeps these in sync:

- Registry contents.
- `bucket_entries_cache` + `dispatch_hash_cache`.
- `ClassifyBuffers.bucket_count`.
- `MaterialEdgeBuffers.bucket_count`.
- The edge-layout uniform.
- `BindGroupCreate` marks.
- Scheduler `Pending` / `Ready` state.
- Per-pass typed pipeline caches.

### 3. Explicit startup warmup

Distinguish two workflows in the public API:

**Scene startup / game load** — compile against the final material set
before showing the scene:

```rust
renderer.register_materials(scene.dynamic_materials)?;
renderer.prewarm_registered_material_pipelines().await?;
```

**Runtime / editor hot reload** — tolerate temporary `Pending` states
and per-frame skipped material dispatches:

```rust
renderer.submit_dynamic_materials(scene.dynamic_materials)?;
while !renderer.pipeline_groups_ready() {
    renderer.render_loading_frame()?;
}
```

`prewarm_pipelines`
([`lib.rs:502`](../../crates/renderer/src/lib.rs#L502)) is already the
plumbing for this — Recommendation 3 is just about giving it a clear
public name and a documented usage pattern for each workflow.

### 4. Honest docs on the cost model

The current module doc / comments imply

> Adding a new dynamic material is one new small pipeline; existing
> materials are unaffected.

That's only true if the bucket layout doesn't force existing pipelines
to be regenerated. Replace with something closer to:

> In steady state, each material archetype owns its own small pipeline,
> so per-frame cost scales with active material buckets rather than with
> mesh/material assignments. When the dynamic bucket set changes,
> bucket-layout-dependent pipelines may need to be invalidated and
> relaunched so classify, opaque shading, and edge resolve agree on
> offsets and bucket indices.

Land this on the
[`dynamic_materials` module doc](../../crates/renderer/src/dynamic_materials/mod.rs)
and on the top-level `DynamicMaterials` struct.

### 5. Benchmark separating registration cost from steady-state cost

Add a benchmark target (under `benches/` or wired into the existing
tuning-scene harness) that explicitly separates the two costs.

Matrix:

```
Dynamic opaque material count:
  4, 8, 16, 28

Scene layouts:
  A. spatially separated materials
  B. checkerboard / highly mixed materials per tile
  C. many tiny meshes with many materials
  D. large meshes with few material islands

Anti-aliasing:
  off, 4×

Registration mode:
  one-by-one
  batch
  prewarmed before first visible frame
  hot reload while rendering
```

Metrics:

```
Per-frame:
  total render time
  Material Classify time
  Material Opaque time
  Edge Resolve time
  Geometry pass time
  active buckets
  recorded dispatches

Registration / warmup:
  total registration wall time
  shader compiles launched
  compute pipelines launched
  render pipelines launched (if transparent involved)
  scheduler Pending → Ready transitions
  frames with skipped material pipelines
  peak compile latency
```

Expected shape:

- Steady-state opaque rendering performs well across all four layouts.
- Spatially-separated materials (layout A) are best.
- Highly-mixed tiles (layout B) raise classify fanout and per-bucket
  shading overlap.
- One-by-one registration looks visibly worse than batch.
- MSAA-on validates edge-resolve compile + dispatch scaling.

### 6. Frame `MAX_BUCKET_ENTRIES = 32` as a deliberate product budget

The cap at
[`dynamic_materials/mod.rs:164`](../../crates/renderer/src/dynamic_materials/mod.rs#L164)
is fine. Just frame it explicitly in the module doc:

> The renderer supports `4 first-party material buckets + up to 28
> dynamic material buckets` in any one scene. Dynamic materials are
> intended for a bounded set of opaque archetypes — dozens of
> co-resident material families, not hundreds of unique shader graphs
> in a single visibility-buffer classify set.

The four first-party buckets are enumerated in
[`first_party_bucket_entries`](../../crates/renderer/src/dynamic_materials/mod.rs#L248)
(PBR, Unlit, Toon, FlipBook + whatever else `awsm_materials::registry::enabled_materials()`
returns at build time).

### 7. Strict opaque-vs-transparent expectations in the docs

The dynamic-material bucket architecture primarily optimizes **opaque**
materials. Transparent materials remain order-sensitive and can't be
freely pipeline-grouped without risking incorrect alpha composition.

Add this to the module doc:

> The dynamic material bucket architecture primarily optimizes opaque
> materials. Transparent custom materials should be treated as a separate
> budget because they remain order-sensitive and are more likely to behave
> like conventional forward-rendered material passes.

The transparent dynamic-material path already exists (the registration
code branches on `alpha_mode` to drive transparent-pipeline compiles),
but it's not what the bucket architecture is optimized for. The doc
update is about setting honest expectations, not removing the support.

### 8. Regression tests for bucket-layout mutation

There are currently no unit tests under
[`crates/renderer/src/dynamic_materials/`](../../crates/renderer/src/dynamic_materials/).
Lock in the contract:

1. Register first dynamic material — bucket count goes from
   `first_party + 0` to `first_party + 1`.
2. Register second dynamic material — `first_party + 2`.
3. Re-register identical material (same name, same hashes) — bucket
   count unchanged, returns the existing `shader_id`. **This must work
   even at saturation** (`MAX_BUCKET_ENTRIES`), because the idempotency
   lookup runs before the cap check (see
   [`register_material`](../../crates/renderer/src/dynamic_materials/mod.rs#L569)).
4. Register until bucket cap — confirm `AwsmDynamicMaterialError::BucketCapExceeded`.
5. Remove via
   [`unregister_material`](../../crates/renderer/src/dynamic_materials/mod.rs#L871)
   — confirm `bucket_entries_cache` + `dispatch_hash_cache` refresh.
6. Register after removal — confirm no stale pipeline keys reused.
7. MSAA on: confirm `MaterialEdgeBuffers` + edge-layout uniform resize
   with bucket count.
8. Pending/Ready: existing materials must not report `Ready` while
   their typed pipeline cache has been cleared and replacement compiles
   are in flight.

The renderer is `wasm32`-only, so these need to be `wasm_bindgen_test`
or guarded under `cfg(not(target_arch = "wasm32"))` with mock GPU
handles — pick whichever matches how other renderer-internal tests are
structured (currently: there are none, so this is a greenfield choice).

---

## Phasing

Each numbered item below is shippable on its own; later items get
easier when earlier ones land.

| Order | Item | Notes |
|---|---|---|
| 1 | Regression tests (§8) | Lock down current behaviour before refactoring; everything below either re-asserts these or extends them. |
| 2 | Internal transaction boundary (§2) | Pure refactor; no public-API change. Pre-req for §1's batch path. |
| 3 | Batch registration API (§1) | Wraps the transaction boundary in a public batch surface. |
| 4 | Explicit startup warmup (§3) | New public method on top of `prewarm_pipelines`; trivial once §3 lands. |
| 5 | Docs cleanup (§4, §6, §7) | One commit, no functional change. |
| 6 | Benchmark harness (§5) | Largest scope; landable independently. Probably its own PR. |

---

## Acceptance criteria

The plan is "done" when all of these hold and there are no Phase
2-equivalent stashed bugs hanging around:

1. Batch registration is the primary public API; the single-registration
   methods are thin shims over it (or removed if the call-site count is
   small enough).
2. Registering N dynamic materials in one batch fires **one** scheduler
   bucket-count update, **one** classify-buffer resize, **one**
   edge-buffer resize, and **N pipeline compiles** — not `N*` of any of
   those four.
3. A scene-load API path exists that lets a frontend register every
   custom material, await full readiness, and only THEN start rendering
   the scene — without observing partially-missing materials.
4. Tests from §8 pass on the `wasm32-unknown-unknown` target via
   `wasm-bindgen-test` (or via a native-target mock if the renderer's
   testing convention turns out to be native-mock).
5. The benchmark from §5 produces a CSV-or-JSON dump that distinguishes
   per-frame steady-state cost from per-registration warmup cost.
6. Docs no longer claim "existing materials are unaffected"
   unconditionally.

Then:

- Delete this file (`git rm docs/plans/dynamic-materials.md`) in the
  final commit. The architecture is documented in
  [`dynamic_materials/mod.rs`](../../crates/renderer/src/dynamic_materials/mod.rs)
  module doc + the new tests; the plan doc has no role once the work is
  shipped.

---

## Things explicitly NOT to do

- **Don't rewrite the steady-state render path.** The visibility-buffer
  + per-bucket material pipeline design is the right architecture for
  the problem this renderer targets. The cleanup here is around its
  lifecycle, not its design.
- **Don't raise `MAX_BUCKET_ENTRIES` past 32 to "support more dynamic
  materials"** until §5's benchmark says the classify-pass cost stays
  acceptable. The cap is a budget, not an oversight.
- **Don't try to bucket transparent materials the same way.** They
  remain order-sensitive. Per §7, treat them as a separate budget and
  expect a forward-rendered shape there.
- **Don't conflate "compile fired" with "material visible".** A
  freshly-registered material whose pipeline is `Pending` shouldn't be
  classified as `Ready` from the frontend's perspective just because
  the registration call returned successfully. §8 test 8 locks this in.
