# Materials system overhaul — many small opaque materials, first-class

> ## ▶ RESUME HERE (fresh session)
> Branch `dynamic-materials-plan`, green at the latest commit (159
> renderer tests; full workspace green). Architecture = **specialize-only,
> NO uber** (read **⭐ ARCHITECTURE PIVOT** below; supersedes conflicting
> "Locked decision"s).
>
> **GPU baseline checksum for `tuning-50-materials` = `3948677115`** (my
> 48×30-grid FNV-1a, two-frame-stable; the old `2985139072` used a
> different unsaved algorithm — not comparable). A pixel-equivalent change
> must reproduce 3948677115. Verification loop is live + working (see
> `[[materials-overhaul-verification]]` memory): dev server `task
> scene-editor:dev` @ 9081 (trunk watches the renderer crate; grep
> `/tmp/scratch/dev.log` for `applying new distribution`), real Chrome via
> Claude-in-Chrome, **foreground visible tab** (do NOT open a new tab — it
> steals focus), `await
> window.wasmBindings.load_scene_by_path("tuning-50-materials")`.
>
> **DONE this session (commits 855097b → 4900123):**
> - #14 foundation: `ShadingBase` decouples opaque + edge body-selection
>   from the numeric id; per-pixel/sample guard is numeric; `BucketEntry`
>   carries the variant identity `(base, pbr_features)`; the hard-coded
>   `enabled_materials()` prefix is gone (built from seeded variants).
> - #14 registry: `resolve_first_party_variant(base, features)` allocates
>   + dedups per-feature-set variant ids (dynamic range), exposed as
>   bucket entries; `dispatch_hash` + `is_empty()` now fold `fp_variants`.
> - #15/#16 routing (IMPLEMENTED, **gated OFF**): `Materials`
>   `resolved_shader_id` payload override + `variants_dirty`;
>   `reconcile_material_variants` (render preamble) derives features →
>   resolves variant → stamps payload → relaunches all buckets on growth;
>   launch reads `base`/`features`/`owns_skybox` from the bucket entry;
>   variants route through `launch_dynamic_material_compile`
>   (`dynamic_shader=None`); `owns_skybox` (only canonical PBR id=1 writes
>   skybox). Fixed a black-screen bug: `is_empty()` ignored variants → the
>   classify pass used the stale eager 4-bucket pipeline.
>
> **✅ SHIPPED: opaque PBR feature-set specialization is ON by default**
> (`Materials::pbr_specialization` default true = Decision 9 Auto;
> `set_pbr_specialization(false)` is the ForceUber escape). GPU-verified on
> a clean load: full scene renders, all buckets compile to Ready, the
> compile-progress modal drains + closes (A.2 fix), pixel-equivalent within
> ≤2/255. Applies to BOTH scene-editor inline materials AND glTF-loaded
> materials (the glTF mapper already emits minimal `PbrMaterial`s → the
> reconcile auto-specializes them → Workstream C "auto-minimize" is
> satisfied for free). The black-screen `is_empty` bug + the stuck-modal
> A.2 counter were found + fixed. Remaining = OPTIONAL follow-ups below.
>
> (Detail retained:) **Routing is GPU-VERIFIED CORRECT** behind the runtime
> `Materials::pbr_specialization` flag. Toggle for A/B via the
> scene-editor wasm exports `set_pbr_specialization(bool)` /
> `set_msaa(bool)` (added this session). Full A/B matrix verified in real
> Chrome on tuning-50-materials (36 PBR / 23 feature-set masks; adjacent
> meshes differ → genuine multi-material silhouette edges):
> - **Specialization shading is pixel-equivalent within ≤2/255** at MSAA
>   off (spec-on vs spec-off: 23/24 points identical, 1 lit pixel off by
>   2). The BRDF lobes don't bit-exactly zero at factor 0, so stripping
>   them shifts a couple LSBs — sub-perceptual, within the ±20/255 tonemap
>   tolerance. (My earlier "it's MSAA edges" hypothesis was WRONG — proven
>   by the MSAA-off A/B; it's the lobe-zeroing, and it's negligible.)
> - **MSAA + bucketing + multi-material edges render correctly**: full
>   scene, no seams, inter-material silhouettes blend to reasonable colors;
>   2141/2160 grid points within 2/255, larger diffs concentrated at the
>   silhouette edges (expected — bucketing now anti-aliases inter-material
>   edges the single-bucket baseline merged).
>
> **REMAINING (optional follow-ups; #14/#15/#16 + A.2 + C-auto-minimize +
> the cap-guard are DONE):**
> - **#17 transparent specialize** — the transparent fragment is still the
>   uber runtime-branched path. It WORKS (correct); specializing it is an
>   occupancy optimization, not a fix. Mirror the opaque `base`+features
>   decoupling onto the transparent cache key/template + gate the fragment.
>   Verify in model-viewer (transmission/blend models).
> - **#18 Toon** — Toon's opaque shader doesn't sample its textures yet (no
>   gateable opaque code paths) so it's correctly single-bucket. To
>   specialize: first add Toon base_color/emissive texture sampling, THEN
>   `ToonFeatures` + flip `ShadingBase::Toon.is_feature_specialized()` +
>   add Toon to the reconcile match.
> - **C `AWSM_material_none`** — the per-material skip-PBR glTF extension →
>   shared flat/unlit bucket (zero PBR compiles). (Auto-minimize already
>   works.) Verify in model-viewer.
> - **A.1 batch engine / A.3 warmup-await** — the bucket-growth relaunch
>   recompiles all buckets (the reconcile already does it ONCE against the
>   final layout, so no N× churn, but a heavy cold-load compile + a one-
>   frame `final_blend`-skip). `compile_materials(set).await` during
>   load_scene would hide the transient. Perf/UX polish, not correctness.
> - **F.2/F.3 benchmarks + GPU-timestamp timing + per-extension pass**;
>   **D.4** honest cost-model docs. **B.4**: raise `MAX_BUCKET_WORDS` past
>   1 only when a scene needs >32 buckets (overflow currently degrades
>   gracefully to the uber PBR bucket with a logged warning).
> Then delete this file.

