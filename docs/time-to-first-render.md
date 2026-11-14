# Time-to-first-render (TTFR) — architecture + the one residual hitch

Audit from the day-4 perf pass. TTFR here = wall-clock from page load to the
first *correct, non-hitching* viewport frame of the loaded scene.

## Boot sequence (editor)

1. `create_context` builds the renderer. **Builder-time prewarm** compiles, in
   parallel (`createRenderPipelineAsync`): the empty-opaque material kernel for
   the active MSAA, every geometry render variant (MSAA × instancing ×
   storage-array × cull_mode), and the shadow / HZB / coverage / decal /
   classify / light-culling passes.
2. `wait_for_pipelines_ready_with_progress` drains those compile promises (the
   "Compiling render pipelines… (N remaining)" boot-loader phase).
3. `render_loop::start`, boot loader removed.
4. **Then** `?load=<url>` auto-loads a project *asynchronously*, after boot.

## What's intentionally lazy (do NOT prewarm at boot)

First-party **material** shaders (PBR / Unlit / Toon / FlipBook) are NOT
compiled at boot — they compile lazily on first use via
`ensure_scene_pipelines`. This is the deliberate **specialize-only** design
(`materials-overhaul`; since 0.41 PBR kernels are specialized on the
expensive features only, see the 0.41 update at the end): a project that
uses none pays zero material-compile cost at startup. Compiling the "common set" at boot would regress that on
purpose, so the lever is *timing*, not *coverage*.

## The residual hitch (identified)

`prewarm_pipelines` is called **only at boot** (step 2), before any project
loads — when the texture pool is empty, so material prewarm is a no-op by
design. The editor's load/import paths (`LoadProjectFromUrl` handler;
`node_sync` / `material` / `dynamic` bridges) call `finalize_gpu_textures`
after materializing, but **never call `prewarm_pipelines` afterward**. So the
first render of a freshly-loaded scene lazily compiles its material pipelines
on the first draw → the loaded scene shows fallback/grey for the few frames
until `createRenderPipelineAsync` resolves.

## Candidate fix (QUEUED — needs browser verification)

After a project/model load completes (meshes materialized + textures
finalized), call `renderer.prewarm_pipelines().await` (pub) so the live
scene's material pipelines are warm before the first draw — exactly the
"invoke after a model has loaded" case its doc describes.

**Why queued, not landed:** this lives in the texture-pool-shape /
"destroyed texture in submit" race area the codebase has repeated scar tissue
around (see `context::sync_canvas_size`'s IN_FLIGHT coalescing + the
finalize/resize ordering comments). Its entire value — the TTFR win AND the
non-regression (no destroyed-texture GPU error) — is browser-measurable only.
Per the project's DONE-MEANS-DONE + honesty bar, an unverified behavioral
change to this path must not be landed blind.

**Repro to verify (browser, stack up):**
1. `task mcp-dev`; open the editor with `?mcp=…`.
2. Baseline: load a textured project (e.g. import the Fox from :9082) and
   watch the first frames — note the grey/fallback frames before materials
   resolve. Record `get_memory_stats` `render_pipelines` / `compute_pipelines`
   before vs after first draw (the count jump = the lazy compile).
3. Apply the fix (prewarm after the load+finalize path), reload, repeat:
   confirm the pipeline counts are already warm at first draw (no jump), the
   first frame renders materials correctly, and NO "destroyed texture" /
   validation error appears in `get_console_logs`.

## Black-until-resize (resolved — do not destabilize)

