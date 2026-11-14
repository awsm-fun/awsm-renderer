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
(`materials-overhaul`; since 0.41 PBR kernels are no longer split on the
features that are free to share, see the 0.41 update at the end): a project that
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
  and mostly identical text. (0.41 stops splitting them on the features that
  are free to share; specular and the lobes still get a kernel each — see the
  0.41 update below.) They also bake in the bucket count, their bucket
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
- Nothing is awaited ahead of the batch any more (0.41; prepared as 0.40.1). The blit pair, the
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

## UPDATE (0.41) — PBR kernels are split only on the features worth a kernel

The batch is throughput-bound at roughly two seconds per megabyte of WGSL,
and most of that megabyte count is opaque PBR kernels: ~240 KB each, one per
distinct feature set. Measuring the features themselves showed how little of
a kernel they are (a kernel with none is 239,552 bytes), and measuring the
GPU showed which of them are free to carry in a kernel whose materials do
not use them:

| Feature | WGSL it adds | Opaque pass when carried unused (`ssr-arena`, 1080p, 2.18 ms without) | 0.41 default |
|---|---|---|---|
| IOR | 0 bytes | 2.19 ms | every kernel |
| emissive strength | 66 | 2.04 ms (one kernel instead of two) | every kernel |
| vertex colour | 385 | 2.22 ms | every kernel |
| dispersion | 7 | 2.18 ms | splits |
| transmission | 16 | 2.45 ms (+12 %) | splits |
| volume | 47 | 2.46 ms (+12 %) | splits |
| specular | 19 | 2.49 ms (+14 %) | splits |
| all seven | 540 | 3.58 ms (+64 %) | — |
| diffuse transmission | 1,527 | | splits |
| anisotropy | 2,853 | changes the image | splits |
| clearcoat | 2,935 | | splits |
| iridescence | 3,144 | | splits |
| sheen | 3,414 | | splits |
| secondary maps | 6,309 | | splits |

Bytes are not cost. The seven smallest features are 540 bytes together and,
compiled into every kernel, make the opaque pass 64 % slower on that scene —
the unused code is not executed, but it is no longer dead: values that were
compile-time constants (specular 1, transmission 0) become loads, an F0 and
f90 that folded are recomputed per light, four more texture slots get their
UVs interpolated, and the transmission branch (refraction, up to three
environment fetches) is live code the compiler must keep registers for.
(Which of those accounts for how much was not separated; the per-feature
rows are what was measured.)

So `PbrSpecialization` goes by the measurement:

- `PbrFeatures::cheap()` — IOR, emissive strength, vertex colour: compiled
  into EVERY opaque PBR kernel under the default policy
  (`ExpensiveFeatures`). Free to compile (451 bytes) and free to run.
- everything else splits: the four that are neutral but not free (specular,
  transmission, volume, dispersion) and the six lobes.

The variant reconcile keys a material's bucket on
`policy.kernel_features(material_features)` =
`material ∪ always_compiled`. Nothing in the templates changed: the payload
already carries an index per extension and the `pbr_material_load_*` loaders
return the glTF-absent value at index 0.

`LobesOnly` is the other point on the curve — all seven of the small
features (`PbrFeatures::neutral()`) in every kernel, only the six lobes
split: one kernel fewer per pair of materials that differ only in those, and
the frame cost above. `EveryFeature` is 0.40.

Transmission, volume and dispersion look like candidates for "compile into
no opaque kernel" — a material that transmits is drawn by the transparent
pass, so for a committed scene that code is a branch never taken. It is not
done, because it is reachable: a material that carries the extension at
factor 0 is opaque, and when the factor is raised on a running scene its
mesh stays in the opaque pass — the mesh was built for that pass, and a
commit does not move it — where the kernel's environment-only transmission
is all that can show the edit. Under the default policy such a material
keeps a kernel with that code, as in 0.40.

### Cold loads

M1 Max, Chrome 157, Metal, cold GPU caches (fresh profile, never-used site),
alternating runs, medians; navigation to the first frame (`player-tests`
`?soak=<scene>&pbr=every|expensive|lobes&msaa=0|1`).

