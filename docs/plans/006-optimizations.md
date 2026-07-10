# 006 — Renderer-wide optimization + feature-robustness sweep

**Order:** sixth — the core of the effort. This is NOT scoped to the current branch:
it is a comprehensive audit of the renderer **as a whole**, in service of one goal:
**players consuming baked bundles get an optimal, fully-featured runtime**, while the
editor stays responsive enough to author and test everything before shipping.

Standing principles (do not relitigate):
- The **prep pass is THE architecture** — fixes and fast paths go through it (all AA
  modes); prefer sharing MSAA/non-MSAA code over duplicating.
- **No per-frame heap allocations** in the render hot path, even when a benchmark shows
  no delta (GC/fragmentation; wasm allocs ≠ JS-GC). Bench with `?stress=N`, trace with
  `?trace=sub-frame`.
- **Loading is ONE transaction**: begin → declare-all → commit; dedup/concurrency are
  internal to the transaction.
- Measure before and after. Every optimization lands with a number (startup pipeline
  count, frame time under `?stress=N`, bundle bytes, load-time ms) or it doesn't land.

---

## Phase 0 — Permanent test scenes (`examples/test-scenes/`) — BUILD FIRST

Everything below is verified against a permanent, versioned suite of scenes. Each scene
is an editor **project** (save/load-able) AND a baked **bundle** (player-loadable), plus
a golden screenshot. Create `examples/test-scenes/README.md` documenting per scene:
what feature it tests, what "correct" looks like, and how to regenerate the golden.
**The README must also include the optimization-axes list below** (verbatim or
lightly edited) so the suite and the sweep stay self-describing.

