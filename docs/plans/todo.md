# AWSM renderer — work log (branch `updates`)

A record of completed work on this branch. Implementation detail lives in the linked
commits; this is the index. Unfinished planning has been stripped — open items are in
**Remaining work / known issues** at the bottom.

---

## ✅ Done (shipped on this branch)

### Scene-loader — player-grade follow-ons
- **ParticleEmitter rendering** — `cf40249f`. The loader materializes emitters from the
  scene; the game ticks them each frame ("loader sets up, game drives").
- **InstancesAlongCurve per-instance colors** — `5d75d862`. Per-instance color attributes
  are applied to curve-distributed instances.
- **Prefab non-mesh children** — `c4f65ecc`. Lights / cameras / lines / decals nested in a
  prefab are replayed per prefab instance (not just meshes).
- **Decal texture-index encoding fix** — `4e7110cd`. Decals now pack `(array, layer)`
  against the device's real `max_texture_array_layers` instead of a hard-coded 64, so a
  decal on a pool array with >64 layers samples the right texture.

### Shadows
- **Static-shadow caching** — `7abac541`. Periodic shadow re-renders are suppressed on
  quiet frames (no shadow-caster or camera motion), keeping cached shadow maps instead of
  redrawing them every throttle tick.

### Runtime perf
- **Thousands of meshes** — `b84a95ca`. Added a stress bench and pooled the per-frame
  cull-path allocations to avoid GC churn at high mesh counts. Root-caused the
  DamagedHelmet-at-scale frame drops to GPU geometry (the visibility pass), not CPU
  allocation churn.

### Uber-shader — evaluated → measured → **reverted**
The per-PBR-feature SPLIT/UBER partition was fully built and **proven correct** (naga
validation, pixel-identical render, bounded module size). Then we **measured runtime** —
the deciding question, is a fat branching kernel faster than separate specialized passes?
— and the answer was **no, it's runtime-neutral**:
- Verified GPU-bound at ~30fps (a dense brick of 4000 DamagedHelmets, 1073×720), the
  maximally-fat uber kernel (all 14 PBR features compiled in + runtime-gated) vs the lean
  specialized kernel: **33.89 ms vs 33.61 ms (p90 66.7 vs 67.0) — identical within noise.**
  The occupancy/branch cost does not measurably exist for this content (it's
  geometry/overdraw-bound, not shading-occupancy-bound).
- The pipeline-collapse benefit is sub-millisecond (~µs per eliminated pipeline).
- The visibility-buffer architecture shades once per screen pixel (cost ∝ resolution, not
  mesh count) + GPU culling, so realistic content is GPU-headroom'd / vsync-bound and
  neither the uber cost nor benefit is even reachable.

**Decision (David):** not faster ⇒ reverted entirely (`git reset` back to the audit) to
the clean one-specialized-pipeline-per-variant model. Joining *different* shaders stays a
user-space concern (a dynamic shader + uniforms); built-ins are never force-joined.
- The audit/spec that scoped the evaluation: commit `26456aac` (in history).
- The full working prototype: git tag **`archive/uber-pbr-eval-2026-06-18`** — restore from
  there only with new evidence (e.g. a genuinely shading-occupancy-bound workload, or
  hundreds of distinct PBR materials in one dispatch-bound scene).

---

## ⏳ Remaining work / known issues

