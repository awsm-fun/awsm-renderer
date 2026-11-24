# 05 — Verification

TAA looks fine on a still screenshot even when it is broken. Every test here
involves motion, and every pass / fail is a number. Static screenshots and
"looks good to me" decide nothing.

## 1. Test scenes

New scenes under `examples/test-scenes/taa-*`, played back with a **fixed
timestep and fixed seeds** through the `player-tests` harness, so frame `k` is
the same frame on every run.

| Scene | Content | Catches |
|---|---|---|
| `taa-motion` | Textured cube translating and rotating at known speeds over a high-contrast checker floor; still camera | Object vectors, dilation, trails (G1, G3) |
| `taa-orbit` | Static scene, camera orbits, strafes, then stops | Camera vectors, jitter removal, blur after stopping (G2, G4) |
| `taa-skin` | A skinned walk cycle (CesiumMan or Fox) and a morph-target animation, side by side | Previous joint / morph data (G1) |
| `taa-reveal` | A wall slides away from a contrasting background, then back | Disocclusion (G6) |
| `taa-thin` | Wire fence, masked grass cards, a fine checker at grazing angle (moiré), a rotating clearcoat object with sharp highlights | Flicker, too much rejection, mip bias (G5, §7) |
| `taa-transparent` | Particles, glass, a UV-flow water plane and lines, over a moving background | Reactive mask (G8) |
| `taa-hud` | HUD meshes over a moving world | HUD excluded |
| `taa-cut` | Camera cut every 2 s; a teleporting object | Resets (G9, G10) |
| Dreidel Derby | The real game, mid-round, recorded input | Everything together; frame-time target |

## 2. Debug views (`TaaDebugView`)

| View | Shows |
|---|---|
| `Velocity` | Hue = direction, brightness = length (1 px/frame = full). Sky in grey |
| `ReprojectionError` | \|current − previous frame warped by v\| (§4.1), heatmap |
| `HistoryLength` | `n / N_max`, black to white |
| `Rejection` | R = off-screen reset, G = disocclusion, B = clip amount |
| `Reactive` | The reactive mask |
| `Confidence` | Upscaling only: `w_best` |
| `FreezeJitter` | TAA on, jitter fixed at 0: separates jitter bugs from rejection bugs |
| `HistoryOnly` / `CurrentOnly` | One side of the blend |

## 3. Ground truth

A **reference frame** for frame `k`: freeze time at frame `k`, render 64
frames with TAA off and jitter on (64 Halton phases), and average them with
the same Blackman-Harris weights the resolve uses. That is what a perfect TAA
would converge to for that instant, and it works for any scene and any frame,
including mid-motion. Build it as a harness mode in phase 0.

## 4. Tests

All colour errors are measured on the tonemapped, display-encoded image in
0–255 units.

### 4.1 Motion vectors (phase 1 gate)

- **Analytic:** in `taa-motion` and `taa-orbit` the expected vector of known
  pixels is computed on the CPU. Measured within ±0.02 px.
- **Reprojection error (every scene):** render with TAA off and jitter off;
  warp frame `k−1` by this frame's `v` into frame `k`. Over pixels that were
  visible in both frames (no disocclusion), mean error < 2 and the 99th
  percentile < 12. Skinned, morphed, instanced, vertex-hook and LOD-switching
  meshes are included. This is the one test that catches a wrong vector
  anywhere.

### 4.2 Ghosting (phase 2 gate, again at every later phase)

- **Trail:** `taa-motion` and `taa-reveal`. The trail band is pixels the
  moving object covered within the last 0.5 s but not now. Error against
  the reference: mean < 2 and max < 16 inside the band, at every frame.
- **Stop:** after motion stops, the whole image is within mean < 1 of the
  reference after 8 frames. A slower settle is history surviving that
  shouldn't.
- **Cut:** the first frame after a cut has no pixel from before it (compare
  against a TAA-off render of that frame: mean < 3, because it is
  aliased but not ghosted).

### 4.3 Convergence and flicker

- **Converged quality:** static `taa-thin` and Dreidel Derby after 32 frames:
  error against the reference is lower than MSAA 4×'s error against the same
  reference.
- **Flicker:** 60 frames of a static scene after convergence: mean
  frame-to-frame difference < 0.5, 99th percentile < 4. Measure in `taa-thin`
  especially. This is the test that stops "fix ghosting by shortening the
  history".
- **Camera-motion blur:** in `taa-orbit` during a slow pan (1 px/frame),
  error against the reference: mean < 3. Above that, the history filter is
  blurring (G4).

### 4.4 Reactive pixels and the HUD (phase 3 gate)

