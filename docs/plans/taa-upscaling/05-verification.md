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

**Which average decides.** The frames are averaged as displayed
(display-linear), which is the literal reading of the paragraph above. Since
round 2 the resolve averages in the display's space too (`docs/TAA.md`
§3.5), so this is also "the reference in the resolve's own space", the rule
round 1 judged by when the resolve averaged Karis-weighted scene colour.
The Karis-weighted reference (`ref=karis`, round 1's deciding one) is
reported beside every number.

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
  **Restated (maximum only; the mean is unchanged).** Round 1 showed "max
  < 16" to be wrong as written: a perfect-information oracle (exact vectors,
  exact disocclusion knowledge, the resolve's own kernel) reads a band
  maximum of 18-39 on the first frame a pixel is uncovered, because one
  sample cannot be within 16 of a 64-sample reference on a high-contrast
  edge. The maximum is therefore held against methods that keep no history
  and so cannot ghost: on every frame, TAA's band maximum may not exceed the
  larger of MSAA 4×'s and no-AA's maximum on the same band. (That bound is
  generous on a checker, where no AA aliases by 55-60 on every frame, and
  strict on a smooth background, 13-33 on `taa-skin`: it follows the
  content.) The oracle's
  maximum is printed beside it, and the original "max < 16" number stays in
  every report (`trail-max-restated`, `trail-max-compare`, `trail-ghosts`).
  The mean bound is not restated: where the oracle itself is at or above 2
  (a band made only of first-frame pixels) the line fails and says so
  (`trail-mean`, `trail-mean-vs-oracle`).
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
  **Restated.** Round 1 showed that MSAA 4× and no AA miss these thresholds
  on this content too (worst frame 10.8 and 18.1 against TAA's 12.7), and
  reactive pixels keep no history by design, so "the same thresholds"
  measures the content, not the AA. TAA is held against the other modes
  instead, over the band ∩ content and over the content's own trail: its
  run average may not exceed MSAA 4×'s, and its worst frame may not exceed
  no-AA's worst frame (`phase3-trail:…-restated`): the same logic as the
  trail maximum, a method without history can be no worse than no AA. The original line and
  its numbers stay in every report.
- `taa-hud`: HUD pixels match a TAA-off render (mean < 1).

### 4.5 Upscaling (phase 4 gate)

- At 0.67: the converged and pan tests pass against the full-resolution
  reference, with thresholds relaxed to 1.5×.
- At 0.5: same, 2×.
- Changing the scale mid-run (0.67 → 0.5 → 0.67) causes no frame with mean
  error > 3 (no reset pop).
  **Restated.** The line was meant to catch a reset pop, but "mean > 3" is
  exceeded by the 0.5 steady state itself on this scene (round 1: a
  constant-0.5 run reads 4.4-4.6). It is restated against each scale's own
  steady state: no frame may exceed a constant-scale run at the same scale
  and frame by more than 1.5, and from 8 frames after a step on by more than
  0.25 (`scale-change-restated`). The original line and its number stay in
  every report.
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

Two rounds. Round 1's record (`2da7b171`) is kept in git history; this section
is the state after round 2. The gates above are unchanged except for the three
"Restated" paragraphs, each of which keeps its original line. Harness:
`examples/taa-tests`, M1 Max, Chrome, DPR 2, 960x720 (cost at 1920x1435),
fixed timestep and seeds, every frame. Renderer `34b6dc92` (later commits
change formatting, the harness and docs only). `docs/TAA.md` §7 carries the
tables.

### Method

- **Reference (§3).** Display-linear decides, which is §3 as written; the
  Karis-weighted number, round 1's deciding one, is given beside it. Round 2
  moved the resolve's averaging into the display's space, so both rounds
  judge against "the reference in the resolve's own space".
- **Reprojection error (§4.1).** As in round 1: 64-phase supersampled frames,
  Catmull-Rom warp, mixed-coverage pixels masked, coverage of each moving
  object required. Round 2 adds: a moving object too thin for the warp (a
  3-pixel strip has no pixel whose 3x3 is free of its own silhouette) counts
  as covered when the analytic test verified at least 99% of its pixels. The
  literal no-AA variant still fails on aliased content with exact vectors.
- **Trail band of `taa-skin`.** From measured, conservative coverage of the
  moving nodes (depth with and without them at the pixel centre and its four
  corners). Round 1 used `|v| > 0.05 px`, which also counted pixels a limb
  still covers once it stops; that number is printed beside it.
- **Trail oracle.** A resolve with perfect vectors and perfect disocclusion
  on the same jittered frames. It bounds what any history method can do on a
  pixel's first uncovered frame. It is not a bound where shading changes
  under a still pixel (the moving shadow in `taa-skin`).
- **Converged, other scenes.** Frame 32 of every scene but `taa-thin` is in
  motion; those lines are TAA in motion against MSAA. `converged-still` holds
  the same frame for 40 frames.

### §4.1 Motion vectors

| Test | Threshold | Measured | Verdict |
|---|---|---|---|
| Analytic (rigid, vertex hook, jittered rows) | ±0.02 px | max 0.0016 px | PASS |
| Reprojection, every scene, geometry target | mean < 2, p99 < 12 | 120/120 lines; `taa-vertex-hook` 0.247 / 5 and 0.266 / 6 (its 3-px strip covered by the analytic test: 100% of its pixels within 0.0016 px) | PASS |
| The same on the forced 32-byte fallback | same | 120/120 (round 1: 70/82): `taa-vertex-hook` analytic 2.14 px → 0.0003 px, `prefab-skinned-morph` 7.56 → 1.48, `taa-skin` 0.496 → 0.177 | PASS |
| Motion-history self-check, 48 scenes × 120 frames | 0 mismatching bytes | 0 on both paths | PASS |
| History burst / density / reset | exact | 3/3 | PASS |

### §4.2-§4.3 Ghosting, convergence, flicker (native)

Display-linear decides; Karis beside it; round 1 (Karis) in the last column.

| Test | Threshold | Measured | Karis | Round 1 | Verdict |
|---|---|---|---|---|---|
| Trail mean `taa-motion` | < 2, every frame | k31 3.79 and k211 2.39; every other frame below 2 | k32-34 and k211 also above 2 | 3.672 | NOT MET on two frames: both are bands made only of first-frame pixels, where the oracle reads 3.81 and 2.17 |
| Trail mean `taa-reveal` | same | worst 1.751 | | 1.931 | PASS |
| Trail mean `taa-skin` (added) | same | worst 1.388 | | 7.425 on the vector band | PASS |
| Trail max, original | < 16, every frame | 89 / 74 / 67 | | 152 / 88 / 135 | NOT MET (the oracle reads 18-39) |
| Trail max, restated | ≤ max(MSAA, no AA) on the same frame | exceeded on 16 / 4 / 42 frames; on 7 / 2 / 37 of them the oracle is at or above the bound too; TAA alone on 9 / 2 / 5 | | | NOT MET on 9 / 2 / 5 frames |
| Stop `taa-motion` / `taa-orbit` | mean < 1 after 8 frames | 0.191 / 0.344 | orbit 1.006 | 0.606 / 0.773 | PASS (Karis: orbit fails) |
| Cut, flagged / unflagged | < 3 vs TAA-off | 2.620 / 2.183 | | 2.249 / 1.883 | PASS |
| Converged `taa-thin` | < MSAA 4× | 0.559 (MSAA 5.525, SMAA 5.268, none 6.274) | 1.84 | 1.449 (MSAA 5.309) | PASS |
| Flicker, 16 scenes | mean < 0.5, p99 < 4 | worst `taa-thin` 0.301 p99 2 | | 0.292 p99 3 | PASS |
| Camera-motion blur (pan) `taa-orbit` | < 3 | worst 1.002 | | 1.134 | PASS |

What is left on the trail: on the two failing mean frames every band pixel is
on its first uncovered frame (`taa-motion` k31: 671 pixels; k211: 1570), and
seven single-frame estimators on the same jittered frame all read at or above
the oracle (k31: 3.81 to 14.3; k211: 2.17 to 9.8). The frames where TAA alone
exceeds the restated maximum all have their worst pixel on the contact line
under the moving cube: pixels shown from this frame only whose 3x3 still holds
cube samples, and three first-frame contact pixels that no test flags.
Pixels whose error exceeds max(no AA, oracle) + 16: 341 of 3.58 M pixel-frames
on `taa-motion`, 4 on `taa-reveal`, 73 on `taa-skin`.

Converged, other scenes at frame 32 (in motion): TAA is below MSAA on 9 of 10
(`taa-skin` 0.180 against 0.173; round 1: 0.341 against 0.168). Held still,
TAA is below MSAA on every scene.

### §4.4 Reactive pixels and the HUD

| Test | Threshold | Measured | Verdict |
|---|---|---|---|
| `taa-transparent`, band ∩ content, original | mean < 2, max < 16 | TAA worst 11.85 (average 4.66); MSAA 8.44 (4.71); no AA 16.78 (7.50) | NOT MET (by any mode) |
| The same, restated | TAA average ≤ MSAA's, TAA worst ≤ no AA's worst | 4.657 ≤ 4.714; 11.846 ≤ 16.778 | PASS |
| The content's own trail, original | same as the first row | TAA worst 3.84 (average 1.01); MSAA 5.64 (3.20); no AA 5.69 (3.28) | NOT MET (by any mode) |
| The same, restated | as above | 1.010 ≤ 3.198; 3.844 ≤ 5.691 | PASS |
| `taa-hud` HUD pixels vs TAA-off | mean < 1 | worst 0.018 | PASS |

At 0.67 (not a 05 line) HUD pixels of an opaque-path HUD read worst 0.984,
average 0.829: under 1 by little (round 1: 0.887 / 0.755).

### §4.5 Upscaling

| Test | Threshold | 0.67 | 0.58 | 0.5 | Verdict |
|---|---|---|---|---|---|
| Converged `taa-thin` | < 1.5× / 2× MSAA (5.525) | 1.243 | 1.769 | 2.668 | PASS, all below 1× MSAA (round 1: 2.991 / 4.233 / 5.759) |
| Pan `taa-orbit` | < 4.5 / < 6 | 2.52 | 3.19 | 4.08 | PASS |
| Flicker | mean < 0.5, p99 < 4 | ≤ 0.29 p99 ≤ 2 at every scale | | | PASS |
| Scale change, original | no frame mean > 3 | worst 4.190 (the 0.5 steady state) | | | NOT MET |
| Scale change, restated | ≤ +1.5 over the constant-scale run; ≤ +0.25 from 8 frames after a step | worst +0.863 on the step-up frame; +0.039 settled | | | PASS |
| Moiré / mip bias | flicker passes | `mip_bias_extra` 0 | | | PASS |

Isolated sub-render-pixel features dropped from a still `taa-thin`: 4.3% /
7.3% / 14.5% (round 1: 5.0% / 31.8% / 73.0%).

### Phase 5 (32-byte fallback, forced on the M1)

Vectors 120/120. Deforming meshes get exact vectors from the velocity overlay
and are no longer reactive; trail, stop and flicker equal the geometry path
to three decimals (`taa-skin` trail 1.388 on both). Not verified on a device:
the Android check; a masked deforming mesh; instance-transform writes (only
through harness-side motion).

### Regressions

TAA off against the real 0.45.0: 36/36 captures bit-identical; 12 timing
configurations within ±0.107 ms with mixed signs. Player tests 78/78. The
sweep of the three browser-test layers, Dreidel Derby, two templates, the
model tester and the editor is in the round-2 report; it found and fixed a
decal bug present since 0.45 (decals cut off after the render size shrinks)
and a marking bug new in this branch (clip-animated materials left
unmarked during a load).

### §5 Frame cost

1920x1435, DPR 2, busy-window method, 5 interleaved runs.

| | Target | Measured | Round 1 | Verdict |
|---|--:|---|---|---|
| Velocity target + second vertex evaluation | ≤ 0.2 ms | 0.12-0.23 ms (± 0.05; includes the seed and the previous-velocity copy) | 0.09-0.12 | at the target |
| Resolve at native | ≤ 0.6 ms | 1.21-1.26 ms, `taa-thin` 1.39-1.41 | 1.03-1.17 | NOT MET |
| Resolve at 0.67 | ≤ 0.6 ms | 1.16-1.18 ms, `taa-thin` 1.34 | about 1.10 | NOT MET |
| Dreidel Derby mid-round, TAA native | ≤ 11.5 ms | see `docs/TAA.md` §7.6 | 12.13 | NOT MET |
| Dreidel Derby mid-round, TAA 0.67 | ≤ 7.5 ms | see `docs/TAA.md` §7.6 | 8.43 paced / 7.30 unpaced | see there |
| TAA off | unchanged | within ±0.107 ms of 0.45.0; pixels bit-identical | ±0.114 | MET |

Where the resolve's time goes (ablations on `kitchen-sink`): disocclusion
about 0.31 ms, the 5-tap history fetch 0.19, the clip-box floor 0.07-0.09,
the display curve 0.01-0.03, the rest (tile loads, 3x3, clip, blend, two
stores) about 0.6. The rest alone is the target.

### What is not met

1. Trail mean < 2 on two frames of `taa-motion` (k31, k211), where the
   oracle is at or above 2 as well.
2. Trail max: the original bound everywhere, and the restated bound on 9 / 2
   / 5 frames (`taa-motion` / `taa-reveal` / `taa-skin`) where TAA alone
   exceeds it.
3. The original forms of the reactive-content trail and the scale-change
   line (their restated forms pass).
4. Frame cost: the resolve at about twice its target, and with it the Dreidel
   Derby native target.
5. In the Karis space only: `taa-orbit` stop 1.006.
6. Not measured: the fallback on a real 32-byte device.