| Load | MSAA | `EveryFeature` (= 0.40.1) | **default** | `LobesOnly` | PBR kernels | WGSL compiled |
|---|---|---|---|---|---|---|
| DANCE-OFF stage (player), 6 runs | on | 8.22 s | **6.75 s** | 6.36 s | 5 / 3 / 2 | 2.65 / 2.18 / 1.94 MB |
| DANCE-OFF stage (player), 6 runs | off | 7.34 s | **6.35 s** | 6.05 s | 5 / 3 / 2 | 2.66 / 2.25 / 2.03 MB |
| `pbr-extensions`, 6 runs | on | 8.71 s | **8.48 s** | 7.96 s | 8 / 7 / 6 | 3.34 / 3.11 / 2.88 MB |
| `pbr-extensions`, 6 runs | off | 7.60 s | **7.24 s** | 6.88 s | 8 / 7 / 6 | 2.99 / 2.79 / 2.59 MB |
| `ssr-arena`, 6 runs | on | 6.20 s | **5.89 s** | 5.73 s | 2 / 1 / 1 | 1.69 / 1.46 / 1.46 MB |
| `ssr-arena`, 6 runs | off | 5.80 s | **5.27 s** | 5.35 s | 2 / 1 / 1 | 1.50 / 1.30 / 1.30 MB |
| `kitchen-sink`, 3 runs | on | 5.77 s | **5.80 s** | 5.79 s | 1 | +451 B |
| `kitchen-sink`, 3 runs | off | 5.32 s | **5.32 s** | 5.30 s | 1 | +451 B |
| `alpha-cutoff`, 3 runs | on | 5.46 s | **5.41 s** | 5.29 s | 1 | +451 B |
| `alpha-cutoff`, 3 runs | off | 4.79 s | **4.89 s** | 4.78 s | 1 | +451 B |
| `contact-shadows`, 3 runs | on | 4.00 s | **4.02 s** | 4.11 s | 1 | +451 B |
| `contact-shadows`, 3 runs | off | 3.63 s | **3.67 s** | 3.66 s | 1 | +451 B |

The test scenes are one build under its three policies. DANCE-OFF is the real
player, which has no policy switch, so its three columns are three builds of
it: against the 0.40.1 candidate, against this release, and against this
release's predecessor on the branch, which compiled the kernels `LobesOnly`
compiles. The player turns MSAA off by default on a touch device.

DANCE-OFF run by run, MSAA on: 8.25 / 8.10 / 8.24 / 8.32 / 8.02 / 8.20 s
before; 6.75 / 6.70 / 6.70 / 6.83 / 6.76 / 6.85 s now. A scene with a single
PBR feature set gains nothing and loses nothing measurable: the six rows at
the bottom move by 0.1 s either way, inside their own run-to-run spread.
(The machine was busier than for the 0.40 notes; compare columns, not
releases.)

### Frame cost

Apple M1 Max (not a phone), Chrome 157, Metal; `timestamp-query` on every
pass, unquantized (`--enable-webgpu-developer-features`; by default Chrome
rounds timestamps to 2^16 ns); frames run back to back, each started when
the GPU has finished the previous one, so the frame interval is CPU + GPU
time and not vsync. Six alternating rounds of ~4 s, medians.

| Scene | Resolution | Opaque pass: `EveryFeature` / default / `LobesOnly` | Frame: `EveryFeature` / default / `LobesOnly` |
|---|---|---|---|
| `pbr-extensions` | 1920×1080 | 1.69 / **1.61** / 1.95 ms | 9.40 / **9.20** / 9.60 ms |
| `pbr-extensions` | 3840×2160 | 3.58 / **3.57** / 4.78 ms | 22.9 / **22.9** / 24.1 ms |
| `lights-many` | 1920×1080 | 1.34 / **1.31** / 1.69 ms | 9.70 / **9.60** / 9.65 ms |
| `lights-many` | 3840×2160 | 2.83 / **2.91** / 4.04 ms | 18.9 / **19.0** / 20.4 ms |
| `ssr-arena` | 1920×1080 | 2.15 / **2.14** / 3.61 ms | 11.2 / **11.2** / 12.7 ms |
| `ssr-arena` | 3840×2160 | 6.36 / **6.27** / 9.62 ms | 31.3 / **31.2** / 34.9 ms |
| `kitchen-sink` | 1920×1080 | 0.91 / **0.95** / 1.27 ms | 8.30 / **8.70** / 8.90 ms |
| `kitchen-sink` | 3840×2160 | 2.79 / **2.90** / 3.95 ms | 20.2 / **20.4** / 21.5 ms |