- `taa-transparent`: the trail test over particle, glass and UV-flow regions
  passes with the same thresholds.
- `taa-hud`: HUD pixels match a TAA-off render (mean < 1).

### 4.5 Upscaling (phase 4 gate)

- At 0.67: the converged and pan tests pass against the full-resolution
  reference, with thresholds relaxed to 1.5×.
- At 0.5: same, 2×.
- Changing the scale mid-run (0.67 → 0.5 → 0.67) causes no frame with mean
  error > 3 (no reset pop).
- `taa-thin` moiré: no visible moiré at the chosen `extra` mip bias; flicker
  test still passes.

## 5. Frame cost

Measured as in the 0.45 work: on an M1 Max in Chrome, repeat a pass N times and
take (T(N) − T(1)) / (N − 1), because Apple GPUs overlap pass timestamps.

| | Target | Notes |
|---|--:|---|
| Velocity target + second vertex evaluation | ≤ 0.2 ms | Dreidel Derby, 1920×1435 |
| Resolve at native 1920×1435 | ≤ 0.6 ms | |
| Resolve at 0.67 → 1920×1435 | ≤ 0.6 ms | It runs at post size either way |
| Dreidel Derby mid-round, TAA native | ≤ 11.5 ms | Against 10.7 ms with no AA and 14.9 ms with MSAA today |
| Dreidel Derby mid-round, TAA 0.67 | ≤ 7.5 ms | The point of the exercise |
| TAA off | unchanged | Zero cost when off |

The targets are estimates until phase 2 and 4 measure them. Record the real
numbers here and keep the table honest.

## 6. When a ghost appears: in this order

1. **`FreezeJitter`.** Gone? The jitter is leaking into the vectors or the
   reconstruction (G2). Still there? Go on.
2. **`Velocity` and `ReprojectionError` under the ghost.** A wrong vector
   (G1) or an undilated edge (G3). Fix the vector; never tune the blend to
   hide it.
3. **`Rejection`.** Should that pixel have been clipped (B) or flagged as
   disoccluded (G)? If not, check γ (G5) and τ (G6).
4. **`HistoryLength`.** `n` should drop where history is being clipped. If it
   stays high, `n_next` is wrong (03 §4.8).
5. **`Reactive`.** Is it a transparent, particle or moving texture that isn't
   flagged (G8)?
6. Only then the blend parameters, and re-run every test in §4, because they
   trade ghosting for flicker.

## Results

Recorded after the implementation on `feat/taa-upscaling`. The gates above
are unchanged; this section only reports against them. Harness:
`examples/taa-tests`, M1 Max, Chrome, 960x720, fixed timestep and seeds.
Native results: `a2be12b1` with the shipped tuning (bit-exact to
`3c975597`). Phase 4: `19acddee`. Phase 5: `3c975597`. The full record, with
every frame number and the evidence captures, was kept as
`gates-native-record.md` beside the report; `docs/TAA.md` §7 carries the
condensed tables.

### Method deviations

- **Reference space (§3).** The reference is averaged with the resolve's own
  Karis weights (tonemapped space), "what a perfect TAA converges to with the
  resolve's weights". A display-linear average was the first reading of §3;
  against it the converged image itself sits at 1.185 on the `taa-orbit` stop
  view, so the stop test could not pass even with a perfect history. Karis
  decides; linear is reported alongside, and the only verdict that differs is
  the `taa-orbit` stop test.
- **Reprojection error (§4.1).** 64-phase supersampled frames, warped with a
  Catmull-Rom filter; mixed-coverage pixels and content without vectors
  masked out; coverage ≥ 50% of the drawn pixels and of each moving object
  required. Thresholds 2 / 12 unchanged. The literal variant (no-AA frames)
  fails on aliased content even with exact analytic vectors (`taa-orbit` k90
  3.26 / 35, `instancing-stress` 8.07 / 68): the aliasing floor of the method.
- **Trail oracle (§4.2).** A resolve with perfect disocclusion runs on the
  same frames, to tell "TAA is wrong" from "no history method can do this".
- **Scenes.** `lod-nanite`'s bundle has no `clusters.bin`; `taa-cluster`
  replaced it. Dreidel Derby is a regression and frame-cost scene, not part of
  the harness gates.
- **Phase 4 stop** is INFO at scale (its floor is the upscaled converged
  image); flicker at scale is judged at the literal bound.

### §4.1 Motion vectors (phase 1)

