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