The default against `EveryFeature`: the opaque pass moves by −0.09 to +0.11 ms,
which is −0.8 to +0.5 % of the frame. `LobesOnly` against `EveryFeature`: the
opaque pass is 15–68 % longer (+0.26 to +3.26 ms), 2.7–13 % of the frame.

The DANCE-OFF player, measured the same way inside its own page at 1920×1080
(the loaded stage, three builds as in the cold-load table, six rounds): the
frame takes 37.8 / **35.5** / 43.8 ms and the opaque pass 9.4 / **9.4** /
15.8 ms. The default costs nothing there either; `LobesOnly` costs 16 % of
the frame.

(The 3840×2160 rows could only be measured once a dispatch bug was fixed: an
indirect dispatch of more than 65,535 workgroups in one dimension is silently
skipped, so a bucket that covered more than 65,535 tiles — the sky, at that
size — was not shaded at all. The kernels are now dispatched in rows of
32,768 tiles; grep `classify-dispatch-row`.)

Three things are worth keeping from this:

- **The opaque pass gets faster with fewer kernels**, all else equal: each
  bucket's kernel is dispatched over the tiles that contain it, so two
  kernels that share tiles cost more than one that shades both.
- **Render-pass timestamps on this GPU are not additive.** A tile-based GPU
  defers a render pass's fragment work, and the begin/end timestamps of
  consecutive render passes overlap (their sum is several times the frame).
  Compute-pass timestamps and the unthrottled frame interval agree with each
  other to within a few percent; those are the numbers above.
- **The frame interval has a run-order effect.** Each run is a fresh browser,
  and the arm that runs first in a round reads 0.2–0.4 ms lower at 1080p:
  `kitchen-sink` shows the default 0.4 ms slower than `EveryFeature` in the
  table (run second) and 0.2 ms faster in eight more rounds run first (8.35
  against 8.55 ms). The opaque pass's own time does not move with the order.

### What stays out, and why

- **Specular** is neutral (a material without it renders identically through
  a kernel that has it) but costs 14 % of the opaque pass, 2.6 % of the
  frame. Most of that is probably an F0 / f90 recomputed per light once
  `specular` is not a constant; computed once per pixel it could be free and
  join the shared set (DANCE-OFF would then compile two PBR kernels).
- **Clearcoat, sheen, iridescence, diffuse transmission.** Compiled into
  every kernel ungated they cost 7–29 % of the frame (the opaque pass
  roughly doubles). Gated at runtime on the material having the lobe,
  clearcoat and iridescence still cost 2.5–11 % on top of `LobesOnly`, and
  the output stops being bit-identical: with the branch in place Metal's
  fast-math compiler orders the surrounding sums differently, a handful of
  HDR values per frame move by one half-float step and occasionally one
  8-bit pixel by 1. Under `strictMath` (a Chrome developer feature, not
  available to a shipped page) the two are bit-identical, which is how the
  cause was established.
- **Anisotropy** replaces the isotropic lobe; compiled in, it changes
  materials without it (90 pixels, up to 162/255).

### How "renders the same" was checked

Screenshots are the wrong tool: SSR's march jitter rotates every frame, so
two captures a frame apart differ by 1/255 in ~0.4 % of `ssr-arena`'s pixels
in any build, and a capture taken "1.5 s after load" lands on whatever frame
that happens to be. The comparison that settled it freezes the harness after
exactly N rendered frames (an injected `requestAnimationFrame` wrapper),
reads every live render target back (`copyTextureToBuffer`; the HDR targets
are `rgba16float`) and compares bytes. On the same frame, all three policies
and the 0.40.1 candidate produce identical targets in all 23 single-commit
scenes. `lights-many` alone differs from itself between two runs of one
build (a few HDR values; its 8-bit image never differs).

`player-tests` locks the policy with `pbr-cheap-features-share-a-kernel`
(sixteen materials, fifteen feature sets: eleven buckets by default, five
with `LobesOnly`, fifteen with `EveryFeature`, and the three renders compared
pixel for pixel) and `pbr-cheap-feature-edit-keeps-kernels`.