**Status**: in implementation. Every open design question is resolved
(see **Locked decisions**, as amended by the ARCHITECTURE PIVOT). This
file is the operational handoff + live tracker.

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
| B.2 gate `material_color_calc` (loads/samplers/struct → DCE) | ✅ done | `bc92e26` | **pixel-identical** at `all()` — full feature data-flow now gateable |
| B.2 **unify** lobe gating — one body, 2 gate modes (`pbr_runtime_gated`) | ✅ done | `958f55f` | uber pixel-identical; **both modes tested** (3 native `brdf_gate_tests`) |
| B.2 step C: activate (pass feature-union / `pbr_runtime_gated=false`) | ⬜ todo | — | **needs a recompile trigger** — see below |
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

### Note — runtime guards vs compile-time specialization (corrected)

While implementing B.2 the actual `brdf.wgsl` was read end-to-end. An
earlier draft of this note wrongly concluded specialization "buys little"
because the uber shader already runtime-guards most extensions
(iridescence `if color.iridescence > 0.0`, anisotropy, diffuse_transmission,
clearcoat-IBL, sheen-IBL; `material_color_calc` samplers all `if exists`;
only **sheen + clearcoat direct** run unconditionally). That conclusion
was **wrong** — corrected here:

**Compile-time specialization wins over runtime guards in ways the guards
cannot, and these are the point of B.3:**

1. **Register pressure → occupancy (dominant).** `PbrMaterialColor`
   carries ~25 extension fields and `material_color_calc` computes them
   because the runtime `if` still *reads* them → they're **live** across
   the lighting loop → high register count → low GPU occupancy. A
   compile-time `{% if %}` that's false means the field is never read, so
   the compiler **dead-code-eliminates** the whole compute→store→load
   chain → far fewer registers → higher occupancy → higher FPS on
   GPU-bound scenes. **Runtime guards cannot DCE; compile-time gating
   can.** This is the main win.