Minimum scene set (extend freely; one scene may cover several features when that
doesn't blur failures):

| Scene | Features under test |
|---|---|
| `anim-skinned` | skinned mesh playback, rig roundtrip, bone gizmo-free verify |
| `anim-morph` | morph targets, multi-track per-index blending (005 §3) |
| `anim-blend` | animation blends / NLA layers, masks, transport |
| `shadows-all` | directional cascades + spot + point/cube, denoise blur, world-ref bias (no Peter-Pan / donut regressions) |
| `alpha-cutoff` | masked materials, alpha cutoff values, double-sided |
| `transparent` | transparent pass ordering over opaque |
| `prefab-static` | prefab duplication of static meshes — geometry NOT re-uploaded |
| `prefab-skinned-morph` | prefab duplication WITH skins + morphs — independent animation per instance, shared geometry |
| `dynamic-materials` | custom WGSL materials, live uniform edits, instance overrides (texture/buffer/uniform) |
| `builtin-overrides` | overriding built-in PBR params per node/instance |
| `pbr-extensions` | transmission, diffuse transmission, clearcoat, sheen, iridescence, dispersion, anisotropy, volume, specular, ior, emissive_strength |
| `env-ibl` | 3-slot environment (skybox/specular/irradiance), KTX2, built-in default |
| `ssr` | SSR on glossy floor (black glossy dielectric probe), half-res + MSAA edges |
| `bloom-post` | bloom knobs, tonemappers (aces vs khronos_neutral_pbr), exposure, DoF |
| `lights-many` | froxel culling under many point/spot lights |
| `particles` | particle emitter (the existing instancing path) |
| `decals` | decal projection, no skybox bleed |
| `lod-classic` | discrete LOD chain switching (incl. skinned) |
| `lod-nanite` | cluster DAG cut, streaming budget, 2+ simultaneous nanite meshes |
| `instancing-stress` | N×1000s instanced meshes (new axis-5 path) |
| `kitchen-sink` | everything at once — the smoke test and the startup-census scene |

Wire a `task test-scenes` entry that serves them; goldens verified via the plan-002
clean-screenshot workflow. These scenes are AUTHORED under the reverse-Z convention shipped in 003 (becoming its permanent regression lock)
and are the bundle inputs for plan 007 (player tests).

## Phase 0 field notes (running, 2026-07-10)
- **Froxel light culling was dead under reverse-Z** (every punctual light
  culled; tile unproject anchored at NDC z=0 → w→0 NaN planes under
  infinite-reverse). Fixed via template axis; `lights-many` is the lock.
- **Editor decals had never projected** — three stacked bugs fixed (decal
  classify HZB gate not reverse-Z migrated + broken firstLeadingBit mip
  selection; bridge hardcoded texture_index 0; one-shot stale world
  snapshot + half-size wireframe). `decals` scene is the lock. KNOWN GAP:
  a PROCEDURAL texture in the decal slot still projects white (raster
  works) — the pool flat-index resolution or decal-pass texture bind-group
  refresh for freshly-created procedural arrays; fold into axis 1/3 work.
- **Skinned prefab duplication is visually broken** (clones lose clip
  binding — tracks target original armature ids — and render mangled bind
  pose; fresh geometry per clone). `prefab-skinned-morph` golden captures
  the broken baseline DELIBERATELY; axis 4 fixes then regenerates it.
- Cross-checks that PASSED during authoring: 005 §3 morph masking verified
  on-device (two tracks, weights [0.5,0.5]); SSR A/B via ssr_enabled;
  froxel pools per-light after fix; alpha mask cutoff coverage differences.

## Phase 0.5 — Feature gaps that must work before "featureful" is true

- **Global shadows config is not wired**: `scene.shadows` (renderer-wide ShadowsConfig)
  is saved/loaded but NEVER applied to the renderer — no observer, no global panel.
  Wire it (observer like post_process's settings_sync) + a global editor panel + MCP
  exposure. The `shadows-all` scene verifies it.
- **Env 3-slot refactor**: shipped + compiling, browser-verify still pending. The
  `env-ibl` scene closes that.
- Anything the test-scene authoring itself surfaces as broken: fix as part of this
  phase (that's what the suite is for).

---

## The optimization axes

### Axis 1 — Build only what we need
A plain renderer instantiation must compile ONLY the pipelines, shaders, and textures
its actual scene requires.
- Add a **startup census**: count pipelines/shaders compiled + textures allocated at
  init and after first frame, exposed via a debug query; record baselines per test
  scene (empty scene = the floor).
- Audit every eagerly-compiled pipeline: effects slots (001 removes the dead bloom
  extract/blur), picker, lines/grid, shadow variants, decal/classify, SSR/bloom (must
  be zero-cost when disabled — SSR already is; keep it that way), edge/MSAA variants.
- Shader-cache-key axes already gate variants — verify no axis compiles speculatively;
  lazy-create anything that can be (with async compile from axis 2 hiding the cost).
- Acceptance: empty-scene and per-scene census numbers recorded in the test-scenes
  README; no scene compiles a pipeline it doesn't draw with.

#### Axis 1 RESULT (2026-07-10)
Empty-scene cold census: compute pipelines **31 → 22**, shader modules
**49 → 40** (render pipelines 68 unchanged — content-correct for the editor
scene). Bloom, SSR, decal(+classify+composite) and cluster-LOD went from
eager-at-boot to lazy: bloom/SSR compile awaited inside their enable
setters; decals compile via a render-loop kick/poll (fat-line pattern) so
the editor's commit-less live-insert path works — found and fixed TWO
regressions during verification: (1) the editor insert path never reaches
a commit, so the original commit-anchored trigger left decals never
projecting; (2) the lazily-installed composite's bind group was never
marked for creation, and its Err abandoned the WHOLE frame encoder —
black viewport with zero validation errors. Composite now marks
TextureViewRecreate on install (both paths) and skip-renders instead of
erroring when unbound. Classification table lives at the prewarm assembly
(render_passes.rs). Player note: bundles compile feature pipelines inside
the loading screen (post_process flows before commit; decals/clusters
land at commit) — no runtime hitches. hzb/occlusion/coverage deliberately
eager (runtime policy flips). New naga-validation tests for bloom +
cluster families.

### Axis 2 — Concurrency at commit time
The transactional design exists so a batch of declared work compiles concurrently at
commit. Make that true everywhere:
- **Shaders/pipelines**: use async pipeline creation (`createRenderPipelineAsync` /
  compute equivalent) fanned out at transaction commit; never serially await one
  compile before starting the next. Audit `pipeline_scheduler/` — that's its job;
  verify it actually parallelizes across ALL pipeline families (geometry, shadow,
  transparent, effects, SSR/bloom, cluster).
- **Textures**: decode concurrently (createImageBitmap / image-decoder off the main
  await chain), upload in batch; KTX2 transcode parallel where applicable.
- **Editor per-node-commit consolidation** (the long-deferred transaction-model item):
  the editor currently commits per node during load/import in places; consolidate to
  one begin→declare-all→commit per user-visible operation.
- Acceptance: cold bundle load time on `kitchen-sink` before/after; a trace showing
  overlapped compiles, not a staircase.

#### Axis 2 BASELINE (2026-07-10, pre-work)
- Cold `kitchen-sink` project load (dispatch → tree fully populated →
  settled): **~306 ms** (3 runs: 305/306/307 — remarkably consistent; local
  http-server on :9084).
- FINDING for the consolidation work: `wait_render_settled` returns BEFORE
  the async load populates the tree — the load path is not inside the
  settle barrier, i.e. not yet ONE observable transaction end-to-end. The
  §5b consolidation should close that seam (drivers must be able to await
  load completion without polling node counts).

### Axis 3 — Compression (WebP and friends)
Current state: `TextureExport::WebpLossless` IS already the default for bundle bakes
(`editor-protocol/src/assets.rs:94` — every raster ships lossless WebP unless opted
out), with per-texture `WebpLossy{quality}`/`Source` overrides.
- Verify the default actually applies to EVERY texture class in a real bake (albedo,
  normal, metallic-roughness, occlusion, emissive, procedural bakes from 003 §4) —
  test-scene bundles are the fixture; assert no stray PNGs unless `Source` was chosen.
- Normal/data maps: confirm lossless WebP path preserves bytes exactly (decode →
  byte-compare) — lossy must never silently apply to data maps; consider guarding
  `WebpLossy` on data-map slots with a warning.
- Editor project saves (`assets/*.png` side files): evaluate moving to lossless WebP
  too (smaller projects, same fidelity) — decide + implement or document why not.
- Mesh/geometry compression (stretch): evaluate meshopt/quantization for bundle
  geometry; record the size/decode tradeoff, implement only if clearly a win.
- Acceptance: bundle size deltas recorded per test scene; pixel-identical goldens.

#### Axis 2 AUDIT (2026-07-10, scoping — implementation follows axis 4)
- **Pipeline compile fan-out: ALREADY PARALLEL.** The scheduler pushes each
  `createComputePipelineAsync` promise into `FuturesUnordered`
  (pipeline_scheduler/launch.rs), `ensure_keys` awaits via `join_all`, and
  boot submits pipeline groups as a batch (6 groups observed). No serial
  staircase found.
- **Bulk load: a single Replace-join `commit_load` already closes bulk
  loads** (node_sync.rs:222). Interactive edits commit per USER OP — which
  IS the transaction law's grain.
- **Remaining §5b work (queued behind axis 4 — shared editor-bridge
  files):** (a) node_sync has 17 commit_load sites — the per-node commits
  fired during import/materialize paths outside the bulk join need
  consolidation onto it; (b) the observability seam: `wait_render_settled`
  returns before an async `load_project_from_url` populates — the load must
  be one settle-visible transaction (drivers currently poll node counts);
  (c) texture decode concurrency at load (restore_textures loops awaits
  sequentially — decode via createImageBitmap can overlap).
- Baseline to beat: kitchen-sink cold load ~306 ms.

#### Axis 3 RESULT (2026-07-10)
- **Bundle coverage: VERIFIED.** Across every test-scene bundle, all raster
  textures ship as `.webp` (7/7; zero stray PNG/JPEG — the remaining files
  are geometry `.glb`, cluster/mesh `.bin`, env `.ktx2`, and TOML/JSON/WGSL).
  The `WebpLossless` default applies to every texture class in real bakes.
- **Data-map fidelity: PINNED.** New native test
  `lossless_webp_roundtrips_data_maps_byte_exact` (editor `export.rs`)
  round-trips a synthetic normal-map gradient (all channels + alpha) through
  `encode_webp_lossless` — pixel-identical. Lossy WebP is per-texture opt-in
  only (`TextureExport::WebpLossy`), never silently applied.
- **Project side-files: DECIDED — keep source bytes.** Saves persist the
  imported texture's ORIGINAL encoded bytes content-hash-addressed; the
  project is the authoring-fidelity store (re-encoding would change hashes
  and destroy the `Source` export option's meaning). Bundles remain the
  optimized distribution.
- **Mesh quantization (stretch): DEFERRED** — geometry already ships as
  compact glb + cluster bins; meshopt adds a decode dependency for unproven
  wins at current scene scales. Revisit if bundle-size numbers demand it.

### Axis 4 — Prefabs: clone must never clone data
Cloning a mesh must NOT clone geometry bytes; same for skins and morphs. Per-instance
divergence lives in transforms, uniforms, and animation state — not buffers.
- **Known offender (this branch):** `duplicate_skinned_with_new_skin`
  (`renderer/src/meshes.rs`) re-slices and re-uploads the full geometry into a fresh
  resource per instance. Redesign: shared geometry + per-instance skin/joint buffers
  (the skin matrices are the only per-instance GPU data a skinned clone needs).
  Morph weights likewise per-instance, morph target data shared.
- Audit the whole editor prefab flow (denote → instantiate → sync) for convolution:
  scene-loader's `PrefabInstance`/`clone_skin_skeleton` path and the editor bridge
  should agree on one model. Simplify where the flow does redundant work.
- Acceptance: `prefab-static` + `prefab-skinned-morph` scenes — instantiate N clones,
  assert GPU buffer count/bytes grow by per-instance data only (census), animations
  drive each clone independently.

### Axis 5 — Instancing as a first-class authoring feature
GPU instancing exists (`renderer/src/instances.rs`, 64-byte world-matrix stride) but is
only reachable through the particle emitter.
- **Authoring model — DECIDED (David, 2026-07-10): an explicit instancer NodeKind**
  (like the particle emitter owns its instances): a node that references a mesh
  source + owns N instance transforms, so 100k instances never become 100k scene
  nodes. Editor UI for authoring/editing instance transforms (at minimum: count +
  distribution/manual list), MCP command + schema, persistence through project.toml
  and bundle scene.toml; scene-loader instantiates via the instancing path.
- Renderer: verify the instancing path works for all relevant pipelines (opaque,
  masked, shadow, transparent?) — document what's excluded and why.
- Acceptance: `instancing-stress` scene: thousands of instances at interactive frame
  rate, one geometry upload, census-verified; editable per-instance transforms.

### Axis 6 — LOD robustness (classic + nanite)
Both paths exist (skinned→discrete chain, static→cluster DAG; bake at export). The
follow-up hardening (multi-mesh, degeneracy guards, global residency budget) shipped —
but coverage is thin.
- `lod-classic` + `lod-nanite` test scenes exercise: switch distances, skinned discrete
  chains, cluster cut correctness at multiple orbit radii, streaming under
  `?stream`/`?streambudget=N`, 2+ nanite meshes under the global budget, per-mesh
  opt-out.
- Verify the bake tools end-to-end from a cold checkout (CLI + editor bake), not just
  the runtime.
- Dynamic paging (streaming Step 2) stays design-only unless the scenes expose a real
  need — record the decision.
- Acceptance: both scenes golden-stable across budgets; bake CLI reproducible.

#### Axis 6 RESULT (2026-07-10, partial — runtime budgets fold into 007)
- **Bake determinism: VERIFIED.** Two consecutive exports of the same
  project (DamagedHelmet) produced byte-identical artifacts across the
  board: `clusters.bin` (nanite DAG), `lod1-3.glb` + `lod.toml` (discrete
  chain), all lossless-WebP textures, canonical `.glb`.
- **Bake tooling note:** the export pipeline IS the baker (there is no
  standalone CLI); only imported static models take the canonical
  glb+lod+clusters path — captured primitives regenerate from their stacks.
  Recorded in the lod-nanite author.js recipe.
- **Coverage locks:** `lod-classic` (per-mesh opt-out flag round-trip) and
  `lod-nanite` (2 simultaneous DAG meshes on one bake) scenes shipped in
  Phase 0. Streaming budgets (`?stream`/`?streambudget=N`) and the LOD
  switch-distance assert are player-side — implemented as plan 007 tests.
- **Dynamic paging (Step 2): stays design-only** — nothing in the scene
  work surfaced a need beyond the shipped global residency budget.

### Axis 7 — Shading code and math
Highest performance without sacrificing quality; goldens are the quality lock.
- WGSL audit across material_opaque/compute, shadows, effects, SSR, froxels: redundant
  normalizes/matrix ops, per-fragment work liftable to prep or uniforms, branch
  divergence in hot loops, texture fetch counts, half-precision opportunities where
  WebGPU allows, common subexpressions across features that pay when combined.
- Use `?trace=sub-frame` GPU timings per pass on `kitchen-sink` + stress scenes as the
  scoreboard; optimize the top passes first.
- Acceptance: per-pass timing table before/after in this doc; goldens unchanged
  (within tolerance).

### Axis 8 — Rust/wasm allocations and hot-path code
- Sweep the per-frame path for heap allocs: known offenders — `sync_bones_to_skin`
  rebuilds a HashSet+Vec every frame (`editor/src/engine/bridge/skin_bridge.rs`), SSR
  composite/render `vec![]`s (001), plus whatever `?stress=N` profiling surfaces
  repo-wide. Pool/hoist/reuse; the standard applies even without a measured delta.
- Audit dynamic dispatch, redundant clones, and Mutable-signal churn on the frame
  path; editor-side, make sure inspector/outliner updates don't do per-frame work when
  idle.
- Acceptance: zero allocations in a traced steady-state frame (document the tooling
  used to prove it), stress-scene frame times recorded.

---

### Axis 8 RESULT (2026-07-10)
Per-frame allocation sweep (pool/hoist/reuse; David's standard applies
without measured deltas): lights.rs punctual pack scratch + stack info
block; transforms.rs recycled dirty map + spare; instances.rs
double-buffered dirty set; meshes.rs update_world 4 sites (chained dedup,
pooled touched/skip_skins/skin_keys, &HashMap signature) +
set_mesh_instance_attrs scratch; light_buckets pooled per-light bucket
Vecs + to_mark scratch; buffer take_dirty_ranges spare + 14 call sites
hand the Vec back; animations.rs mixer writes by reference (clone only on
rest-restore/first-seed; morph-masking semantics preserved); editor
selection_box thread-local scratch; skin_bridge was already pooled
(d4561d33). Deliberately left (reasons recorded in commit): readback
future ownership moves, core descriptor-vec API (pervasive, collides with
axis-1 files), blend AnimationData in-place redesign (flagged follow-up).

## Method / sequencing
1. Phase 0 scenes + README (with the axes list), Phase 0.5 feature gaps.
2. Baseline measurements on every scene (census, frame times, bundle sizes, load
   times) — commit the numbers into the README.
3. Axes in order 1→8 (cheap structural wins first, shading/alloc micro-work last, so
   earlier changes don't invalidate later measurements twice).
4. Re-measure + update the tables; goldens re-verified after every axis. One commit
   (or more) per axis.
