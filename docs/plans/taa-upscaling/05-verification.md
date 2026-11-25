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
  **Restated (by the user, after round 2's evidence).** "Mean < 2 and max
  < 16 at every frame" cannot tell a ghost from what no history method can
  know. A perfect-information oracle (exact vectors, exact disocclusion
  knowledge, the resolve's own kernel) reads a band maximum of 18-39 and,
  where a band holds only first-frame pixels, a mean of 2.2-3.8
  (`taa-motion` k31, k211): one sample cannot be within those bounds of a
  64-sample reference on a high-contrast edge. The gate is restated as what
  it was for: **no ghost**. Every pixel of every frame is classified
  (`?mode=ghost-track`):
  - **(a) ghost**: history was used (alpha < 0.99) and the error exceeds
    both no AA's and the oracle's (MSAA's on moving pixels) by more than 16
    display units: stale history where it should not be;
  - **(b) current frame**: the pixel shows this frame only (alpha ≥ 0.99)
    and has the same excess: kernel bleed or aliasing, gone when the edge
    moves on;
  - **(c)** within 16 of the oracle, **(d)** within 16 of no AA.

  (a) pixels are followed along their motion into events. The gate: **in
  the trail band a moving object leaves and in the still parts of the
  frame, no (a) event lasts more than one frame** (`ghost-persist`), at the
  default configuration; and over each of those two regions TAA's mean
  error is no worse than MSAA 4×'s or the oracle's, whichever is larger
  (`ghost-region`; all four means, with no AA's, are printed, so a region
  where TAA is above MSAA is visible even when the line passes). The moving
  objects' own surfaces are reported, not gated (softness of moving detail,
  judged against MSAA by its region error). `taa-orbit` moves only the
  camera: its band is the pixels whose motion fell below 0.05 px, so its
  trail and still regions are reported only, judged by the moving-region
  error against MSAA. `taa-thin`'s still region is reported only: sub-pixel
  wires in a still image repeat with the 8-frame jitter cycle, which the
  flicker test judges. The original numbers stay in every
  report (`trail`, `trail-mean`, `trail-max-restated`, `trail-max-compare`).
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

**Restated (by the user, after round 2's evidence).** The 0.6 ms resolve
rows and the Dreidel Derby 11.5 ms and 7.5 ms rows are reported, not gates:
the measured resolve is about twice the estimate for structural reasons
(`docs/TAA.md` §7.6). The cost gate is: TAA at the shipped default is
cheaper than MSAA 4× in Dreidel Derby, mid-round and on the title screen.
Resolve time may be spent where it buys visible quality.

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

Three rounds. Earlier records are in git history (`2da7b171`, `8582017d`).
This is the state at renderer `ec35a6fd` plus the new-object flag
(`reactive_new_objects`, `dbf594df`) and bloom's source
(`TaaBloomSource::CurrentFrame`) and depth of field before the resolve
(`TaaDofOrder::BeforeResolve`, round 4; the nine scenes have DoF off, and
with DoF off its captures are bit-identical), which leave every other
line of this section unchanged (the nine ghost-track scenes, flicker, stop /
cut / converged / pan; TAA off 36/36). Later commits change the harness and
docs only. Harness `examples/taa-tests`, M1 Max, Chrome, DPR 2, 960x720
(cost at 1920x1435), fixed timestep and seeds, every frame. Display-linear
decides (§3); the Karis-weighted number is beside it where the verdict
differs. Every original line still emits with its original threshold.
`docs/TAA.md` §7 carries the tables.

### Method notes

- **Reprojection error (§4.1).** 64-phase supersampled frames, Catmull-Rom
  warp, mixed-coverage pixels masked, coverage of each moving object
  required; an object too thin for the warp counts as covered when the
  analytic test verified at least 99% of its pixels.
- **Trail band of `taa-skin`.** From measured, conservative coverage of the
  moving nodes, not from `|v| > 0.05 px`.
- **Oracle.** A resolve with perfect vectors and perfect disocclusion on the
  same jittered frames.
- **Ghost classification (§4.2 restated).** "This frame only" is alpha ≥
  0.985: the reactive mask is 8-bit and capped at 0.99 (1.0 means HUD), so
  a fully reactive pixel stores 252/255 = 0.988. The record gives the
  default-level tables at 0.99 too.

### §4.1 Motion vectors

| Test | Threshold | Measured | Verdict |
|---|---|---|---|
| Analytic | ±0.02 px | max 0.0016 px | PASS |
| Reprojection, every scene | mean < 2, p99 < 12 | 120/120 lines on the geometry target and 120/120 on the forced 32-byte fallback (round 1: 70/82 there) | PASS |
| Motion-history self-check, 48 scenes × 120 frames | 0 mismatching bytes | 0 | PASS |
| History burst / density / reset | exact | 3/3 | PASS |

### §4.2 Ghosting

| Test | Threshold | Measured | Verdict |
|---|---|---|---|
| Ghost events (restated), trail band and still areas | none longer than one frame | none on `taa-reveal`, `taa-skin`, `prefab-skinned-morph`, `anim-skinned`, `particles`, `taa-transparent`, and in the still areas of `taa-motion`. Left in the trail band: `taa-motion` 1 event (k211: 1 pixel, 20 levels, 4 frames) and `taa-thin` 3 events of 2 frames | NOT MET on 4 events (below) |
| Region error (restated) | TAA ≤ max(MSAA, oracle), trail band and still areas | 15/15 regions | PASS |
| Trail mean, original | < 2, every frame | `taa-motion` k31 3.79 and k211 2.39 (oracle 3.81, 2.17); `taa-reveal` worst 1.750; `taa-skin` 1.384 | NOT MET on the two oracle-floor frames |
| Trail max, original | < 16, every frame | 89 / 74 / 50 | NOT MET (oracle 18-39) |
| Stop `taa-motion` / `taa-orbit` | < 1 after 8 frames | 0.187 / 0.296 (Karis 0.674 / 0.970) | PASS |
| Cut, flagged / unflagged | < 3 vs TAA-off | pass 2/2 | PASS |

The four remaining events, none of which is the colour of an object that
has moved away:

- `taa-motion` k211. The cube rests for 60 frames with its edge partly
  over one floor pixel, and TAA converges to the partial coverage, as the
  reference does. The cube then moves off; nothing in the pixel's
  neighbourhood moves, and the history takes 4 frames to let go.
  `edge_release: 1.0` removes it (events 1 → 0) but raises still-image
  flicker on thin wires and small instances (`taa-thin` 0.301 → 0.323,
  `instancing-stress` 0.105 → 0.151), so it is off by default.
- `taa-thin`, 3 events on the 1-2 pixel light rim of the moving red block:
  the pixels blend history fetched through a neighbour that moves with
  them, about half a pixel off. Fetching through the pixel's own vector
  removes two of them and doubles the scene's still-image flicker (0.300 →
  0.587, p99 12).

New objects (round 3, `reactive_new_objects`, default 1.0): a mesh shown
for the first time, or again after being hidden, held the history of what
was behind it, which no test rejects (nothing nearer was there) and the
clip keeps on its silhouette. On `taa-motion@popin` (the cube appears,
still, over the checker floor), weight 0 → 1: still-area ghost events longer
than one frame 135 → 0, pixels more than 16 off the reference three frames
after the pop 4,817 → 63, still-area error 0.303 → 0.183 (MSAA 2.021). On
`taa-motion@popjump` (shown a frame before it is posed in place, as Dreidel
Derby's respawned coins are) the flag fires again on the jump frame. In
Dreidel Derby the coin's rim matches the reference from its first frame.

Bloom from very bright thin sources (round 3, `TaaBloomSource`): with bloom
reading the resolved image, the glow around a 1-px line at HDR 50 was 25%
below the reference and around a 2-px glint 12% (`bloom-post@thinstill`),
and a respawned Dreidel Derby coin's spawn glow was absent. Bloom now reads
this frame's HDR render (default): 0.4194 against the reference's 0.4196
and 0.3318 against 0.3300. Reported, not gated: in a still image the halo
ring of a source smaller than a pixel changes frame to frame by mean
1.6-3.4, p99 8-14 at the jitter period, above the flicker gate's p99 of 4
in that region (whole-frame flicker 0.086, p99 2: PASS). `docs/TAA.md` §6.

Depth of field (round 4, `TaaDofOrder`): the harness's `<scene>@dof*`
variants turn DoF on with a focus and aperture per scene (the CoC per node is
logged). With DoF after the resolve (the previous behaviour) out-of-focus
bright sources were dim (`bloom-post@thin@dofstill`, disc energy 95.5 /
92.7 / 74.8% of the reference for a 1-px line, a 2-px glint and a sub-pixel
glint), slight defocus failed the flicker gate (`taa-thin@dofslight`, CoC
1-2 px: 0.391 p99 6), and a defocused foreground over a sharp background
ghosted (`taa-reveal@dofnear` / `taa-orbit@dofnear`: 223 / 533 trail-band
events longer than one frame, up to 59 frames). With DoF before the resolve
(now the default): 98.9 / 100.8 / 97.3%, 0.198 p99 1, 0 / 1 events (2
frames), every other DoF scene at its no-DoF level. With DoF on and the
order `AfterResolve`, and with DoF off in either order, captures are
bit-identical to `a2bfa3e9` (18/18 and 10/10). Dreidel Derby: `docs/TAA.md`
§7.7.

By disocclusion level, trail-band events longer than one frame on
`taa-motion` / `prefab-skinned-morph` / `anim-skinned` / `taa-skin` /
`taa-thin`: `Depth` 266 / 2 / 1 / 17 / 105, `DepthAndMotion` 26 / 1 / 1 / 0 /
8, `DepthMotionAndId` (default) 1 / 0 / 0 / 0 / 3. Round 2's head, by the
same classifier: `taa-motion` 36 events up to 5 frames, `taa-skin` still
areas 9 events up to 18 frames and its moving legs 584 up to 45 frames,
`taa-transparent` 172 up to 7 frames.

Where TAA's region mean is above MSAA's although the line passes (the oracle
is higher): `taa-skin` trail 0.931 against 0.813, `particles` trail 0.892
against 0.795. On the moving objects themselves (reported, not gated) TAA is
below MSAA except `taa-reveal` 0.348 / 0.336, `anim-skinned` 0.246 / 0.161
and `particles` 0.558 / 0.327; on `taa-motion` and `taa-orbit` it is 2.20
against 7.63 and 1.06 against 4.12.

### §4.3 Convergence and flicker

| Test | Threshold | Measured | Verdict |
|---|---|---|---|
| Converged `taa-thin` | < MSAA 4× | 0.560 (MSAA 5.525, SMAA 5.268, none 6.274) | PASS |
| Converged Dreidel Derby | < MSAA 4× | `docs/TAA.md` §7.7 | see there |
| Flicker, 17 scenes | mean < 0.5, p99 < 4 | worst `taa-thin` 0.300 p99 2, `taa-transparent` 0.291 p99 3 | PASS |
| Camera-motion blur (pan) | < 3 | 0.858 | PASS |

### §4.4 Reactive pixels and the HUD

| Test | Threshold | Measured | Verdict |
|---|---|---|---|
| `taa-transparent` band ∩ content, original | mean < 2, max < 16 | worst 7.67 (round 1: 12.68) | NOT MET (by any mode) |
| The same, restated | ≤ MSAA's average, ≤ no AA's worst | average 2.96, worst 7.67 | PASS |
| The content's own trail, original | as the first row | worst 1.88 mean; max over 16 | NOT MET (max) |
| The same, restated | as above | average 0.81, worst 1.88 | PASS |
| `taa-hud` | mean < 1 | 0.018 | PASS |

### §4.5 Upscaling

| Test | Threshold | 0.67 | 0.58 | 0.5 | Verdict |
|---|---|---|---|---|---|
| Converged `taa-thin` | < 1.5× / 2× MSAA (5.525) | 1.254 | 1.783 | 2.679 | PASS, all below 1× MSAA |
| Pan | < 4.5 / < 6 | 2.329 | 2.934 | 3.764 | PASS |
| Flicker | mean < 0.5, p99 < 4 | ≤ 0.297 p99 2 | | | PASS |
| Scale change, original | no frame mean > 3 | worst 3.900 | | | NOT MET |
| Scale change, restated | ≤ +1.5 over the constant-scale run; ≤ +0.25 settled | +0.626; +0.008 | | | PASS |

Isolated sub-render-pixel features dropped from a still `taa-thin`: 4.4% /
7.3% / 14.6% (round 1: 5.0% / 31.8% / 73.0%). Opaque-path HUD pixels at
0.67: worst 0.984 against the bound of 1.

### Phase 5 (32-byte fallback, forced on the M1)

Vectors 120/120 at each disocclusion level; trail, stop, flicker and
converged equal the geometry path to three decimals; live level changes
12/12 on both paths. Not verified on a device: the Android check; a masked
deforming mesh; instance-transform writes (harness-side motion only).

### Regressions

TAA off against 0.45.0: 36/36 captures bit-identical; frame time within
±0.107 ms (round 2, direct) and within ±0.053 ms of that build at the final
head. The sweep of the three browser-test layers (41 Layer A scenes in
their default mode, TAA native and TAA 0.67), the model tester, the editor,
Dreidel Derby and two templates found no regression; it found and fixed a
decal bug present since 0.45, a marking bug new in this branch, and an
editor lock leak this branch had made easy to hit.

### §5 Frame cost

1920x1435, DPR 2, busy-window method. Reported, not gates, except the last
two rows.

| | Estimate | Measured |
|---|--:|---|
| Velocity target + second vertex evaluation (+ seed, + previous-frame copies) | ≤ 0.2 ms | 0.12-0.23 ms at round 2; the id level adds its 8 B/px copy |
| Resolve at native | ≤ 0.6 ms | 1.53-1.55 ms (`taa-thin` 1.83) at the default level; `Depth` 1.23 / 1.30, `DepthAndMotion` 1.47 / 1.64 |
| Resolve at 0.67 | ≤ 0.6 ms | 1.46-1.47 ms (1.73) |
| Synthetic scenes, TAA native against MSAA 4× | | −0.72 (`instancing-stress`), −0.45 (`taa-thin`), +0.71 (`kitchen-sink`), +1.03 (`ssr`) ms |
| Synthetic scenes, TAA 0.67 against MSAA 4× | | −0.10 to −2.24 ms |
| Dreidel Derby, TAA at the default against MSAA 4× (the gate) | cheaper | `docs/TAA.md` §7.6 |
| TAA off | unchanged | MET |

### What is not met

1. The restated ghost gate, on four events: `taa-motion` k211 and the three
   `taa-thin` rim events, described above.
2. The original trail bounds (mean on the two oracle-floor frames, max
   everywhere), the original reactive-content and scale-change lines; their
   restated forms pass.
3. Not measured: the fallback on a real 32-byte device.