2. **Guard overhead** — the per-pixel-per-light load+compare+branch ×
   ~17 extensions, gone.
3. **Divergence** — today all PBR is one bucket, so a warp spans mixed
   feature-sets and the guards *diverge* (both paths execute, masked).
   B.3's per-feature-set buckets make each warp coherent → no divergence.

**Implication for the implementation**: the occupancy win requires gating
**both** `material_color_calc.wgsl` (so the field computation DCEs) **and**
`brdf.wgsl` (the lobes), so the feature's data flow drops end-to-end.
B.2 step B gated only `brdf.wgsl` — necessary but **not sufficient** for
the occupancy win; `material_color_calc` gating (+ ideally conditional
`PbrMaterialColor` fields) is still required. The magnitude is real but
scene-dependent (occupancy-bound scenes benefit most) — confirm with the
GPU-bound FPS test, but **B.3 is worth pursuing**; do not deprioritize it.

#### Activating the gating (step C / B.3) — the exact remaining work

The B.2 *mechanism* is in + verified (gates render identically at
`all()`). To make features actually strip, the PBR opaque pipeline must
be compiled with a narrower `pbr_features` than `all()`. Two findings
pin down how:

1. **Dispatch looks up pipelines by `PipelineKeyId { msaa, mipmaps,
   shader_id }` — NOT the full shader cache key** (which is what carries
   `pbr_features`). So `pbr_features` only selects *which WGSL compiles*;
   reinstalling a differently-gated pipeline at the same `PipelineKeyId`
   is transparent to dispatch. No dispatch-side change needed for the
   *scene-union* approach.
2. **`launch_first_party_material_compile` compiles PBR once per
   `shader_id` and does NOT recompile for later same-`shader_id`
   materials** (launch.rs:396). So a scene-union computed lazily would be
   built from whatever materials existed at first-compile and then go
   stale — later materials' features would be wrongly stripped.

So the **scene-union (one PBR bucket) path** needs: compute the union
over all opaque `Material::Pbr` (`self.materials.lookup`, filter
`!is_transparency_pass`, fold `PbrFeatures::from_material`); cache it;
and on material add/remove/edit, if the union grew, **invalidate +
relaunch the PBR opaque pipeline** (clear its `PipelineKeyId` entry so
it recompiles with the wider union). Pass the cached union as
`pbr_features` for `shader_id == PBR` at the four cache-key sites
(`all().bits()` for every other id).

The **per-feature-set bucket path (B.3)** sidesteps the union/stale
problem entirely and is the locked design: derive each opaque PBR
material's `shader_id` from its `PbrFeatures::bits()` feature-hash
(deterministic, reuse the dynamic registry), write that id into the
material payload, add it to `bucket_entries`, and compile its pipeline
with exactly that feature-set. No union, no invalidation race — each
material routes to its exact specialized pipeline, and warps are
feature-coherent (the divergence win). Bigger change, cleaner result.
Recommended over the union halfway-house.

**Measurement gate**: either way, the FPS win only shows on a GPU-bound
scene. Make a stress variant of `tuning-50-materials` (≫ resolution or
≫ instance count) so it drops below 60 fps, then compare rAF FPS
before/after with identical settings.

#### Unified PBR feature gating (DRY — one body, two gate modes)

To avoid maintaining the gating logic twice, each PBR feature's shading
body in `brdf.wgsl` is written **once** and wrapped by a single scheme
driven by the template flag `pbr_runtime_gated`:

```wgsl
{% if pbr_features.<feat> %}                  {# compile-time presence #}
if ({% if pbr_runtime_gated %}<runtime_cond>{% else %}true{% endif %}) {
    <feature body — written once>
}
{% endif %}
```

- **Specialized** (opaque per-bucket, post-B.3): `pbr_runtime_gated=false`,
  narrow `pbr_features`. Absent features emit nothing; present features
  run unconditionally (`if (true)` folds away) — **compile-time only**.
