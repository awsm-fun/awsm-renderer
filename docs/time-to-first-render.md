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