| Test | Threshold | Measured | Verdict |
|---|---|---|---|
| Analytic, `taa-motion` / `taa-orbit` (incl. jittered G2 rows) | ±0.02 px | max 0.0037 px, 0 px over | PASS |
| Reprojection, every scene | mean < 2, p99 < 12 | worst `instancing-stress` 1.90 / 9, `prefab-skinned-morph` 1.48 / 9, `taa-hud` 1.03 / 8 | PASS |
| Reprojection, `taa-vertex-hook` k30 / k60 | same, coverage ≥ 50% | 0.25 / 0.27, but a thin grazing sliver is below coverage | FAIL (coverage) |
| Motion-history self-check, 48 scenes × 120 frames | 0 mismatching bytes | 0 | PASS |
| History burst / density / reset | clean, exact | 0 mismatching, max 0.0037 px | PASS 3/3 |
| Teleport (after `reset_motion_history`) | literal | passes | PASS |

### §4.2-§4.3 Ghosting, convergence, flicker (phase 2, native)

| Test | Threshold | Measured (Karis) | Linear | Verdict |
|---|---|---|---|---|
| Trail `taa-motion` | mean < 2, max < 16, every frame | 3.672 @k31, max 152 | 3.227 / 158 | NOT MET |
| Trail `taa-reveal` | same | 1.931, max 88 | 2.788 / 91 | NOT MET (max) |
| Trail `taa-skin` (added) | same | 7.425 @k2, max 135 | 7.082 / 142 | NOT MET |
| Stop `taa-motion` | mean < 1 after 8 frames | 0.606 | 0.805 | PASS |
| Stop `taa-orbit` | same | 0.773 | 1.080 | PASS (linear FAIL) |
| Cut, flagged (`taa-cut` k120) | < 3 vs TAA-off | 2.249 | | PASS |
| Cut, unflagged, guard fired (k240) | same | 1.883 | | PASS |
| Converged `taa-thin` | < MSAA 4x | 1.449 (MSAA 5.309) | 1.979 (MSAA 5.525) | PASS |
| Converged Dreidel Derby | < MSAA 4x | not measured | | NOT MEASURED |
| Flicker, 16 scenes | mean < 0.5, p99 < 4 | worst 0.292 p99 3 (`taa-transparent`), `taa-thin` 0.272 p99 2 | | PASS |
| Camera-motion blur (pan) `taa-orbit` | every strafe frame < 3 | worst 1.134 | 1.217 | PASS |

Trail, broken down: outside a 1.5 px ring around the current silhouette the
band is 1.999 (`taa-motion`, max 85), 1.655 (`taa-reveal`, max 41), 1.365
(`taa-skin`, max 72); the ring alone is 12.6 / 16.0 / 12.9. The oracle with
perfect disocclusion exceeds max 16 at the first uncovered frame on every
band measured (18-39, e.g. `taa-motion` k55 oracle 0.58 / 30, TAA 1.85 / 127,
MSAA 3.85 / 56), and is within it from age 2-3. "Max < 16 at every frame" is
not reachable by a history method on these scenes; the TAA ring deficit
against the oracle (contact pixels: TAA 3.13 / 127 against oracle 0.64 / 16)
is real and remains.

Converged, INFO scenes (TAA / MSAA): `taa-skin` 0.341 / 0.168 and
`instancing-stress` 1.179 / 1.033 are worse than MSAA; every other scene is
better (`taa-motion` 0.633 / 1.935, `taa-orbit` 1.037 / 3.075, `taa-reveal`
0.329 / 1.703, `taa-transparent` 0.690 / 2.866, `taa-hud` 0.939 / 2.497,
`taa-cut` 0.675 / 2.747, `taa-vertex-hook` 0.703 / 1.516, `taa-cluster`
0.607 / 1.045).

### §4.4 Reactive pixels and the HUD (phase 3)

| Test | Threshold | Measured | Verdict |
|---|---|---|---|
| `taa-transparent` trail, band ∩ particle / glass / UV-flow / line content | mean < 2, max < 16 | TAA 12.68 (avg 5.58); MSAA 10.78 (avg 6.22); no AA 18.06 | NOT MET (by any AA mode) |
| `taa-transparent`, the content's own trail | same | TAA 5.83, max 192 (avg 1.12); MSAA 5.35 (avg 3.09) | NOT MET (by any AA mode) |
| `taa-hud` HUD pixels vs TAA-off | mean < 1 | worst 0.068, max 1 | PASS |

### §4.5 Upscaling (phase 4)

