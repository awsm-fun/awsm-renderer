# Uber-shader — selectable per-variant grouping (the partition is the design)

**Status:** implementation-ready spec, NOT scheduled. Do **not** start until Plan B
(`deferred-shared-prep-pass.md`) has fully landed (all stages `[x]`) and the bugs the current
per-material approach is surfacing are fixed. Plan B produces exactly the buffers this consumes
(UV/vcolor arrays, K-layer shadow visibility, the `froxel_walk.wgsl` SSOT, depth-reconstructed
world-pos). Re-read Plan B's "After this loop completes" note before starting.

## The idea

Today shading is **N specialized compute dispatches** — one per bucket (`(shader_id, pbr_features)`
tuple) — issued by `MaterialOpaqueRenderPass::render` looping over `bucket_entries_cached()`, each an
indirect dispatch over that bucket's classify-produced tile list. Every bucket is its own compiled
pipeline (lean, dead-code-eliminated to exactly its feature-set).

The uber-shader lets a **set of variants collapse into one branching pipeline**:

```
read prep buffers (UV/vcolor arrays, K-layer shadow, depth→world_pos)
  → switch(shader_id) { case PBR: …; case TOON: …; case CUSTOM_n: …; }    // runtime branch
  → write opaque_tex
```

**This is NOT all-or-nothing, and that is the whole design.** The construct is a **partition of
variants into groups**; each group compiles to one pipeline. Group-of-1 is today's per-bucket
pipeline; group-of-all is one global uber-shader; the useful configurations are in between and chosen
**per game**. The plan below makes that partition a first-class, authored, schema-persisted input —
not a hardcoded policy.

## Why this is the win vector vs three.js

three does N forward passes (one draw per `mesh.material`); awsm does N compute dispatches (one per
bucket) **plus** a geometry pass + G-buffer bandwidth. Both are O(N) in variant count, and awsm carries
strictly more — losing is structurally expected. Collapsing N shadings into **one branching dispatch**
is the move three **cannot** make (its shading is welded to its per-material draws). awsm's deferred
decoupling — Plan B in particular — is what makes one shading pass possible. That asymmetry is the win.

Secondary wins:
- **Precompile collapse.** N specialized modules → one (per group). The original ~230 s / 1024-module
  compile is the *unbounded custom-material* axis; grouping those customs is where this bites hardest.
- **MSAA edge machinery already shrank to one kernel** (`unified-edge-shading.md` landed): edges are the
  edge branch of each pipeline's `cs_shade` (write-target = accumulator), not a separate per-bucket
  `cs_edge`/`skybox_edge_resolve`. The remaining cross-pipeline combine (`accumulator` + `edge_slot_map` +
  `final_blend`) can shrink **further to zero only at the single-pipeline extreme** (see §MSAA); with >1
  pipeline it stays, by necessity.

---

## Locked decisions (these were the open gates; they are now resolved)

### D1 — The grouping is a *partition*, decided at pipeline-batch-submission time, and rides in the schema

The grouping policy does **not** live on the `AwsmRendererBuilder`. It is **input to the pipeline
scheduler's batch submission** (`AwsmRenderer::submit_pipeline_group_batch` /
`pipeline_scheduler::types::PipelineGroupDef`), which is driven by `ensure_scene_pipelines`. Rationale:
the grouping does not need to be known until pipelines are submitted for compile, so:

- The **editor** can recompute the grouping and **resubmit → recompile** when the author changes it
  (the scheduler already transitions affected materials `Ready → Pending` and recompiles on config
  drift — the same mechanism handles a grouping change).
- The grouping becomes **part of the loadable schema** a player consumes (it ships with the scene, like
  `ShaderIncludes` opt-in ships today).

Concretely: today each material maps **1 `MaterialDef` → 1 `PipelineGroupDef::Material` → 1 pipeline**
(`MaterialId`). The grouping policy changes that mapping so that **several `MaterialDef`s resolve to one
shared pipeline group**. The scheduler/`MaterialId` model already supports a group owning multiple
sub-pipelines and multiple materials being charged to one group, so this is an extension of the
existing batch shape, not a new subsystem.