The "starts black until you resize" path has TWO healers in `render_loop.rs`:
the `DID_REAL_SIZE_SYNC` latch (drives the thorough `sync_canvas_size` on the
first 0→nonzero transition) and a per-frame surface reconcile under the
renderer guard (reconfigures whenever backing-store ≠ CSS box, before any
render). The per-frame reconcile heals first paint *deterministically* on the
first frame with a nonzero client size. Because the canvas is reparented into
the viewport slot **after** layout (genuinely non-deterministic web timing),
this per-frame self-heal is the *correct* pattern — not a workaround to remove.
Removing it to "fix the root cause" risks reintroducing the black screen for
no TTFR gain (it's a cheap int-compare that no-ops after frame 1). **Audited:
no change.**

## Measurement tooling (already shipped)

`get_memory_stats` (day-3) reports live `render_pipelines` + `compute_pipelines`
counts — sample before/after first draw to observe lazy compiles. Wall-clock
TTFR needs a browser `performance.now()` probe (queued with the fix above).

## UPDATE (2026-06-14, browser-verified) — runtime load already prewarms

Re-investigated with the live editor. The earlier "queued fix" was over-stated:

- **Runtime / player / bundle load (`populate_awsm_scene`) ALREADY prewarms** —
  its Phase 4 calls `wait_for_pipelines_ready_with_progress`, driving every
  material + shadow pipeline to ready (one batch) BEFORE returning, reporting
  `LoadPhase::CompilingPipelines`. So the path that actually ships in a game
  has no first-frame pipeline hitch. ✓ No change needed.
- **Editor interactive import** (`import_model_from_url` → async bridge
  materialization) is the only path that compiles lazily on first draw.
  MEASURED on the live tab: importing the Fox jumps render pipelines 13→107
  (+94) and compute 23→50, settling in <1s on a warm GPU cache; the editor's
  activity pill already covers this window. The cold-cache cost is the
  multi-hundred-ms relocate-able tax.
- **Verdict:** the runtime TTFR is already optimal; the editor-import lazy burst
  is an editor-only UX nicety. A clean prewarm there needs a
  post-materialization-settle hook in the async bridge (not the load handler) —
  a small, non-urgent follow-up, NOT the risky load-path bolt-on first imagined.
  A naive prewarm would be compile-only + lock-serialized (no destroyed-texture
  risk) but ineffective if it runs before the debounced materialization settles.

## UPDATE (2026-11, v0.40) — the cold-load compile model, measured

Everything above describes the renderer as it was in mid-2026 and is kept for
history (`wait_for_pipelines_ready` and the public `prewarm_pipelines` no
longer exist; `commit_load` is the one compile point). This section is the
current model.

A cold first view of a real scene is compile-bound: on desktop Chrome / Metal
~95 % of it is the platform shader compiler, and nothing is drawn until
`commit_load` returns. Instrumenting a shipped player (117 pipelines, 5.3 MB of
WGSL, 13.6 s cold on an M1 Max) showed where the wall time went, and v0.40
changes the compile path accordingly.

### What a cold load compiles, and when

1. **`build()` compiles nothing.** It reserves a pool slot for every config
   pipeline (the "boot pool") and leaves the pipeline caches **deferred**.
2. **While no scene is committed, passes reserve.** In deferred mode
   `ensure_keys` / `get_key` hand back a reserved slot and return. Shadows (on
   the first casting light), SSR, bloom, volumetrics, effects and display (from
   a scene's `set_post_processing`), masked variants (texture finalize) and the
   transparent / line pipelines therefore cost nothing before the commit.
   Nothing can dispatch them yet — `render()` only clears until the first
   commit.
3. **`commit_load` compiles the load as one batch.** It reserves material prep
   for the live config, finalizes the texture pool, resolves material variants
   and kicks the material kernels (`ensure_scene_pipelines`), then compiles the
   whole reserved pool concurrently while the kernels are in flight, and
   finally drains the kernels.
4. **After the commit the caches are no longer deferred.** A live add (the
   editor's steady state: content added to a scene that is already on screen)
   compiles passes as they are built, exactly as before.

`begin_load()` puts the caches back into deferred mode for a full reload.

### What it no longer compiles

- **The first-party-only kernel set.** `ensure_config_pipelines` used to
  compile the MSAA kernels for the five-bucket first-party layout at the top of
  every commit. Kernels bake in the bucket count and their own index, so on any
  scene with a feature-set variant or a custom material those were for a layout
  the scene does not have — the largest serial wave of the measured load after
  the real kernels — and outside a commit they overwrote the live sky /
  canonical kernels. Material kernels are compiled only by
  `ensure_scene_pipelines`, against the live layout.
- **Kernels for buckets nothing shades through.** The canonical PBR bucket is
  always in the layout but every opaque PBR material routes to a feature-set
  variant, so its kernel shaded nothing; likewise Toon / FlipBook / Unlit in a
  scene with no such material. A first-party bucket gets a kernel only when a
  material resolves to it. Registered custom materials always compile (that is
  how an author's WGSL error surfaces right after `register_material`).
- **Masked variants for families with no alpha-tested material.** The masked
  geometry / shadow variants of all four built-in families used to compile on
  every load (44 pipelines).
- **Stale boot reservations.** A pass that re-keys before the first commit
  (material prep after an SSCS change, effects after `set_post_processing`)
  releases the slot reserved for the boot-time config instead of leaving it to
  be compiled for nothing.

### Rules this relies on

- **A material kernel is keyed on the bucket LAYOUT.** Never build kernel
  descriptors from any bucket list other than the registry's live one, and
  never install a kernel without going through the content checks in
  `pipeline_scheduler::launch` (`is_edge_key_desired`,
  `desired_compute_install`). A resolved compile installs iff the latest ensure
  pass still wants its cache key.
- **A reserved slot must not outlive its shader.** Evicting a shader module
  must also release the reserved pool slots built from it
  (`release_reserved_by_shader_keys`), or the next drain fails with "missing
  shader".
- **What a commit skips, the render loop compiles on demand.** A commit only
  compiles for the content it sees, so two things can turn up later on a
  running scene with no commit after them, and both are picked up by the
  render preamble (`render_all`), the same kick/poll way lines and decals are:
  - a material whose bucket has no kernel — the first of its family, or any
    newly inserted PBR material, which shades through the canonical PBR bucket
    until a commit routes it to a feature-set variant (`kick_missing_kernels`:
    one ensure pass, layout untouched). Its pixels are unshaded until the
    kernel resolves, as with a custom material registered live;
  - a built-in material that turns alpha-tested when its family had no masked
    variants (`render_passes::masked_lazy`). It draws solid until they land.

  Warm, either is a few frames; cold it is one compile (about a second).
  Anything that changes the bucket LAYOUT — a new PBR feature set, a custom
  material — still needs a `commit_load`, as it always did.
- **Before the first commit, compile errors surface from `commit_load`**, not
  from the setter that built the pass: the setter only reserved. A rejected
  POST-PROCESS pipeline (SSR, bloom, the froxel volume, the effects / display
  variant) does not fail the commit — the effect is turned off with a warning
  and the load goes on (`degrade_failed_post_processing`); a scene on a device
  that cannot build an effect loads without it. Any other rejected pipeline is
  the commit's error. On a committed scene the setter compiles on the spot and
  returns the error itself. Every setter follows this, `set_anti_aliasing`
  included: called while loading it reserves and leaves the pool alone.
- **On a committed scene, compile before you install.** A pass that is drawing
  must never be handed a key whose pipeline may not arrive: compile the new
  keys first and swap them in only once they exist, so a rejected compile
  leaves the pass on what it had (`ensure_material_prep_pipelines`).
  Reserve-then-drain is for the uncommitted case only.

### What is still serial or wasteful

- Kernels remain the long pole, and the per-feature-set PBR kernels are large
  and mostly identical text. (0.41 stops splitting them on the cheap
  features; the expensive ones still get a kernel each — see the 0.41 update
  below.) They also bake in the bucket count, their bucket
  index and their shader id, so any change to the bucket set recompiles all of
  them and identical kernels in different scenes never share a cache entry.
- The batch is throughput-bound. With several large kernels in it, one of
  them often resolves after every other pipeline, which looks like a
  straggler worth removing. Removing it did not buy anything: an experiment
  that queued every compile in front of the device and released them largest
  shader first — uncapped, and capped at 12, 8, 6 and 4 in flight — moved the
  LAST KERNEL from ~8.2 s to ~6.2 s and left the load at ~8.2 s (DANCE-OFF,
  three alternating rounds each; medians 8.08–8.30 s against 8.21 s
  uncapped, cap 4 the slowest). The same compile work finished in a
  different order. So there is no dispatcher and no cap; the reserved pool is
  issued largest shader first only so that the order is the same on every
  build. One thing stays unexplained: on 0.40's development builds a single
  hash-map permutation of the pool finished about half a second sooner two
  runs in three. Nothing tried since reproduces it.
- Nothing is awaited ahead of the batch any more (0.40.1). The blit pair, the
  SSR composite, the default BRDF LUT and the sRGB / mipmap upload helpers
  live outside the pools (their shaders and layouts are private to their
  modules), and each used to be compiled and awaited where it was built.
  They are now started as early as they are known and collected later:
  `build()` starts the blit compiles first and collects them last; the SSR
  composite is issued when the pass is built and collected by the commit;
  the default BRDF LUT is generated in the background of a cold commit; the
  upload helpers start when an uncompressed image is added, and the three a
  plain RGBA upload uses start inside every cold batch. The per-thread
  caches in `renderer-core` dedupe compiles in flight so that "started
  early" and "needed now" join one compile. `player-tests` locks each with a
  slow-compile check (`nothing-awaited-ahead-of-the-batch:*`).
- The first frame still waits for every pipeline. Dispatch already skips a
  pass whose pipeline is missing, but committing with kernels outstanding
  needs a defined look for unshaded buckets first.

`examples/player-tests` locks the invariants (`pipelines-ready:<scene>`,
`config-ensure-keeps-kernels:<scene>`).

## UPDATE (0.41) — PBR kernels are specialized on the expensive features only

The batch is throughput-bound at roughly two seconds per megabyte of WGSL,
and most of that megabyte count is opaque PBR kernels: ~240 KB each, one per
distinct feature set. Measuring the features themselves showed how little of
a kernel they are:

| Feature | WGSL it adds to a 239,552-byte kernel |
|---|---|
| IOR | 0 bytes |
| dispersion | 7 |
| transmission | 16 |
| specular | 19 |
| volume | 47 |
| emissive strength | 66 |
| vertex colour | 385 |
| diffuse transmission | 1,527 |
| anisotropy | 2,853 |
| clearcoat | 2,935 |
| iridescence | 3,144 |
| sheen | 3,414 |
| secondary maps | 6,309 |

DANCE-OFF compiled five PBR kernels that differed by 66 + 19 + 2,933 bytes.

So the first seven — 540 bytes together — are no longer worth a kernel.
`PbrSpecialization::ExpensiveFeatures` (the default) compiles them into every
opaque PBR kernel; the variant reconcile keys a material's bucket on
`policy.kernel_features(material_features)`, so materials that differ only in
those resolve to one bucket. Nothing in the templates changed: the payload
already carries an index per extension, the `pbr_material_load_*` loaders
return the glTF-absent value at index 0, and the texture fetches and UV
gradients of an absent extension are behind `tex_info.exists`. A kernel
compiled for a superset of a material's cheap features therefore shades it
as the exact kernel would.

Cold, M1 Max, Chrome 157, alternating runs against 0.40.1 (medians):

| Load | 0.40.1 | 0.41 | PBR kernels | WGSL compiled |
|---|---|---|---|---|
| DANCE-OFF stage (player), 6 runs each | 7.60 s | 6.04 s | 5 → 2 | 2.65 → 1.94 MB |
| `pbr-extensions`, 3 runs each | 8.10 s | 7.23 s | 8 → 6 | 3.33 → 2.87 MB |
| `ssr-arena` | 6.06 s | 5.63 s | 2 → 1 | 1.69 → 1.45 MB |
| `kitchen-sink` | 5.35 s | 5.37 s | 1 → 1 | +540 B |
| `alpha-cutoff` | 4.83 s | 4.84 s | 1 → 1 | +540 B |
| `contact-shadows` | 3.86 s | 3.90 s | 1 → 1 | +540 B |

A scene with one feature set compiles the same number of kernels as before,
each 540 bytes longer; its load is unchanged within the run-to-run spread.

Two more things follow from a material's cheap features no longer deciding
its bucket:

- **A cheap feature gained on a running scene shows at once.** The kernel
  already has the code; `update_material` only gives the payload a value for
  it. With a kernel per exact set the material kept its old kernel until a
  commit moved it.
- **Such an edit never changes the bucket set**, so the commit after it
  recompiles nothing. (A new EXPENSIVE feature still adds a bucket, and a
  bucket-set change still recompiles every kernel.)

What this leaves, and what it costs:

- **The expensive features still split.** Merging clearcoat, sheen,
  iridescence and diffuse transmission as well would take DANCE-OFF to about
  5.6 s (one PBR kernel) and `pbr-extensions` to about 5.1 s, and their
  unused paths rendered pixel-identically in the measurement — but every PBR
  pixel would then run those lobes with zero weight, and nobody has measured
  that on a phone. Anisotropy cannot join until its unused path is neutral:
  compiled in, it changed 90 pixels (max 162/255) on materials without it.
- **Per-pixel cost is not measured, on any device.** By reading the code, a
  material without the cheap features now pays, per shaded pixel: one index
  test and a constant per extension loader, the UV lookup for four texture
  slots whose fetch is then skipped (specular, specular colour, transmission,
  volume thickness), and a few multiplies by 1. That should be noise next to
  the light loop, shadows and IBL, but it is an argument, not a measurement.
  `with_pbr_specialization(PbrSpecialization::EveryFeature)` restores a
  kernel per exact feature set for anyone who finds otherwise.

`player-tests` locks it with `pbr-cheap-features-share-a-kernel` (ten feature
sets: two buckets by default, ten with `EveryFeature`, and the two renders
compared pixel for pixel) and `pbr-cheap-feature-edit-keeps-kernels`.

