# Design: the load transaction (declare → commit → ready)

The architecture for getting content onto the GPU: **declare everything, commit once against
the final state, then render.** This replaces the current "mutate the live renderer as content
streams in and reactively recompile on every input change" model.

---

## Why (the problem this deletes)

Today the renderer is mutated while content streams in, and it **reactively recompiles whenever an
input changes** — the texture-pool array count, the live bucket set, etc. Compiling against a
*moving target* is the root complexity, and every bug we hit in this area is a symptom of it:

- **TTFR:** the MSAA edge pipeline recompiles ~3× per load because `finalize_gpu_textures` runs
  several times (gltf populate, scene-loader, IBL/skybox batches) at evolving texture-pool counts,
  each a real recompile. A loading-frame render in between catches a pipeline mid-compile.
- **Config drift on recreate:** the renderer is mutable-after-build, so `remove_all` hand-copies
  live state and silently dropped configs (we hit this with the bucket cap + shadow-caster K).
- The eager-vs-deferred asymmetry, the `last_ensured_bucket_layout` fingerprint, the
  `variants_dirty` per-frame dance — all of it exists *only* to cope with the moving target.

**Principle: never compile against a moving target.** Declare the whole scene, then commit once
against the final, known state. The typical case (load it all up front) becomes first-class; this
deletes machinery rather than adding it.

---

## The transaction

### Lifecycle: `open → add (async) → commit → await → ready`

1. **open** a load transaction.
2. **add** content — textures, materials, meshes, IBL, skybox, … These only *register intent +
   data*; they do **not** touch GPU pipelines or mutate the live pool/bucket layout. Adds may be
   async and run concurrently. The transaction therefore knows the *final* texture-pool array count
   and bucket set the moment adding is done — before anything compiles.
3. **commit()** — the single point where the final inputs are known:
   - finalize the texture pool + bucket set **once** (the final counts),
   - compute the complete set of pipelines needed,
   - compile everything **concurrently** (see below) — kick all, drain as they resolve,
   - upload textures, await to completion → **ready**: every pipeline GPU-resident, every texture
     uploaded, against the final state.
4. Only once a commit is **ready** does its content participate in rendering.

### ① Commit compiles CONCURRENTLY, not serially

`commit()` kicks every needed compile/upload at once and drains them via `FuturesUnordered`
(yielding to the JS event loop so the driver's compile promises fire), rather than awaiting them
one at a time. (The scheduler's existing `inflight_compile` stream is already this shape — the
commit formalizes it as the one drain point.)

### ② Progress: a callback AND an imperative getter, same struct

`commit` reports progress through a single `LoadingStats` struct, exposed two ways from the same
source of truth:

- **subscribe:** `commit().on_progress(|stats: LoadingStats| { … })` (pseudo) — invoked as each
  concurrent compile/upload resolves, so a loading screen can draw a real progress bar.
- **poll:** an imperative getter returns the *same* `LoadingStats` for callers that prefer to read
  it each frame.

`LoadingStats` carries everything we can report (e.g. phase, `pipelines_total` / `pipelines_compiled`,
`textures_total` / `textures_uploaded`, …) — one struct, defined once, used by both paths.

### ③ ZERO difference between cold-load and live — same machinery

The **same** transaction (`open → add → commit → await → ready`) is used for the first scene load
**and** for any mid-gameplay addition. There is **no** separate "initial load" path vs "live add"
path. It works because compile is content-keyed: `commit` compiles *what's needed but not yet
compiled* — cold load ⇒ everything; live ⇒ just the delta (unchanged pipelines are cache hits).
Same logic, different scope.

The only thing that differs is **the app's choice of when to await**, not the machinery:
- **Cold load:** the app awaits the commit before revealing the scene (loading screen meanwhile).
- **Live:** the app fires the commit and keeps rendering the existing scene; the new content
  appears when its commit becomes ready.

> **INVARIANT (hard rule):** if any part of the commit machinery is forced to differ between
> cold-load and live, **STOP and raise a flag to discuss it** before building the divergence.
> Divergent paths for the same action is exactly the trap this design exists to eliminate.

### Render gate

Before the first commit is ready, the renderer does **not** render scene frames against incomplete
state (a loading screen / clear color is fine). This removes the loading-frame renders that today
catch pipelines mid-compile (the `not compiled, skipping` warnings) and the reason the edge ever
compiled against transient inputs.

---

## What it deletes / replaces

- The reactive-recompile machinery: the eager `edge_pipelines.ensure_compiled(...)` inside
  `finalize_gpu_textures`, the multiple finalize→recompile cycles, the `last_ensured_bucket_layout`
  fingerprint, the `variants_dirty`-driven per-frame edge-compile guessing. With "compile once at
  commit against final inputs," none of it is needed.
- Collapses the ad-hoc `prewarm_pipelines` / `finalize_gpu_textures` / `compile_material_variants` /
  `wait_for_pipelines_ready` calls into the one `commit` primitive.
- `remove_all` becomes "drop the committed scene, keep the renderer." Renderer config lives in the
  renderer spec (not scattered mutable fields), so there's no hand-copy to drift — this also closes
  the config-drift class (the `brdf_lut_options` / shadow-K / bucket-cap drops we found on recreate).

---

## Relation to `AwsmRendererBuilder`

`AwsmRendererBuilder` is already "describe the renderer config, then `build()`." The load
transaction is "describe the scene content, then `commit()`." Same declare→commit shape. During
design, decide whether to layer them (builder produces the renderer; transactions load content into
it) or unify the vocabulary so there is one consistent pattern across config + content.

---

## Open questions (resolve during the design/impl pass)

- **API surface:** where the transaction type lives; how `add()` registers content with zero GPU
  work; how `commit()` ties into the scheduler's `inflight_compile` drain.
- **Render gate mechanism:** how the renderer refuses scene-frame rendering until the first commit
  is ready, and how a loading screen draws meanwhile.
- **`LoadingStats` fields:** the exact progress surface.
- **Migration:** move the existing call sites onto the transaction — `populate_gltf`,
  `finalize_gpu_textures`, `compile_material_variants`, `wait_for_pipelines_ready`, the model-tests
  load flow, the editor's load/thumbnail/preview paths, and `remove_all`.
- **Live delta scoping:** what a mid-session transaction declares, and how partial adds interact
  with already-committed content (without violating invariant ③).
- **Verification:** a single `ensure_compiled`/edge compile per cold load (no repeats); no
  `not compiled, skipping` on the first interactive frame; MSAA edges intact; a load trace showing
  reduced compile time; and a live-add path that hits the exact same commit code.

---

## Unrelated open issues (not part of this design)

- **Minor model-tests quirks (cosmetic).** `IridescenceDishWithOlives` renders black (camera
  framing / IBL — black in baseline too, not a renderer regression); a few model names in the
  picker route to "Not Found".