- **Uber** (transparent always; opaque until B.3): `pbr_runtime_gated=true`,
  `pbr_features=all()`. Everything emitted, gated by the runtime
  `if (color.x > 0)` — because one shader serves heterogeneous materials.

`pbr_runtime_gated` is a field on all three `brdf.wgsl` includers
(opaque/edge/transparent), currently `true` everywhere; B.3 flips the
opaque path to `false` per specialized bucket. `material_color_calc.wgsl`
needs no runtime flag — its `{% if pbr_features %}` field-computation
gate already serves both (uber=all→computes all for the runtime guards;
specialized→present computed, absent constant → DCE).

**Both modes are tested** natively (`brdf_gate_tests`): uber emits the
runtime guards; specialized strips absent lobes and emits present lobes
with no runtime guard. (They caught a real omission — an un-gated IBL
iridescence block — on first run.) Anisotropy is handled by writing the
isotropic base once and letting the anisotropy feature *override* it
(no macro, no duplication). Remaining mechanical follow-up:
transmission/volume/dispersion IBL still use their original runtime
guards (correct at uber; convert to the same scheme for reliable
specialized stripping).

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

## ⭐ ARCHITECTURE PIVOT — specialize-only, no uber (supersedes the uber decisions below)

Decided with the owner after implementing B.2. **There is no "uber"
shading mode anywhere.** Reasoning: runtime feature-gating only earns its
keep when one shader invocation must serve *multiple* feature-sets — but
every dispatch is already material-homogeneous (opaque pixels are split
by `shader_id` in the classify pass; transparent is drawn per-mesh, one
material per draw). So a compile-time-specialized shader is always
correct, and it wins on every per-pixel axis (register pressure →
occupancy, instruction cache, divergence). Uber only ever bounded
pipeline *count* (a warmup/memory concern the batched compiler handles)
and transparent pipeline-*switch* count (tiny, per-mesh, and mitigable by
sorting — never by a fat shader). And the per-pixel runtime guards it
used were pure perf, never correctness (every gated lobe contributes
mathematically zero when its value is zero), so deleting them is *also*
likely faster (no branch, lower registers, uniform execution).

**This supersedes Decisions 4 (partially), 9, 10, 15 below.** Concretely:

1. **One shading mode: specialized.** Every PBR/Toon material compiles a
   shader gated *only* at compile time (`{% if pbr_features.<x> %}`).
   The only runtime branches left are logically-necessary ones (lighting
   geometry, light loops) — never feature presence.
2. **Transparent specializes too.** No shared uber transparent fragment
   shader; each transparent material gets a pipeline for its feature-set.
3. **No `pbr_runtime_gated` flag, no force-uber config, no scene-union
   "step C".** Those were uber/transition scaffolding — removed.
4. **Unified variant registry — static and dynamic go through the EXACT
   same mechanism.** `shader_id` is an in-memory id allocated by the
   registry; there's no cross-session meaning, so no deterministic
   encoding. Every bucket is a registry *variant* with an allocated id,
   deduped by its key:
   - **FirstParty `{ base: PBR|TOON, features: FeatureSet }`** — WGSL is
     the built-in shader templated from `features` (gets Askama's
     compile-time guarantees). Deduped by `(base, feature_hash)`.
   - **Custom `{ registration }`** — WGSL is the author's fragment.
     Deduped by content hash (as today).

   The hard-coded first-party `bucket_entries` prefix goes away; PBR/Toon
   buckets are registry variants like everything else (PBR's empty
   feature-set = the smallest PBR variant). Unlit + Flipbook stay
   single-bucket (negligible optional features).
5. **B.3 is mandatory, not a fallback** (no uber to fall back to). The
   bucket-budget cap is a hard error (Decision 5, unchanged); raise
   `N_WORDS` (B.4) to lift it.

The "Locked decisions" below are kept for history; where they conflict
with this pivot, **this pivot wins.**

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
