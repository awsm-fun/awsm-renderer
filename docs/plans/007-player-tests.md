# 007 — Player runtime test projects (`examples/player-tests/`)

**Order:** seventh — consumes the baked bundles from 006's `examples/test-scenes/`.

## Why
The editor exercises authoring; nothing systematically exercises the **player**
consumption path — `scene-loader` + the bundle packages driving a game at runtime,
which is the product's actual point. The 006 sweep optimizes for players; this plan
proves it where players live.

## What
A new `examples/player-tests/` app (same shape as `examples/render-worker` /
`multithreaded`: small wasm frontends + a taskfile entry) that loads **test-scene
bundles** and runs scripted runtime scenarios with pass/fail output readable from the
browser console (so an agent can assert on it headlessly via chrome-devtools; renderer
`tracing::info!` lands in the BROWSER console).

### Scenarios (minimum)
1. **Bundle load transaction** — load each test-scene bundle cold; assert ONE
   begin→declare-all→commit, record load-time ms + peak memory; fail on any
   double-commit or per-node commit (regression net for 006 axis 2).
2. **Large instancing** — `instancing-stress` bundle: spawn/tear-down thousands of
   instances at runtime, with and without particle systems running; frame-time budget
   asserted under `?stress=N`.
3. **Nanite streaming** — `lod-nanite` bundle under `?stream` / `?streambudget=N`:
   orbit a scripted camera, assert resident tris stay ≤ budget, cut changes with
   distance, no holes (readback counts, not just pixels).
4. **Prefab instantiation at runtime** — instantiate/destroy prefab clones (static +
   skinned+morph) in a loop; assert geometry buffer count stays flat (006 axis 4),
   per-clone animation advances independently, no leak across 1000 cycles.
5. **Animation stress** — many skinned + morph + blended clips simultaneously; frame
   budget + correctness spot-checks (bone/world positions sampled at known times).
6. **Startup census** — plain renderer instantiation + empty scene: assert the 006
   axis-1 pipeline/shader/texture floor numbers; any growth fails loudly.
7. **Post/SSR/bloom runtime toggles** — flip post-process settings at runtime from the
   player API; assert structural recompiles happen off the frame spike path
   (async compile) and settings apply.

### Infra
- `task player-tests` serves the app + the baked bundles (re-bake test scenes via the
  editor export path as part of the task, or check in the baked bundles — prefer
  re-bake so export stays covered).
- Each scenario prints a single machine-readable `PLAYER-TEST <name>: PASS/FAIL {json}`
  line; a driver script (or the agent) collects them.
- Document everything in `examples/player-tests/README.md`, cross-linking
  `examples/test-scenes/README.md`.

### Acceptance
All scenarios PASS headlessly from a cold checkout with documented commands; numbers
(load times, frame times, census) recorded in the README next to the 006 baselines.
