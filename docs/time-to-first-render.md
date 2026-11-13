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
(`materials-overhaul`): a project that uses none pays zero material-compile
cost at startup. Compiling the "common set" at boot would regress that on
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
- **Anything that needs a new pipeline needs a `commit_load`.** That includes a
  first-party material whose bucket had no kernel yet and a built-in material
  that becomes alpha-tested at runtime.
- **Before the first commit, compile errors surface from `commit_load`**, not
  from the setter that built the pass.

### What is still serial or wasteful

- Kernels remain the long pole, and the per-feature-set PBR kernels are large
  and mostly identical text. They also bake in the bucket count, their bucket
  index and their shader id, so any change to the bucket set recompiles all of
  them and identical kernels in different scenes never share a cache entry.
- A few pipelines live outside the pools and are still awaited where they are
  built: the SSR composite, the BRDF LUT, and the core blit / sRGB / mipmap
  helpers.
- The first frame still waits for every pipeline. Dispatch already skips a
  pass whose pipeline is missing, but committing with kernels outstanding
  needs a defined look for unshaded buckets first.

`examples/player-tests` locks the invariants (`pipelines-ready:<scene>`,
`config-ensure-keeps-kernels:<scene>`).

