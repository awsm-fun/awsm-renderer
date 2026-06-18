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

- **TTFR — prewarm pipelines after scene load (#31).** Root-caused but not implemented
  (deferred mid-session to do the uber-shader evaluation first). The prewarm-skip fires on
  the early loading-render frames, and the edge pipeline's inputs evolve through
  build → loading-render → model-load, so it can't be prewarmed reliably up front. Fix
  direction: finalize the edge bind-group layout up-front so the prewarm has stable inputs.

- **Many distinct PBR materials at scale rendered black (unconfirmed — worth a look).**
  While building a throwaway measurement bench, a scene with ~32 distinct first-party PBR
  feature-variants (→ 32 variant pipelines) plus thousands of meshes rendered fully black
  in model-tests. Root cause unconfirmed and the bench is reverted, but a single material /
  a few variants render fine, so something breaks specifically at *many distinct PBR
  variants*. Candidates: mesh material-reassignment not fully re-classifying, transient
  variant-pipeline compile, or a real classify/bucket issue at many-variant scale. Many
  distinct PBR materials is a normal workload, so this is worth confirming or ruling out.

- **Minor model-tests quirks (cosmetic).** `IridescenceDishWithOlives` renders black
  (camera framing / IBL — black in baseline too, not a renderer regression); a few model
  names in the picker route to "Not Found".