- **TTFR — prewarm pipelines after scene load (#31). INVESTIGATED 2026-06-18 — awaiting David.**
  Traced the full path (`prewarm_pipelines` → `ensure_scene_pipelines` → `launch_edge_resolve_compile`;
  `wait_for_pipelines_ready` Phase 1 ensures + Phase 2 drains `inflight_compile`; the app calls
  `compile_material_variants` post-populate). **Two findings:**
  1. The **interactive frame appears already handled**: the app's post-load `compile_material_variants()
     → wait_for_pipelines_ready()` re-runs `ensure_scene_pipelines` at FINAL inputs and *awaits* the
     edge/`final_blend` inflight compiles, so the first interactive frame should be warm. A speculative
     `mark_variants_dirty` force in `compile_material_variants` was tried and **reverted as redundant**
     (wait_for_pipelines_ready already covers it) — it couldn't be shown to change anything.
  2. The **real residual cost is wasted load-time recompiles**: the console shows
     `MaterialEdgePipelines::ensure_compiled: compiling 5 buckets + skybox + final_blend` **~3× during a
     single DamagedHelmet load** (msgids 193948 / 194019 / 194139), because the edge recompiles each
     time the texture-pool-array count / bucket set evolves (0→final) across loading-render frames; a
     render frame in between catches `final_blend` mid-compile → the single first-occurrence
     `not compiled, skipping` warning. The final compile is what the interactive frame uses.
  **DECISION (David, 2026-06-18): DO the deeper fix, in a FRESH session** (this session's context is
  exhausted; edge-compile surgery deserves clean focus). **Plan for the fresh session:**
  - Gate `launch_edge_resolve_compile` (and the per-bucket opaque compile it pairs with) to SKIP while the
    texture-pool-array count / live bucket set are still GROWING during load — i.e. don't compile the edge
    against transient loading-time inputs. Compile ONCE when the inputs are stable (the load-complete
    signal the app already has: `compile_material_variants` runs post-populate; OR detect "counts stopped
    growing"). The 3× recompile (`ensure_compiled: compiling … final_blend` at msgids 193948/194019/194139
    in a single load) collapses to 1.
  - MUST NOT regress: the warm per-frame path (dirty-gated, zero work when nothing changed), the
    MSAA-compile invariant (module carries only its AA config's entry points), or default-equals-today
    (a no-MSAA scene compiles no edge pipelines at all). Don't break the genuine mid-session recompile
    cases (AA toggle, dynamic material register) — only suppress the redundant DURING-LOAD recompiles.
  - Verify: console shows a SINGLE `ensure_compiled … final_blend` per load (no repeats) + no
    `not compiled, skipping` on the interactive frame; screenshot renders (MSAA edges intact); a load
    trace shows reduced compile time. Study `pipeline_scheduler/launch.rs` (`ensure_scene_pipelines`,
    `launch_edge_resolve_compile`, the `last_ensured_bucket_layout` gate) + how the texture-pool array
    count is read during loading vs final.

- **[x] Many distinct PBR materials → whole scene black: a real `remove_all` bug, FIXED (2026-06-18).**
  Reproduced minimally (`?stress=200&variants=32`, a `?variants=M` diagnostic bench in
  `model-tests/scene.rs`) and screenshot-verified. The symptom was `BucketCapExceeded
  { would_be: 33, max: 32 }` → `compile_material_variants` fails → whole scene black. **The bucket
  cap is configurable** (`AwsmRendererBuilder::with_bucket_config`, default 32, range 1..=65534;
  per-frame GPU widths follow the LIVE bucket count, so a high cap is free). **Root cause:**
  `AwsmRenderer::remove_all()` recreates the renderer from a fresh builder but did NOT carry over
  `with_bucket_config` — so the configured cap silently reverted to the default 32 on the first
  `remove_all` (which the model-tests app calls on model load). **Fix:** `remove_all` now
  preserves the live cap via `with_bucket_config(max_bucket_entries: self.dynamic_materials
  .max_bucket_entries())` (`renderer.rs`). Model-tests also sets a generous cap (1024) at build
  (`canvas.rs`). Verified: cap = 1024 at finalize, no error, 32 distinct PBR variant pipelines
  render. The `?variants=M` bench is kept as a dev diagnostic (parallels `?stress`).
  **`remove_all` carry-over audit (done):** compared all 18 `AwsmRendererBuilder::with_*` methods
  vs what `remove_all` copies. Found two more dropped CONFIGS: `with_max_shadow_casters_per_pixel`
  (the prep `K`) — **now carried over** via `self.prep_config.max_shadow_casters_per_pixel`; and
  `with_brdf_lut_options` — still dropped (niche: only a CUSTOM BRDF-LUT reverts to default on
  `remove_all`; the renderer stores the generated LUT, not the options, so carrying it needs a new
  stored field — left for the refactor below). The rest of the not-copied set is intentional: IBL/
  skybox colors are scene content (re-set by the caller on reload); `with_phase_handler` is a
  transient loading-UI hook; `with_profile` is a bundle whose effects are carried via the individual
  resolved settings. Also rewrote the cavalier `// meh, just recreate the renderer` comment into a
  carry-over contract so future builder configs don't silently drift.
  **RECOMMENDED REFACTOR (David to decide):** the hand-listed copy is structurally fragile — every
  new builder config must remember to add a line here. A robust fix: capture the build-time config
  inputs in one struct stored on the renderer and replay it in `remove_all` (also closes the
  `brdf_lut_options` gap). Not done unprompted (it's a builder/renderer structural change).

- **Minor model-tests quirks (cosmetic).** `IridescenceDishWithOlives` renders black
  (camera framing / IBL — black in baseline too, not a renderer regression); a few model
  names in the picker route to "Not Found".
