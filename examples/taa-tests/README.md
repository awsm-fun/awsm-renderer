# examples/taa-tests — the TAA test harness

The numeric half of [`docs/plans/taa-upscaling/05-verification.md`](../../docs/plans/taa-upscaling/05-verification.md):
deterministic playback of test-scene bundles, bit-exact frame capture, the
jittered ground-truth reference, the image metrics, and the TAA-off pixel
baseline. Every check prints one machine-readable line (player-tests
conventions, `TAA-TEST` prefix):

```
TAA-TEST <name>: PASS — <numbers>
TAA-TEST <name>: FAIL — <numbers>
TAA-TESTS COMPLETE: <pass>/<total>
```

All lines are mirrored to the page's `#log` element, the aggregate to `#hud`
(`taa-tests: COMPLETE <pass>/<total>`), so a driver reads the whole table from
the DOM: `document.getElementById('log').textContent`.

## Running

```sh
task test-scenes                                     # bundles on :9084
python3 examples/taa-tests/capture-server.py <dir>   # capture store on :9095
cd examples/taa-tests && trunk serve --port 9093     # the harness
```

`capture-server.py <dir> [port]` maps `PUT/GET /<set>/<file>.png` to
`<dir>/<set>/<file>.png` (local only, 127.0.0.1, `cache-control: no-store`).
Captures are PNG, so they round-trip bit-exactly. The same script serves
`examples/test-scenes` uncached for authoring loops
(`capture-server.py examples/test-scenes 9099`, then `&bundles=http://localhost:9099`):
`task test-scenes` sends `max-age=2500`, so a re-baked bundle is otherwise not
seen by a reloaded page.

Features: `taa-hooks` (the `awsm_renderer::taa` hooks), `taa-phase1` (the
`debug_read_velocity` / `debug_read_depth` readbacks) and `taa-phase2`
(`set_taa_debug_repeat`) are on by default; `taa-schema` (the scene schema's
`anti_aliasing.taa`) is for trees after that schema change. The
0.45 baseline build is `--no-default-features`; a phase-0 build is
`--no-default-features --features taa-hooks` (trunk:
`data-cargo-no-default-features data-cargo-features="taa-hooks"`).

## Determinism

- Fixed timestep: frame `k` is time `k/60 s`. `set_time_source(k/60)`,
  `update_animations(1000/60)` and `ParticleDriver::tick(1/60)` every frame
  (0 on frame 0 and on frozen re-renders). Nothing reads the wall clock.
- Fixed seeds: the loader's particle driver seeds emitters by sorted index.
- Fixed frame count: exactly `warmup` (default 8) renders of frame 0 before
  the scenario; between them the harness waits for the GPU and for all
  pending pipeline compiles *without rendering*, so the number of `render()`
  calls before frame `k` never depends on compile timing (SSR / volumetrics
  temporal state keys off it).
- Fixed size: canvas backing store 960×720 by default (`?size=1920x1435` for
  cost measurements). `devicePixelRatio` never enters.
- Capture: GPU `copyTextureToBuffer` of the swap-chain texture right after
  `render()` (`copy_texture_for_readback`, canvas configured with
  `COPY_SRC`), 8-bit, display-encoded, alpha forced to 255. No
  `toBlob`/`drawImage` resampling or colour management in the path.

## Modes (`?mode=`)