**Default = all-split = today's exact behavior** (one pipeline per bucket). Grouping is **opt-in**;
a scene that specifies nothing compiles and renders bit-identically to today. Zero-risk default.

### D2 — PBR uses a **per-feature SPLIT / UBER partition** (this is the PBR-split answer)

PBR's "variant" is not one program: it is the 17-bit `PbrFeatures` mask
(`awsm_materials::pbr::PbrFeatures`), and today **each distinct mask is its own pipeline** (it feeds
`ShaderCacheKeyMaterialOpaque::pbr_features`, compile-time gated via `{% if pbr_features.x %}` with
dead-code elimination). A runtime `switch(shader_id)` cannot have an arm per *unknown* feature-combo,
so folding PBR into the uber requires converting compile-time feature gates to **runtime** gates.

**This is feasible with zero new per-instance data:** the per-material feature presence is *already* in
the material storage buffer read by `pbr_get_material(byte_offset)` —
- each extension is an **absolute index where `0 == absent`** (`clearcoat_index`, `sheen_index`,
  `iridescence_index`, `anisotropy_index`, `ior_index`, `specular_index`,
  `emissive_strength_index`, `vertex_color_info_index`, …), and
- each texture is a `TextureInfo` whose presence is detectable (sentinel index).

So a runtime-gated PBR arm branches on `if (m.clearcoat_index != 0u) { … }` etc. Today's compile-time
`pbr_features` gating is therefore **purely a DCE / register-pressure optimization**, not a data
dependency — which is exactly what makes the partition a free knob.

**The knob:** partition the PBR feature set into **SPLIT** and **UBER**:
- **SPLIT feature** → stays compile-time gated → contributes to the pipeline's cache key → materials are
  partitioned into separate pipelines by their combination of present SPLIT features (today's behavior,
  restricted to the SPLIT set). Lean, DCE'd, no register cost for absent features.
- **UBER feature** → runtime-gated inside the shared arm → all materials in the group share one
  pipeline regardless of whether they use it; the feature's code is compiled in (register cost paid by
  every pixel in the group) and skipped at runtime via the `*_index != 0` branch.

The partition spans the full spectrum with one mechanism:
- SPLIT = all 17 features → **exactly today** (every feature-set its own pipeline; min register
  pressure, max pipeline count).
- SPLIT = ∅ → **one PBR pipeline**, every feature runtime-gated (max register pressure, one dispatch).
- any mix → e.g. UBER the common core, SPLIT the rare/heavy lobes.

**Scope of the PBR feature axis is smaller than 17 — transmission-family is excluded.** Materials with
**alpha-blend OR transmission** route to the **transparent forward pass**
(`MaterialShader::is_transparency_pass = has_alpha_blend() || has_transmission()`), which is **out of
scope** for both Plan B and this uber-shader. So the **opaque** uber-PBR arm never compiles
`transmission` / `volume` / `dispersion` (they accompany transmission and are transparent-routed). The
opaque-routed feature axis is just: base-color / metallic-roughness / normal / occlusion / emissive
textures, `vertex_color`, `emissive_strength`, `ior`, `specular`, `clearcoat`, `sheen`, `anisotropy`,
`iridescence`, `diffuse_transmission` (opaque unless paired with transmission).

**Recommended default partition** (when a scene opts PBR into a group but does not specify per-feature):
UBER the **common core** — base-color tex, metallic-roughness tex, normal tex, occlusion tex, emissive
tex, `vertex_color`, `emissive_strength`, `ior`, `specular` — and SPLIT the **rare + register-heavy
lobes** — `clearcoat`, `sheen`, `anisotropy`, `iridescence`, `diffuse_transmission`. This keeps the
shared PBR arm's register footprint bounded while a clearcoat/sheen-heavy scene still gets specialized
tail pipelines. The editor exposes this as a named **"PBR Default" preset** (one click), with every
feature individually overridable; the chosen partition persists in the schema (§Authoring surface).