| Test | Threshold | 0.67 | 0.5 | Verdict |
|---|---|---|---|---|
| Converged `taa-thin` | < 1.5x / 2x MSAA (5.309) | 2.991 | 5.759 | PASS; 0.5 only via 2x (above 1x MSAA) |
| Pan `taa-orbit` | < 4.5 / < 6 | 2.683 | 4.553 | PASS; 0.5 above the unrelaxed 3 |
| Flicker `taa-thin` | mean < 0.5, p99 < 4 | 0.292 p99 2 | 0.284 p99 2 | PASS |
| Flicker `taa-orbit`, `taa-motion`, `taa-reveal` | same | ≤ 0.149 p99 1 | ≤ 0.161 p99 2 | PASS |
| Scale change 0.67 → 0.5 → 0.67 | no frame mean > 3 | worst 4.589 @k82 | | NOT MET (literal) |
| Moiré / mip bias | no visible moiré; flicker passes | `mip_bias_extra` 0 chosen by sweep | | PASS (flicker); moiré judged by the sweep numbers, not by eye |
| Balanced 0.58 | | not measured | | NOT MEASURED |

The scale-change failure is the 0.5 steady state under orbit (a constant-0.5
run is itself 4.4-4.6), not a reset pop: the step down is better than
constant 0.5 for about 6 frames; the step up costs +1.033, decaying to +0.175
within 4 frames. Isolated sub-render-pixel features dropped from a still
`taa-thin` (INFO): native 0%, 0.67 5.0%, 0.5 73.0%.

### Phase 5 (32 B/sample fallback, forced on the M1)

Vectors 70/82: rigid scenes identical to the geometry path (analytic ≤ 0.0004
px). Not met, by design of the fallback: `taa-vertex-hook` analytic (max 2.14
px, hook motion absent), `prefab-skinned-morph` reprojection 7.56 / 6.05
(geometry 1.48 / 1.43); `taa-skin` passes degraded (0.496 against 0.177).
Trail, stop and flicker match the geometry path within noise (stop 0.606 /
0.773 / 0.942 / 0.247 / 0.489, all PASS; flicker all PASS). "Mobile path
works" on an actual 32 B/sample device: not measured (the Android device
check is outstanding). Governor: a render-scale step does not reset the
history (verified); the hitch is +0.4-0.6 ms CPU on the step frame,
indicative.

### Regressions

TAA-off baseline: 36/36 bit-exact to 0.45 (6 scenes × MSAA / SMAA / none × 2
frames). Player tests: 77/77. Regression sweep of Dreidel Derby, two
templates, the editor and the model tester: TAA-off output equal to 0.45
where compared; a pre-existing live-AA-switch bug in the model tester was
found and fixed (`7e8eac3f`).

### §5 Frame cost

| | Target | Measured | Verdict |
|---|--:|---|---|
| Velocity target + second vertex evaluation | ≤ 0.2 ms | pending quiet-window measurement (about 1 ms on `instancing-stress`, indicative) | not yet |
| Resolve at native 1920x1435 | ≤ 0.6 ms | about 1.1 ms by pass-timestamp slope after optimisation; pending quiet-window measurement | NOT MET as measured so far |
| Resolve at 0.67 → 1920x1435 | ≤ 0.6 ms | pending quiet-window measurement | not yet |
| Dreidel Derby mid-round, TAA native | ≤ 11.5 ms | pending quiet-window measurement | not yet |
| Dreidel Derby mid-round, TAA 0.67 | ≤ 7.5 ms | pending quiet-window measurement | not yet |
| TAA off | unchanged | pixel-identical; timing pending quiet-window measurement | not yet |

Also measured: RCAS about 0.21 ms, reactive seed about 0.03-0.04 ms (quiet);
whole frames, indicative: `kitchen-sink` no AA 9.09 / MSAA 11.27 / SMAA 9.08 /
TAA 11.83 ms, `instancing-stress` 6.65 / 13.63 / 7.29 / 9.79 ms.

### What is not met

1. Trail, every frame, max < 16: `taa-motion`, `taa-reveal`, `taa-skin`. The
   oracle shows max < 16 unreachable at the first uncovered frame; the mean
   bound is also missed on `taa-motion` and `taa-skin`, mostly within 1.5 px
   of the silhouette.
2. Phase 3 trail over particle, glass, UV-flow and line content: not met by
   TAA, MSAA or no AA.
3. Reprojection coverage on `taa-vertex-hook` (a thin grazing sliver).
4. Scale change 0.67 → 0.5 → 0.67, literal: the 0.5 steady state exceeds 3.
5. At 0.5, converged and pan pass only through the 2x relaxation; the
   converged image is above native MSAA's error.
6. Converged quality worse than MSAA on `taa-skin` and `instancing-stress`
   (INFO scenes).
7. Not measured yet: Dreidel Derby converged quality, Balanced 0.58, every §5
   frame-cost row in a quiet window, the fallback on a real 32 B/sample
   device.
