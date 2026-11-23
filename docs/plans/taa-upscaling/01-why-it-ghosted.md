# 01 — Why it ghosted

Ghosting is history that should have been thrown away and wasn't. A TAA image
is mostly last frame's image moved to where its content went. The trail behind
a moving object is old colour that was moved to the wrong place, or that
should have been rejected and survived. Every cause below is one of those two.

## The previous implementation

The forward renderer had TAA until `c42b6805` (July 2025):
`post_process/anti-alias.wgsl`, plus a Halton jitter that is still in
`camera/buffer.rs` behind `APPLY_JITTER = false`. Read against the list
further down, it had nearly every problem on it:

| What it did | Why that ghosts or blurs |
|---|---|
| Motion vector = this frame's clip position at a pixel minus **last frame's clip position at the same pixel** (two position textures, both indexed by `pixel_coords`). | That is the difference between two *different* surfaces whenever anything moves: it is not where this surface was. It is right only for a static camera over a static scene, where it is zero anyway. Every moving edge got a wrong vector, and the history fetched from the wrong place. **This alone explains most of the ghosting.** |
| The clip positions were written with the **jittered** projection. | The vector then contains the jitter delta (up to a pixel) every frame: the history shakes, and the clamp below turns the shake into blur. |
| Jitter amplitude 0.8 when still, 0.2 when the camera moved, and `jitter / width` in NDC (a pixel is `2 / width`). | Half the intended amplitude, and the sample pattern changes as soon as the camera moves, so there is no stable pattern to converge on. |
| History sampled **bilinearly**. | Each frame blurs the history a little. Over 30+ frames it is a smear, which then counts as "history" for the clamp. |
| Per-channel RGB **min/max clamp** of the 3×3 neighbourhood, in linear HDR. | A single bright pixel nearby widens the box until anything passes. In HDR one specular pixel can be 50× its neighbours, so the clamp stops rejecting near highlights, emissives and bloom sources. Per-channel RGB boxes also let through colours that occur nowhere in the neighbourhood. |
| Current weight **0.02** (≈ 50 frames of memory), adjusted by ad hoc luma and motion boosts. | The longer the memory, the longer any wrong history survives. 50 frames is close to a second of trail. |
| History rejected entirely when motion > 0.05 UV. | Fast motion snapped to raw jittered frames (flicker), slow motion kept everything. |
| No velocity dilation. | At an object's silhouette the edge pixels read the *background's* velocity, so the object's edge drags background-coloured history along. That is the classic one-pixel halo trail. |
| No disocclusion test. | Background revealed behind a moving object fetches the history that was the object, and the clamp only partly removes it. |
| Transparents, particles and UV-scrolling materials handled like everything else. | They have no correct motion vector. Their colour moves while the vector says "background", so they smear. |
| A fixed sharpening mix on top. | Sharpening amplifies whatever the history got wrong. |

The jitter code still in the tree has the same amplitude and NDC-scale bugs.
Phase 0 rewrites it before anything turns it on (see 03 §1).

## The causes of ghosting, in general

Each row is a rule the design follows, and the place that enforces it.

| # | Cause | Rule | Where |
|---|---|---|---|
| G1 | Wrong motion vectors | A vector is *this surface's* screen position now minus *the same surface point's* screen position last frame. It includes camera, object, skin, morph and vertex-hook motion. | 03 §2; phase 1 gate |
| G2 | Jitter in the vectors | Vectors use unjittered matrices on both sides. The jitter is removed where the current frame is sampled, not baked into the vector. | 03 §1, §2.1 |
| G3 | Edge pixels with background velocity | Dilate: each pixel uses the velocity of the nearest-depth pixel in its 3×3. | 03 §4.3 |
| G4 | Blurry history accumulates | History is fetched with a sharp 5-tap Catmull-Rom, never plain bilinear. | 03 §4.5 |
| G5 | The rejection box is too big | Variance clip (mean ± γσ) in YCoCg, on tonemapped colour, so one HDR outlier cannot open the box. Clip towards the mean, not a per-channel clamp. | 03 §4.6 |
| G6 | Revealed background keeps old foreground | Disocclusion test against last frame's depth at the reprojected position. Reset where it fails. | 03 §4.7 |
| G7 | Memory too long | 1/(n+1) running average, n capped at 16 (24 when upscaling). Raised by motion and reactivity. | 03 §4.8 |
| G8 | Things with no correct vector | A reactive mask (transparents, particles, UV flow, HUD) lowers or removes history there. | 03 §5 |
| G9 | History reused across a cut | Explicit reset on camera cuts, scene loads, resolution and AA changes, and anything flagged as a teleport. | 03 §8 |
| G10 | Objects appearing or teleporting | A transform with no valid previous value gets prev = current (zero motion, then G6/G5 handle it), never stale data from a recycled slot. | 03 §2.3 |
| G11 | Blending in linear HDR | Blend in tonemapped space (Karis weighting) so fireflies don't dominate and dark edges aren't swamped. | 03 §4.4 |
| G12 | Sharpening the errors | Sharpening is a separate, optional pass after TAA, mild and off for the ghosting tests. | 03 §9 |

Two things that look like ghosting but aren't:

- **Blur when nothing moves** means the jitter, the sample reconstruction or the
  history filter is wrong (G2, G4), not the rejection. Fix it by checking the
  "converged still image vs 16× supersampled reference" test, not by shortening
  the history.
- **Shimmer on thin or specular detail** means too *little* history (the clip
  is rejecting valid samples, or the memory is too short). The instinct to cure
  ghosting by shortening history makes this worse. That is why the tests in 05
  measure both.

## The temporal passes that already work here

The repo has two temporal passes that did go through this and got it right in
their domain. The TAA resolve should reuse their structure and test style:

- `render_passes/ssr/shader/ssr_wgsl/temporal.wgsl`: depth reprojection
  through `prev_view_proj`, an unconditional 3×3 neighbourhood clamp, and
  ping-pong history by frame parity. Commit `12333a3f` ("temporal moved
  post-resolve with neighborhood clamp — kills motion ghosting") is the same
  lesson at small scale. `wgsl_validation` pins the clamp so it cannot be
  dropped.
- Volumetrics temporal (`c982931c`, which also fixed two flicker bugs it
  exposed).

Both rely on camera motion only, which is right for reflections and fog but
not enough for TAA (G1).