### D2b — The other built-ins (Unlit / Toon / FlipBook) have **no compile-time feature axis** — base-level membership only

Verified in code: Unlit, Toon, and FlipBook each compile to a **single program** with runtime uniform
params (Toon's `diffuse_bands`/`rim_*`/`specular_steps`/`shininess`, FlipBook's `cols`/`rows`/
`frame_count`/`fps`/`mode`/`flip_y`, base-color factors) — there is **no `*Features` mask**, so there is
no SPLIT/UBER decision *within* them. `toon.rs` says so explicitly ("no compile-gateable paths to vary
on. If Toon ever gains texture sampling, add a `ToonFeatures` mirroring `PbrFeatures`…").

⇒ For these bases the only grouping control is **base-level membership** (which group + per-group
opt-out, D3) — each is one `case` in its group's `switch(shader_id)`. **The schema is nonetheless
per-base-general:** every base carries an (optional) feature-partition slot, empty for Unlit/Toon/
FlipBook today. So the anticipated "Toon gains textures → `ToonFeatures`" path drops into the same
SPLIT/UBER mechanism with **no schema or editor redesign** — only a new feature list to render.

### D3 — Custom/dynamic material grouping is **author-controlled** (same partition, at material granularity)

Custom materials (`shader_id >= DYNAMIC_START`, registered via `MaterialRegistration`) are the
**unbounded** axis — the 1024-unique case. Their grouping is **explicit and author-controlled**, exposed
the same way `ShaderIncludes` opt-in is today:

- An author assigns materials to a named **shading group**; all materials in a group compile into one
  branching pipeline (a `switch(shader_id)` over the group's members, each member's author WGSL wrapped
  as its own `case` exactly as the dispatch-table wrapper does today).
- **Default = group-of-1** (unassigned custom → its own pipeline → today's behavior).
- Surface in **editor + MCP** ("assign these materials to shading group X"). The assignment is part of
  the scene schema (D1) and recompiles on change.
- **No automatic heuristic** in v1. (A by-include-set / cost-similarity auto-grouper is possible later
  but is opaque to reason about for register pressure and divergence — deferred, not built.)

Custom materials keep the **Tier-B protection** they have today: a grouped custom pipeline still forces
`BRDF`/`APPLY_LIGHTING`/`MATERIAL_COLOR_CALC` off per `ShaderIncludeFlags::for_custom`, and each member
compiles only what it declares — grouping must not leak first-party shading into a custom arm or one
member's includes into another. The group's include-set is the **union** of its members' declared
includes (a cost the author opts into by grouping them).

**Per-group opt-out:** a group may be flagged to stay separate pipelines (group-of-1 for its members)
when profiling says branching loses for it — the grouping is a measured tunable, per group.

**Overflow / cap:** a max members-per-group (register pressure / module size). Exceeding it is
**clamped + logged** (mirrors Plan B's K-overflow and the UV/color-set cap policy — never a silent cap);
the overflow members fall back to their own pipelines (hybrid: uber for the group + N-pipeline tail).

### D4 — MSAA: accumulator stays by default; a **single-pipeline fast path** unlocks when (and only when) it can

> **UPDATE — `unified-edge-shading.md` has LANDED (material-increase).** Edges are no longer a separate
> per-bucket pipeline. There is now **one `cs_shade` kernel per material pipeline** that does interior
> *and* edge work, the difference being only the **write target** (interior sample-0 → `opaque_tex`; edge
> samples → the per-sample `accumulator` slot). The old per-bucket `cs_edge`, the global
> `skybox_edge_resolve`, and the per-bucket edge-sample lists are **deleted**; `cs_shade` is driven by a
> per-pixel `edge_id_tex` + the `edge_slot_map`. What REMAINS is exactly the cross-pipeline combine:
> `accumulator` + `edge_slot_map` + `final_blend`. So the analysis below still holds, with the machinery
> renamed: read "`cs_edge`" as "`cs_shade`'s edge branch", and "`skybox_edge_resolve`" as "the skybox
> bucket's `cs_shade` arm". **Crucially for the uber-shader: the write-target branch lives INSIDE the
> shared kernel, so the uber-shader composes with it for free — there is no separate edge pipeline to also
> merge.** The per-default partition just changes which pipeline owns a tile; `cs_shade`'s edge branch is
> already there.

The MSAA edge machinery — `MaterialEdgeBuffers` (`edge_slot_map`, the 4-slot `accumulator`) + the edge
branch of each pipeline's `cs_shade` + `final_blend` — exists because shading is
split across pipelines: at an edge pixel the 4 samples can belong to materials in **different
pipelines**, and a pipeline's `cs_shade` can only shade *its own* samples, so a cross-pipeline
accumulate-then-combine is mandatory. **The instant there is >1 opaque material pipeline, this machinery
is required and does NOT simplify.** Since real scenes almost always have *some* pipeline separation,
**the accumulator path is the default and stays.**

**Fast-MSAA path (detected, not authored):** when the grouping collapses to **exactly one opaque
material pipeline**, light up a fast path that **bypasses** the accumulator / `final_blend` /
`edge_slot_map`. It is gated on pipeline count at submit time (the scheduler
knows the count), not a mode the author picks — so a PBR-only game that chooses full-uber-PBR
(SPLIT = ∅, no customs) gets it automatically.

**Precondition (the subtlety — do NOT assume it away):** "one material pipeline" is necessary but not
sufficient, because edge pixels at silhouettes mix material samples with **skybox** samples (e.g. 2
material + 2 sky → must average together; today the skybox bucket's `cs_shade` arm writes the sky samples
to the accumulator and `final_blend` combines). The fast path works **only if** that single pipeline's
`cs_shade` edge branch resolves **all 4 samples itself** — shading its material samples and sampling the
skybox inline for the sky samples — and writes the blended result directly to `opaque_tex` (the existing
write-target branch, now resolving in place instead of to the accumulator). With one material there is no
material-vs-material edge, so every edge pixel is owned by that one pipeline, which makes this possible:
`accumulator`, `final_blend`, and `edge_slot_map` all dissolve into the self-contained `cs_shade` edge
branch (the per-bucket `cs_edge` + `skybox_edge_resolve` they used to need are already gone).

**Required parts of this decision, all to be specified before coding the fast path:**
- Per-sample divergence at edges is inherent: an edge pixel's samples may hit different UBER-feature
  branches → divergence concentrated at edges (small pixel fraction, real cost).
- The fast `cs_shade` edge branch consumes Plan B's compact per-edge-sample attribute+shadow buffer
  (Option B), branching per sample on `shader_id` + runtime feature flags.
- Correctness is **visual-only** (MSAA edges can't be naga-checked): the fast path must match the
  accumulator path **exactly** (sample weighting, sample-count division, skybox edge samples). Keep the
  accumulator path behind a flag and verify model-tests MSAA-on visually until parity is proven.
- The forward **transparent** path keeps its own MSAA handling (`EdgeResolveBlend`) — unaffected.

### D5 — Defaults summary (zero-risk)

| axis | default | opt-in |
|------|---------|--------|
| material→group mapping | all-split (1 pipeline per bucket = today) | grouping spec in schema |
| PBR per-feature SPLIT/UBER | all-SPLIT (= today) when ungrouped; core-UBER/heavy-SPLIT when PBR is grouped | per-feature override |
| custom grouping | group-of-1 | author assigns groups (editor/MCP) |
| MSAA | accumulator path (one `cs_shade` kernel/pipeline; cross-pipeline combine) | fast path auto-detected at 1 pipeline + skybox-inline `cs_shade` edge branch |

---

## Authoring surface + schema (complete coverage — every material kind is controllable)

The grouping must be fully expressible **in the schema** (so it ships with the scene and a player loads
it) and fully **authorable in the editor + MCP** (so it can be changed and recompiled). This section is
the contract: if it's listed here it's controllable; nothing about grouping is renderer-build-time-only.

### Schema shape — `ShadingGroupSpec` (per scene)

```
ShadingGroupSpec {
  groups: [ ShadingGroup {
    id, name,
    members: [ MaterialRef ],            // first-party bases AND/OR custom shader_ids
    opt_out: bool,                       // force members to stay group-of-1 (D3)
    cap: u32,                            // max members; overflow → own pipelines + log (D3)
  } ],
  // per-base feature partition (only PBR is non-empty today; D2b keeps it general)
  feature_partitions: { base: FeaturePartition { uber: [Feature], split: [Feature] } },
  // unlisted material → group-of-1, all-split (default = today, D5)
}
```

- A `MaterialRef` is a first-party base (PBR/UNLIT/TOON/FLIPBOOK) or a custom `shader_id`
  (`>= DYNAMIC_START`). A group may mix bases and customs (its module is one `switch(shader_id)`).
- `feature_partitions` is keyed by base; PBR carries the SPLIT/UBER lists, others are empty (D2b).
- Round-trips with the rest of the scene schema; `ensure_scene_pipelines` reads it to build the batch.

### Per-kind controllability (the exhaustive matrix)

| material kind | group membership | per-feature SPLIT/UBER | per-group opt-out | cap/overflow | notes |
|---------------|:----------------:|:----------------------:|:-----------------:|:------------:|-------|
| **PBR** | ✅ assign to a group | ✅ full 14-feature partition + **"PBR Default" preset** | ✅ | ✅ | the only base with a feature axis |
| **Unlit** | ✅ | — (single program, D2b) | ✅ | ✅ | feature slot reserved, empty |
| **Toon** | ✅ | — (D2b; future `ToonFeatures` drops in) | ✅ | ✅ | feature slot reserved, empty |
| **FlipBook** | ✅ | — (single program) | ✅ | ✅ | feature slot reserved, empty |
| **Custom/dynamic** | ✅ (editor + MCP) | — (author WGSL is the unit) | ✅ | ✅ | Tier-B protection; include-set = union of members |

### Editor surfaces

1. **Group manager** — create/rename/delete shading groups; drag any material (built-in base or custom)
   into a group; a group spanning bases shows its member `switch` set.
2. **PBR feature partition editor** — a per-feature SPLIT/UBER toggle list, with the **"PBR Default"
   preset** button (sets core-UBER / heavy-SPLIT per D2) plus per-feature override. (Generic per-base
   widget: renders empty for Unlit/Toon/FlipBook today, auto-populates if a base gains a `*Features`.)
3. **Per-group opt-out** toggle and **cap** field (D3).
4. **Live recompile** — any change resubmits the affected groups to the scheduler (`Ready → Pending →
   Ready`); the editor reflects compile status via the existing `pipeline_group_status` /
   `drain_pipeline_status_events` surface. No renderer rebuild.
5. **Feedback/diagnostics** — surface (a) cap overflow ("N members exceeded cap → M fell back to own
   pipelines"), (b) a **single-pipeline indicator** ("fast MSAA active") when the grouping collapses to
   one opaque pipeline (D4), (c) an optional divergence hint when grouping spatially-interleaved
   divergent materials (the opt-out footgun).

### MCP parity

Every editor operation has an MCP equivalent (mirrors how `ShaderIncludes` opt-in + material
registration are already exposed): create/edit/delete groups, assign materials, set the PBR partition
(incl. apply-default), set opt-out/cap. So an agent can author and measure groupings headlessly.

### Player / runtime

The loaded schema's `ShadingGroupSpec` flows straight into `ensure_scene_pipelines` → the scheduler
batch (D1). A player never re-derives grouping; it compiles exactly what the scene authored. Absent a
spec → all-split → identical to today.

---

## The variant space, precisely (what a group's `switch` branches over)

Dimensions that **force separate pipelines** (cannot be a runtime `switch` arm — they change bind-group
layout, raster/sample state, or the sampling intrinsics):
- `msaa_sample_count` (`None`/2/4) — different sample state + edge path.
- `mipmaps` (gradient vs no-mips sampling intrinsics).
- `texture_pool_arrays_len` / `texture_pool_samplers_len` (bind-group layout dims).
- PBR **SPLIT** features (by D2).
- group-overflow members (by D3).

Dimensions that **can be runtime `switch`/`if` within one pipeline**:
- `shader_id` (the top-level `switch` — PBR / Toon / Unlit / FlipBook / custom-n).
- PBR **UBER** features (`if (m.*_index != 0u)`).

So a group's compiled module is: a `switch(shader_id)` over its members, the PBR arm itself an
`if`-ladder over its UBER features, with SPLIT features (and the non-switchable dims) having already
partitioned which pipeline this is.

---

## Implementation hazards (pre-resolved — these silently break if missed)

Verified against the code; each is a concrete blocker a naive pass would hit.

1. **Grouped custom members collide on WGSL symbol names.** The dynamic-material generator emits
   **fixed** names — `struct MaterialData`, `fn material_data_load` (literal in
   `dynamic_materials/registry.rs`), and a single `custom_shade_dynamic` wrapper. Two customs in one
   module redefine all three → compile error. **Fix:** namespace per `shader_id` when grouping —
   `MaterialData_<id>`, `material_data_load_<id>`, `custom_shade_<id>` (the cache_key comments already
   anticipate `custom_shade_<id>`; the generator must be parameterised by id, not hardcoded
   `"material_data_load"`/`"dynamic"`). Same care for any other top-level decl a custom fragment emits.
2. **Per-pixel/per-sample divergence is only safe because sampling uses EXPLICIT gradients.** A grouped
   kernel's `switch(shader_id)` + UBER-feature `if`s are non-uniform across a tile (mixed materials in
   one 8×8 tile). Implicit-LOD `textureSample` in non-uniform control flow is a WGSL hazard (undefined
   gradients). This is already safe in the opaque path — it samples with explicit gradients
   (`texture_pool_sample_grad`, `mipmap_pbr.wgsl`) over prep-materialized UVs (Plan B) — but the uber
   kernel **must preserve that invariant**: no implicit-LOD sampling anywhere reachable under the
   variant/feature branches. State + test it (naga won't catch it; visual artifacts at variant
   boundaries will).
3. **The group's pipeline cache key must encode group composition.** Add the **ordered** member
   `shader_id` list + the per-base SPLIT/UBER partition to `ShaderCacheKeyMaterialOpaque` (alongside
   today's `pbr_features`/`dispatch_hash`/`bucket_entries`). Two different groupings must not alias one
   cached pipeline, and a membership change must invalidate. **Order must be stable** (sort by
   `shader_id`) so the same group hashes identically across frames — and so the `switch` arm order is
   deterministic.
4. **A group needs ONE bind-group layout covering all members.** A group's pipeline binds a single
   layout (main + lights + texture_pool + shadows). Members already share the registry-managed `materials`
   storage + bindless texture pool, so they normally unify — but a member needing a binding the others
   lack means it **cannot join** that group. Enforce at grouping time (diagnostic: "material X can't
   join group G — incompatible bindings"), don't silently miscompile.
5. **Classify groups tiles by GROUP; per-pixel `shader_id` drives the `switch`.** Today
   `material_classify` appends a tile to a bucket's list if any pixel matches, keyed by
   `MaterialBucketLut` (shader_id→bucket). For grouping, the LUT becomes shader_id→**group**, a tile
   joins a group's list if any pixel matches any member, and the group gets **one** indirect-args slot.
   Tiles are then **heterogeneous** — the kernel reads each pixel's `shader_id` (from the visibility
   buffer, as today) and switches. Nothing reads "the tile's material"; there isn't one.
6. **Skybox is not a groupable material.** Bucket 0 / `SKYBOX` (the `OpaqueEmpty` / uncovered-pixel path)
   stays special and is never a group member. Post-`unified-edge-shading.md` it participates as a lean
   `cs_shade` arm (its uncovered/sky samples write the accumulator at edges); it re-enters the picture in
   the fast-MSAA `cs_shade` edge branch (D4), where sky samples are resolved inline.

## Costs / risks to design against

- **Branch divergence:** a wavefront straddling two `switch` arms (or two UBER-feature branches) runs
  both serially. Mitigate with material-coherent tiling — `material_classify` already groups tiles by
  bucket, so tiles are mostly one variant; coherence holds spatially. Net: trades N-dispatch overhead
  for divergence; wins when tiles are coherent (usually). **A group should be coherent** — grouping
  spatially-interleaved divergent materials is the author's footgun (per-group opt-out, D3, is the
  escape hatch).
- **Register pressure / occupancy:** every UBER feature + every group member compiles into the module;
  the compiler allocates for the union → lower occupancy for *all* pixels in the group, even simple
  ones. This is the central tradeoff the SPLIT/UBER knob (D2) and group membership (D3) exist to tune.
  The user's explicit stance: **trading register pressure to unlock fast MSAA (single pipeline) is an
  acceptable per-game choice.**
- **Module size:** bounded only if the group is bounded (hence the cap, D3).
- **Bandwidth at 4K is orthogonal:** one dispatch or N, the G-buffer + prep-buffer read traffic is
  identical. The uber-shader does NOT fix bandwidth; the win is in dispatch/draw-bound regimes (high
  instance count, moderate res) — most real content.

---

## Implementation stages (each independently testable + green; mirror Plan B's stage discipline)

Each stage: `cargo test -p awsm-renderer -p awsm-materials --lib` green (naga validation +
size_regression + completeness) and model-tests render correctly (PBR/IBL dish, alpha, shadows, MSAA
on/off) with a clean console. Default-off / default-all-split until a stage proves parity.

0. **Grouping spec plumbing (inert).** Add the `ShadingGroupSpec` types (§Authoring surface shape —
   groups + members + opt-out + cap + per-base `feature_partitions`, the partition map kept **per-base-
   general** per D2b) to the scene schema + `pipeline_scheduler` batch input. `ensure_scene_pipelines`
   reads it; **default produces the exact same `PipelineGroupDef::Material` set as today** (all-split,
   all-SPLIT, group-of-1). No behavior change; the spec is parsed + threaded but every group is size-1.
   Tests: schema round-trips; default batch is byte-identical to current.

1. **Runtime-gated PBR arm (single-member group, UBER core).** Add a PBR template path that reads
   feature presence at runtime (`m.*_index != 0u`, texture sentinels) instead of `{% if pbr_features %}`,
   for the **UBER** features only; SPLIT features still key the pipeline. Behind the grouping spec; a
   PBR-only scene with the default core-UBER partition now compiles **one** PBR pipeline. Validate visual
   parity (Iridescence/clearcoat dish, normal/emissive/occlusion variants, vertex-color, MSAA off) vs
   the specialized path; measure register pressure / module size / occupancy. This is the **PBR-split
   proof** — do it before any multi-member grouping.

2. **Multi-member groups (first-party). HIGHEST-RISK STAGE — split into sub-commits.** Allow
   PBR+Toon+Unlit+FlipBook (or any subset) to compile into one `switch(shader_id)` pipeline. Per
   hazard 5: turn `MaterialBucketLut` into shader_id→**group**, a tile joins a group's list if any pixel
   matches any member, one indirect-args slot per group; the kernel reads per-pixel `shader_id` and
   switches (tiles are heterogeneous). Carry the ordered member list + partition in the cache key
   (hazard 3); one unified bind-group layout (hazard 4). Visual parity, no-MSAA. Measure dispatch-count
   drop. Suggested sub-commits: (2a) classify group LUT + per-group args, inert; (2b) the merged
   `switch` kernel for a 2-member first-party group; (2c) extend to all four bases.

3. **Custom-material groups + full authoring surface.** Wrap N custom members into one group pipeline
   (each member a `case`, Tier-B protected, include-set = union). Build the complete §Authoring surface:
   group manager (membership for **every** kind — built-in bases AND customs), the PBR partition editor
   with the **"PBR Default" preset**, per-group opt-out + cap, live-recompile status, and the
   diagnostics (cap-overflow, single-pipeline/fast-MSAA indicator). **MCP parity** for all of it. Schema
   persistence + player load (D1). naga over the union; visual parity for a 2–3 custom-material group
   and a mixed base+custom group.

4. **MSAA accumulator path for groups.** Make the existing edge machinery group-aware: a group's
   `cs_shade` edge branch shades its members' samples; `edge_slot_map` keys by group not bucket;
   `final_blend` combines across groups. This is the **general** MSAA path with grouping. Visual MSAA-on
   parity. (Already one kernel per pipeline post-`unified-edge-shading.md` — this just makes the owning
   unit a group instead of a bucket.)

5. **Fast-MSAA single-pipeline path (D4).** When the scheduler reports exactly one opaque material
   pipeline, compile the `cs_shade` edge branch so it resolves all 4 samples (material + skybox inline)
   and writes final directly to `opaque_tex`; skip allocating/dispatching `accumulator`/`final_blend`/
   `edge_slot_map`. Gated, accumulator path kept behind the flag. **Visual-only
   parity** vs the accumulator path on a PBR-only MSAA scene; measure the machinery savings (VRAM +
   dispatch count) and the edge-divergence cost.

6. **Finalize.** Decide per-default partition tuning from measurements; document the editor/MCP grouping
   recipe; re-dump `reports/awsm-dumps/`; update `report.md`; tighten ceilings.

---

## Measurement gates (record before/after; AA off AND on; 1280×720 AND 3840×2160)

1. **Per-group module size + register pressure / occupancy** — the central tradeoff (UBER inflates,
   SPLIT keeps lean). Capture per partition choice.
2. **Precompile time** — pipeline-count × module-size; expect the big drop on the grouped-custom axis.
3. **Dispatch count** — N buckets → #groups; the direct dispatch-overhead win.
4. **Runtime FPS** 720p AND 4K — dispatch-bound (expected win at high instance count) vs bandwidth-bound
   (4K, orthogonal/expected wash).
5. **Edge divergence cost** (MSAA) — fast path vs accumulator; and the VRAM/dispatch the fast path saves.
6. **Correctness** — naga (non-MSAA + accumulator MSAA); model-tests visual parity incl. fast-MSAA
   single-pipeline; clean console.

A useful experiment once grouping exists: take a many-custom-material scene and sweep group size N from
1 (today) to all (one global) — directly measures the partition sweet spot (divergence + register
pressure vs dispatch/precompile collapse).

---

## Open questions (genuinely remaining — small)

- **Per-feature partition tuning:** the recommended core-UBER/heavy-SPLIT default (D2) is a starting
  guess; stage-1 register/occupancy measurements may move `specular`/`ior` or pull `clearcoat` in. Pick
  empirically; the knob makes it cheap to revisit.
- **Group cap value** (D3) — set from stage-2/3 register-pressure measurements, not guessed now.
- **Auto-grouping heuristic** — explicitly deferred (D3); only build if author-controlled grouping
  proves too tedious in practice.

## Readiness for a `/loop`

The plan is loop-shaped: every stage is independently testable and green. Two gates a loop must respect:

- **Plan B must be finished first** (it's the hard prerequisite — this consumes its shadow buffer,
  edge buffer, prep arrays, and `froxel_walk` SSOT). As of this writing Plan B has stages 3b, 4, 5, 6,
  7 remaining. Run Plan B's loop to completion **before** kicking the uber-shader loop (or chain them:
  Plan B → re-read this doc → uber-shader, per Plan B's closing note).
- **Stage 5 (fast-MSAA) needs human visual sign-off.** It's visual-only correctness (naga can't check
  MSAA edge output), so the loop should **stop and request eyes** at stage 5 rather than self-certify
  (same human-in-the-loop stance as `followup.md` for visual/sub-frame work). Stages 1–4 the loop can
  self-verify via `cargo test` + model-tests `scene_png` over the browser MCP.

Empirical choices (PBR partition tuning, group cap) are to be **measured and chosen by the loop**, not
guessed — the measurement gates above produce the numbers.

## Out of scope

- Transparent-path slimming / grouping (transmission/blend stay forward, per D2).
- Auto-grouping heuristics (deferred, D3).