| Mode | What it does |
|---|---|
| `determinism` | Plays each scene `runs` times from fresh renderers, captures `frames`, requires bit-identical captures. |
| `baseline-record` | Captures `frames` of `scenes` × `aa` into store set `set` (`<scene>-<aa>.png` = last frame, `<scene>-<aa>-kNNN.png` the others). |
| `baseline-check` | Captures again and requires max per-pixel error ≤ `tolerance` (default 0) against `set`. Failing (or `&keep`) captures + ×16 diff images go to `<set>-check/`. |
| `reference` | Plays to each `k`, freezes, renders `phases` (64) Halton-jittered frames with TAA off and accumulates them (03 §4.2 kernel). Stores `ref/<scene>-kNNN-{display-linear,karis,aliased}.png` and reports the aliased-vs-reference error and the bias between the two accumulation spaces. Needs `taa-hooks`. |
| `hooks` | The renderer hooks the reference relies on: the harness's jitter / kernel mirrors equal `awsm_renderer::taa`'s; a frozen re-render is bit-identical; a zero jitter override is neutral; jitter `+j` moves content by `+j` px (x right, y down; measured with MSAA). Needs `taa-hooks`. |
| `vectors` | The 05 §4.1 phase-1 gate over the TAA scenes + the animated / instanced / LOD scenes (see below). Needs `taa-phase1`. |
| `motion-history` | The renderer's byte-exact previous-state self-check (`debug_verify_motion_history`: transforms, joints, morph weights, instance transforms) every frame for `frames` (default 120) frames of EVERY bundled scene; PASS only with 0 mismatching bytes and > 0 bytes compared. Reports the meshes' `skin_update_period` histogram. |
| `history-burst` | `gates=burst,density,reset`: 40 new transforms in non-adjacent recycled slots while the cube moves (taa-motion); `burst_n` new transforms in a scene reduced to 3 transforms (fresh ≥ 60 %); `reset_motion_history` on 2 of 3 skinned prefab clones while the third walks. Self-check clean on k−1…k+3 and the moving object's vectors right on the event frame. |
| `aa-toggle` | `taa-vertex-hook`: none → TAA → MSAA → TAA at runtime (`set_anti_aliasing` + `commit_load`), each frame bit-identical to a fresh session of that mode; the hook mesh moves. |
| `gate` | `gates=ghosting` (trail, stop, cut) / `convergence` (converged, flicker, pan) or any list; TAA vs lockstep 64-phase references (05 §4.2–4.3). `relax=1.5` for 0.67 scale, `stride=1` for every frame of the trail. taa-cut's switch at 120 is flagged (`reset_temporal_history`), at 240 unflagged (the camera-jump guard must fire). `converged` also prints `converged-still` for a scene that is moving at k32 (the frame held for 40 frames with time frozen: the converged image of that instant). Failing lines store 05 §6 evidence (FreezeJitter, Velocity, ReprojectionError, Rejection, HistoryLength) under `phase2/`. Scene `<name>@cadence`: coverage LOD on and the skins at update periods 2 / 4 / 1. |
| `trail-oracle` | The trail band of a scene without a tracked box (`taa-skin`; `moving=a,b` names the nodes), from REAL coverage (depth with and without those nodes, jitter frozen) instead of the gate's `|v| > 0.05 px`, with the oracle, no-AA and MSAA beside TAA, split by ring / age, and the gate's vector band split into pixels the object still covers and pixels it left (INFO). `k=` stores tiles under `r2/trail-oracle/`. |
| `trail-evidence` | Diagnostics for a failing trail (INFO): at `k=…`, the band split by age (frames since the tracked object left the pixel) and by cause (≤ 2 px from the silhouette; the object's measured shadow footprint), TAA / no-AA / MSAA vs the same reference; the worst pixel with its Rejection / HistoryLength / ReprojectionError / Velocity debug values; full-frame tiles under `phase2/evidence/`. |
| `speeds` | Screen speed (px/frame) per scenario phase from the velocity readback. |
| `reference-floor` | Static frame: the 64-phase reference vs the resolve's own convergence model (8 phases, per-frame normalisation, clamped borders), and the converged TAA frame vs both — the floor under the stop / trail thresholds. |
| `disocclusion` | taa-motion `k=32,55`: the resolve's disocclusion test replayed on the CPU (`disocc_def=v1…v5` = the definitions of 4809e43a / 8298170a / 757ad193 / 8d87a418 / a2be12b1) from the depth + velocity readbacks, against the LIVE Rejection view; with `taa-debug-raw`, the raw views 10-13. |
| `disocclusion-static` | Raw view 10 after 29 static frames, then the cube's old footprint on its first moving frames (`taa-debug-raw`). |
| `static-rejection` | Static scenes (time frozen, jitter cycling): disoccluded px per frame in the live Rejection view, per-frame series, hit map + burst-frame mask (`phase2/static-rejection/`), the burst split by test (CPU replica) and its jitter pair. |
| `rejection-motion` | taa-orbit under camera motion: per frame test-1 / test-2 flags (replica) and the shader's; pan error within 2 px of a parallax silhouette vs the rest. |
| `phase3` | 05 §4.4: taa-transparent trail over band ∩ particle/glass/UV-flow content and the content's own trail (TAA / MSAA / no-AA, every frame); taa-hud HUD px vs TAA-off (mean < 1, every frame). |
| `tune-sweep2` | Motion tuning by coordinate search from the defaults (`knobs=name:v|v;…`, `best=`, `configs=`): trail taa-motion / taa-reveal (literal, minus ring, ring, contact, rest), taa-orbit stop / pan / strafe motion flicker, taa-thin converged + flicker, taa-orbit flicker. Needs `refcache=`. |
| `reactive-sweep` | reactive_transparent × reactive_material (× reactive_gamma_scale with `taa-experimental`) on taa-transparent: static flicker per content region, the 05 §4.4 trail per region, each region's own trail; `particles` flicker. |
| `scale-change`, `mip-bias`, `governor-hitch`, `sharpen-sweep`, `upscale-sweep`, `gamma-sweep` | Phase 4 (`taa-phase4`; `gamma-sweep` needs `taa-tuning-v2`): see src/phase4.rs. `?scale=` sets the TAA render scale of every TAA session (references stay full resolution). `gamma-sweep&values=…[&motion]`: gamma_still vs thin / isolated-detail error and dropout, static flicker, and with `motion` the taa-motion trail + stop and taa-orbit pan + stop. |
| `frame-cost` | GPU frame time from the timestamp queries (`profiling::gpu_frame_timing`) over `cost_frames` frames; `runs` interleaved rounds, median + spread per configuration, and a sentinel (first scene, no AA) before and after every run to show drift. `&busy` (use it: Apple GPUs clock down when drained every frame) measures a window of back-to-back frames drained every 4th. `variants=none,msaa,smaa,taa,taa@0.67,taa@0.5,taa@0.67s0.25,taa-fb` (scale, RCAS strength, forced fallback); `repeat=1,8,32` measures the resolve of native TAA by repetition, by frame slope and by "Taa" pass-timestamp slope, and TAA − none − resolve; also the RCAS pass, the transparent pass TAA − none (holds the reactive seed) and fallback − native. Use `size=1920x1435`. Builds against v0.45.0 with `--no-default-features` (none / MSAA / SMAA only). |

`?tune=name:value,…` overrides TaaTuning fields (and, with `taa-experimental`, `clip_alpha` / `reactive_gamma_scale`) for every TAA session; the session re-applies its TAA config after the scene load (the loader applies the scene's authored AA) and refuses to open unless the renderer holds exactly that config. `?refcache=<name>` caches the 64-phase references in the store (they do not depend on TAA tuning).

URL params: `scenes=a,b`, `aa=msaa,smaa,none,taa`, `size=WxH`,
`frames=5,60`, `k=30,90`, `runs=2`, `warmup=8`, `phases=64`, `set=…`,
`tolerance=0`, `keep`, `bundles=<origin>` (default `http://localhost:9084`),
`store=<origin>` (default `http://localhost:9095`).

## The TAA-off pixel baseline (zero-cost-off proof)

The baseline is recorded from the **0.45 release** with the harness built
`--no-default-features` (no `taa-hooks`, which 0.45 does not have), from a
`git worktree` of `5a8077e7`. Any later commit is checked with

```
http://localhost:9093/?mode=baseline-check&set=baseline-0.45
```

(6 scenes × MSAA 4× / SMAA / no-AA × frames 5 and 60 = 36 checks, ~15 s).
The noise floor between two identical runs is 0 (bit-exact), so the check is
strict: max per-pixel error 0.

## Metrics (`src/metrics.rs`, unit-tested natively)

`cargo test -p awsm-renderer-taa-tests-example --lib`

- Per-pixel error: max |Δ| over R, G, B in 0–255 (alpha ignored). Stats over a
  mask: mean, max, exact 99th percentile (nearest rank), non-zero count;
  histograms merge across frames.
- `frame_to_frame`: per-pair stats + merged (flicker test).
- `rasterize_convex` + `convex_hull`: exact conservative coverage of a
  convex object (the tracked boxes) from its projected corners;
  `trail_band(recent, now)`: covered in the last 30 frames, not now.
- `warp_by_velocity`: warps frame k−1 into frame k by a UV velocity field
  (bilinear), with a validity mask (in bounds and visible in both frames,
  from `depth_visibility_mask` or `velocity_consistency_mask`).
- `estimate_shift_x/y`: sub-pixel image shift (jitter-convention check).

## The vector gate (`?mode=vectors`, src/velocity.rs)

Per scene, lockstep sessions: **A** TAA + `FreezeJitter` (vectors + depth via
the debug readbacks), **B** TAA with a per-frame Halton jitter override (G2),
**R** TAA off (plain frames + 64-phase references), **H** TAA off with the
no-vector content (emitters, lines, glass, UV-flow) hidden. At each evaluated
frame k (and k−1):

- `vectors-analytic`: every drawn pixel's vector against the exact expected
  one, from its depth, VP_k / VP_{k−1} and the scene's motion model (rigid
  tracked box; static world; the vertex-hook wave as rasterised flat facets).
  Bound: 0.02 px, or one rg16float ulp of v when larger (teleports).
- `vectors-reprojection-ss` (the gate): reference(k−1) warped by v_k
  (Catmull-Rom) against reference(k), mean < 2, p99 < 12, over the refined
  mask: visible in both (in bounds, drawn, forward-backward consistent ≤ 1 px,
  depth-continuous ≤ 5 %), no mixed pixel (3×3 depth jump > 5 % or vector
  spread > 0.25 px, in k or around the reprojected position in k−1), no
  no-vector content (R vs H diff, dilated 1 px). Coverage < 50 % is reported as
  insufficient. Variants (bilinear, the plain visible-in-both mask, the literal
  no-AA frames) and the same warp with ANALYTIC vectors (control) are INFO lines.
- `vectors-g2-analytic` / `vectors-g2-ss`: B's jittered vectors against the
  analytic motion of the surface each pixel shows (q + 0.5 − j), and resampled
  to the unjittered grid driving the same warp.
- Coverage: kept / drawn ≥ 50 %, AND over the MOVING pixels (|v| > 0.5 px)
  per 4-connected moving component of ≥ 200 px, each ≥ 50 % kept, or, for a
  component the warp cannot measure (a strip a few pixels wide has no pixel
  whose 3×3 is free of its own silhouette), ≥ 99 % of its pixels verified by
  the analytic test (scenes with an analytic model only). The moving
  pixels excluded ONLY by the forward-backward consistency test are reported
  (`vectors-consistency-excluded`) with the warp error there using v_k as-is
  (a wrong vector would show there), against a per-scene bound.
- `vectors-object*` (skinned scenes, `&sweep` for all): per object (k-means on
  world position), the error for 0…2× v; the minimum must be at 1×; fewer
  than 200 kept moving pixels is a FAIL ("insufficient moving pixels").
- `vectors-lod-switch` (lod-*, taa-cluster): a deterministic pre-pass finds the
  frames where the discrete level or the GPU cluster cut (read back from the
  cluster pass's `selected` flags) changes; the gate runs across them.

## Lockstep

Sessions compared frame by frame (test vs reference) see the same scene state
at frame k. Frozen re-renders (64-phase references, stop / flicker holds) must
re-render the last normally rendered frame (so they never step into a cut) and
restore `renderer.frame_index`, so frame-index cadences (skin
`skin_update_period`) agree across sessions. Lockstep sessions refuse SSR
temporal, volumetric temporal and coverage LOD (`assert_lockstep_safe`):
those key on internal render counters or asynchronous readbacks.

## Scenarios (`src/scenario.rs`)

Scripts keyed by scene name drive nodes by the names `author.js` gives them,
relative to their authored transforms, as pure functions of `k`. Phases are
labelled (`still`, `moving`, `stopped`, …) so gates can select frame ranges.
Unknown scenes (the existing test scenes) get a slow orbit about their bounds.
