# 03 — Design

## 0. Terms

| | |
|---|---|
| **Render size** `R` | What the scene is rasterised and shaded at: `render_size()`, i.e. surface × `render_scale`. |
| **Post size** `O` | What TAA writes and bloom / effects / display run at: the surface size. |
| `s = R / O` | ≤ 1 when upscaling, 1 for TAA at native resolution. |
| Pixel coordinates | `x` right, `y` down, pixel `p` covers `[p, p+1)`, centre `p + 0.5`. |
| UV | `(pixel + 0.5) / size`, `y` down. NDC is `y` up. |
| `j` | This frame's jitter, in render pixels, in `[-0.5, 0.5]²`. |
| `v` | Motion vector, in UV units: `uv_now − uv_prev` of one surface point, both unjittered. |
| `n` | History length: how many frames the history pixel represents, stored in its alpha. |

## 1. Jitter

### 1.1 Sequence

- Halton base 2 for x, base 3 for y (`camera::halton`), minus 0.5.
- Index `(frame_index % N) + 1`. Index 0 is `(−0.5, −0.5)` and would sit on
  a pixel corner every cycle.
- `N = ceil(8 · (O.w / R.w)²)`: 8 phases at native, 18 at s = 0.67, 32 at
  s = 0.5. Each output pixel then gets about 8 render samples per cycle. This
  is FSR2's rule.
- The amplitude is constant. Never scale it with camera motion: the history
  is only valid if every frame samples the same pattern.
- `frame_index` restarts at 0 on every history reset (§8), so a reset never
  starts mid-cycle.

### 1.2 Applying it

The NDC offset for a render-pixel jitter `j` is

```
ndc_offset = ( 2·j.x / R.w ,  −2·j.y / R.h )      // y flips: pixels down, NDC up
projection_jittered = translate(ndc_offset.x, ndc_offset.y, 0) · projection
```

Pre-multiplying a translation adds `offset · w` to clip x/y, which is a
constant NDC shift after the divide. It works for perspective and
orthographic projections alike.

Effect: a surface point whose unjittered screen position is `s` rasterises at
`s + j`. Equivalently, **render pixel `p` holds the scene sampled at
unjittered position `p + 0.5 − j`**. Every formula below uses that sentence.

### 1.3 What uses the jittered matrices and what doesn't

| Consumer | Matrices | Why |
|---|---|---|
| Geometry raster, shadow-receiver reconstruction, SSR trace, prep's world position, DoF / haze depth reads | **jittered** (`view_projection`, `inv_view_projection`, `inv_proj`) | They read or write the depth buffer, which is jittered. |
| Motion vectors (both ends), history reprojection | **unjittered** (`view_projection_unjittered`, `prev_view_projection`) | A motion vector must not contain jitter (G2). |
| Shadow maps, cube maps, light culling froxels | their own cameras / unjittered | Unrelated to screen sampling. |
| Frustum culling, LOD screen error | unjittered | Half a pixel changes nothing, and must not make LOD flicker. |

The camera uniform gains `view_projection_unjittered` (this frame) and
`jitter` (this frame and last, in render pixels, plus `R`). It already has
`prev_view_projection` (unjittered). Append them at the end, the same way
`prev_view_projection` was added, so no existing offset moves.

### 1.4 Other jitters in the frame

SSR's march jitter and volumetrics' froxel jitter also advance with the frame.
Under TAA they then get averaged twice, which is fine, but they must not move
in lock-step with the camera jitter, or the patterns correlate into visible
structure. Use a different sequence or a fixed offset of the index for each,
and note it at both sites.

## 2. Motion vectors

### 2.1 Definition

For the surface point shown at a render pixel:

```
clip_now  = view_projection_unjittered · world_now(point)
clip_prev = prev_view_projection        · world_prev(point)
uv(c)     = ( c.x/c.w · 0.5 + 0.5 ,  −c.y/c.w · 0.5 + 0.5 )
v         = uv(clip_now) − uv(clip_prev)
```

`world_prev(point)` is the same mesh vertex evaluated with **last frame's**
world matrix, instance transform, joint matrices, morph weights and frame time,
interpolated with **this frame's** barycentrics. That is what makes it the same
surface point.

### 2.2 Where it is written: a fifth geometry target (primary path)

When the device's `maxColorAttachmentBytesPerSample ≥ 36`:

- The geometry pass, in TAA mode only, gets a fifth colour target
  `velocity: rg16float` at render size. TAA turns MSAA off, so it is
  single-sampled: 4 bytes per pixel.
- The vertex shader evaluates `apply_vertex` twice, once with current inputs
  and once with previous inputs (§2.3), and outputs `clip_now` and `clip_prev`
  as `@interpolate(perspective, center)` varyings.
- The fragment shader does the divide **per fragment** (`clip.xy / clip.w`)
  and writes `v`. Interpolating already-divided positions is wrong under
  perspective.
- Masked and custom-vertex variants get the same outputs. The HUD pass writes
  `v = 0`.
- Sky pixels have no geometry. The resolve computes their vector from the view
  direction: reconstruct the direction from `inv_view_projection` and project
  it with the rotation part of `prev_view_projection` at `w = 0`.

Cost: one extra vertex evaluation for skinned / morphed meshes (the skin loop
again with the previous matrices), two extra varyings, 4 bytes a pixel.

### 2.3 Previous-frame state

New GPU buffers, each a copy of a current one as of last frame:

| Buffer | Copy of | Bytes |
|---|---|---|
| `prev_world_transforms` | the model matrix half of `transforms` | 64 per transform |
| `prev_instance_transforms` | the instancer's per-instance rows | 64 per instance |
| `prev_skin_joint_matrices` | `skin_joint_matrices` | as current |
| `prev_morph_weights` | `geometry_morph_weights` | as current |
| `frame_globals.prev_time` | the time a vertex hook saw last frame | 4 |

**Ordering matters.** `queue.writeBuffer` uploads run before the frame's
command buffer executes, so a copy recorded at the *start* of the frame's
encoder would already see this frame's data. Record the copies
(`copy_buffer_to_buffer`, current → prev) at the **end** of the frame's
encoder, after the last pass that reads either buffer. Next frame, "prev" then
holds this frame's values. This is a GPU-side copy of the used range, with no
CPU traffic.

**New and recycled slots (G10).** A transform inserted this frame, or a slot
reused after `remove`, has no meaningful previous value. Whenever the CPU
writes a slot that did not exist last frame, it also writes the same matrix
into the prev buffer, so its motion is zero. Then disocclusion and clipping
handle its appearance like any reveal. `Transforms::reset_history(key)`
(public) does the same for teleports. The same goes for instances, skins and
morph targets.

**Vertex hooks.** A custom vertex hook gets `prev_time` and is evaluated twice
like skinning. That is exact for hooks that depend only on time and the
mesh. A hook that reads other changing state can't be reprojected. Its
material sets `temporal: reactive` (§5), and its meshes get history
discounted.

**LOD.** A cluster-LOD or LOD-level switch changes the triangles but not the
surface, so the vectors stay right, and clipping absorbs the small
difference in shading. LOD hysteresis must use unjittered inputs (§1.3) or the
switches flicker.

### 2.4 Fallback: devices at 32 bytes per sample

No fifth target. The **prep pass** (which already reconstructs world position
from depth, per pixel) writes `velocity` to an `rg16float` storage texture:

```
P        = world position from depth (jittered inverse)
M, M_prev = world and previous world matrix of this pixel's mesh / instance
P_prev   = M_prev · inverse(M) · P          // exact for rigid motion
v        = uv(VP_unjittered · P) − uv(prev_VP · P_prev)
```

That is exact for the camera and for rigid objects. For skinned, morphed and
vertex-hook meshes it gives the rigid part only. Those pixels are marked
reactive at 0.5, so their history is discounted rather than trusted. Mobile
gets TAA with slightly less stable animated characters, which is the right
trade.

Prep needs `inverse(M)` per pixel. The normal matrix in `transforms` is not
the inverse. Either store `inverse(M)` alongside the previous model matrix (a
64-byte column per transform, written by the CPU when the transform is dirty),
or invert in the shader (one 3×4 inverse per pixel, cheap next to shading).
Measure both.

## 3. History

- `taa_history[2]`: `rgba16float` at post size, ping-ponged by frame parity
  (the SSR temporal pattern).
- `.rgb` holds colour in the **tonemapped working space** (§4.4); `.a` holds
  `n`, the history length.
- `prev_depth`: `r32float` at render size, last frame's raw depth, copied at
  the end of the frame (same ordering rule as §2.3).
- First frame and after every reset: no read; write the current frame with
  `n = 1`.

## 4. The resolve

One compute pass at post size, 8×8 workgroups, one invocation per output
pixel. Inputs: the render-size `composite` (lit HDR after SSR), `depth`,
`velocity`, `reactive` (§5), `prev_depth`, the previous `taa_history`, and the
camera uniform. Outputs: this frame's `taa_history` and `taa_out` (linear HDR
at post size, what bloom and effects read).

All loops below run over a fixed 3×3 and must be unrolled or accumulate
directly. No per-invocation arrays indexed by the loop counter (02, Apple
pitfalls).

### 4.1 Where this output pixel falls in the render image

```
q        = output pixel (integer), uv_q = (q + 0.5) / O
r_pos    = uv_q · R                         // unjittered render-pixel position of q's centre
p0       = floor(r_pos)                     // the render pixel it lands in
```

### 4.2 Current colour: gather the render samples

For each of the 3×3 render pixels `p = p0 + (dx, dy)`:

```
c        = composite[p]                              (HDR, linear)
t        = tonemap(c)                                (§4.4)
sample_at = p + 0.5 − j                              (§1.2: where this sample really is)
d        = sample_at − r_pos                         (render pixels)
w        = exp(−2.29 · dot(d, d))                    Blackman-Harris approximation, 1 render px wide
current += w · t ;  W += w ;  w_best = max(w_best, w)
m1 += ycocg(t) ;  m2 += ycocg(t)²                    (unweighted moments, for §4.6)
```

- Distances are in **render** pixels, where samples are always one pixel
  apart whatever `s` is, so the kernel does not change with scale. At native
  resolution the sample nearest the centre dominates and the neighbours add
  the sub-pixel filter. When upscaling, the nearest sample can be up to about
  0.7 render pixels away (weight ≈ 0.3), and this frame's estimate gets
  correspondingly unsure. That is what `confidence` carries.
- FSR2 uses a Lanczos-2 here. Start with the Gaussian (no negative lobes, no
  ringing) and keep Lanczos as a tuning option.
- `current = current / W`.
- `confidence = w_best`, the weight of the sample nearest this output pixel's
  centre: 1 when a sample landed on it, lower the further this frame's
  nearest sample is. Upscaling depends on it (§4.8).

### 4.3 Motion vector: dilated (G3)

Over the same 3×3 render pixels, find the one **nearest the camera**
(reverse-Z: largest raw depth; forward-Z: smallest) and use **its** `v`. A
silhouette pixel whose centre shows background then moves with the object in
front of it, instead of dragging background history across the edge.

Sky: if the nearest pixel is sky, `v` is the rotation-only vector (§2.2).

### 4.4 Working space (G11)

```
tonemap(c)  = c / (1 + max3(c))
untonemap(t) = t / (1 − max3(t))        // max3(t) < 1 always
ycocg(t)    = ( 0.25·r + 0.5·g + 0.25·b ,  0.5·r − 0.5·b ,  −0.25·r + 0.5·g − 0.25·b )
```

Blending, clipping and the Catmull-Rom fetch all happen on `t`. One
saturated highlight then weighs about as much as a white pixel, not 50 of
them. That is the Karis trick, and the reason the old HDR clamp failed. The
MSAA resolve here already does the same thing (`final_blend`: `s/(1+s)`), so
use the same helper.

### 4.5 History fetch (G4)

```
uv_hist = uv_q − v
```

- If `uv_hist` is outside `[0, 1]²`: reset this pixel (`α = 1`, `n = 0`). It
  was off-screen last frame.
- Otherwise fetch `taa_history_prev` with the **5-tap Catmull-Rom**: 9-tap
  bicubic collapsed to 5 bilinear fetches by dropping the corners. Clamp the
  result to `≥ 0`, because the negative lobes can undershoot. Fetch `n_prev`
  from the nearest texel. Never interpolate `n`.

Bilinear history is the single biggest cause of TAA "blur" complaints, and
blur that keeps accumulating looks like ghosting.

### 4.6 Rejection by variance clipping (G5)

```
mean  = m1 / 9 ;  sigma = sqrt(max(m2 / 9 − mean², 0))
box   = [mean − γ·sigma , mean + γ·sigma]            // YCoCg, tonemapped
h     = clip_towards(ycocg(history), mean, box)       // ray from mean to history, cut at the box
clip_amount = |ycocg(history) − h| / max(|ycocg(history) − mean|, ε)    ∈ [0, 1]
```

- `γ = 1.0` to start. Lower (0.75) is tighter: less ghosting, more flicker.
  Higher (1.25) is looser. Tune γ as a function of `|v|`: tighter when moving,
  looser when still, where flicker is the visible failure.
- **Clip, don't clamp.** A per-channel clamp moves the colour to a corner of
  the box, a colour that may occur nowhere in the neighbourhood. Clipping
  along the line to the mean keeps hue.
- The box comes from the **current** frame only, plain 3×3, before any
  reconstruction filtering.
- Clipping is unconditional: no "skip clip when still" shortcuts. The SSR
  temporal pass pins this with a test, and so should this one.

### 4.7 Disocclusion (G6)

```
d_now   = linearize(depth at the dilated pixel)
d_prev  = linearize(prev_depth at uv_hist, nearest of its 2×2 footprint)
disoccluded = d_prev < d_now · (1 − τ)         // something nearer was there last frame
```

- `τ = 0.05` relative to start (depth moves a little between frames, so the
  test needs slack). Note the comparison: it only fires when last frame's
  depth at that spot was **nearer**, i.e. this surface was hidden behind
  something.
- On disocclusion: `α = 1` (or `max(α, 0.8)` when upscaling with low
  confidence), `n = 0`.
- **Errors in the safe direction.** A false disocclusion costs one frame
  of aliasing at that pixel. A missed one costs a ghost. When in doubt, reject.

### 4.8 Blend weight (G7)

```
n      = min(n_prev, N_max)              N_max = 16 at native, 24 when upscaling
alpha  = 1 / (n + 1)                     running average while the history is young
alpha  = max(alpha, alpha_motion(|v| in output px))     e.g. lerp(0, 0.2, saturate(speed / 8))
alpha  = max(alpha, reactive)                           (§5)
alpha  = max(alpha, clip_amount · 0.5)                  a heavily clipped history is suspect
if upscaling: alpha *= mix(0.3, 1.0, confidence)        no sample nearby → lean on history
if reset (off-screen / disoccluded / §8): alpha = 1
```

Then

```
result_t = mix(h, current, alpha)
n_next   = reset ? 1 : min(n + 1, N_max) · (1 − clip_amount)
```

- `n_next` shrinks when the history had to be clipped. A pixel whose history
  keeps getting clipped (a revealed area, a moving shadow) stops trusting
  history after a frame or two, instead of carrying a long accumulation that
  keeps getting cut.
- With `N_max = 16` the effective memory is about 16 frames, a quarter of a
  second at 60 fps. Never use the old 0.02 (50 frames).
- After a reset with low confidence (upscaling, `alpha = 1`), `current` is a
  sparse upsample of one frame. It looks soft for the few frames until history
  fills in. That is expected and is what the "reveal" test measures.

### 4.9 Outputs

```
taa_history_next = ( result_t , n_next )
taa_out          = untonemap(result_t)        // linear HDR, post size
```

NaN / Inf guard: if any input is non-finite, write `current` and `n = 0`. One
NaN in a history otherwise spreads through the clip box forever.

## 5. Reactive pixels (G8)

Some pixels show colour that does not move with their motion vector.
`reactive: r8unorm` at render size says how much to distrust history there
(0 = trust, 1 = ignore). The resolve takes the max over the same 3×3 as the
dilation.

### 5.1 Transparent surfaces and particles

The forward transparent pass writes `reactive` as a second target (1 byte),
blended with **max**: `reactive = max(reactive, alpha · k_transparent)`,
`k_transparent = 0.8` to start. Glass with low alpha barely changes anything;
an opaque-looking particle is close to fully reactive. Lines use the same
rule.

Fully reactive pixels alias like no-AA. That is the deliberate trade: no
trails. A later phase can give the transparent pass velocity of its own for
large, mostly opaque transparents (the history then follows them). It is not
needed to fix ghosting.

### 5.2 Materials whose texture moves

UV flow (`texture flow`), flipbooks, animated emissive and custom shaders
reading time change colour on a still surface. Material data gets a
`temporal` flag (`stable` default, `reactive`). Opaque shading writes
`reactive = 0.6` for flagged materials, through the prep / classify output it
already has per pixel.

### 5.3 Reflections

SSR is composited before TAA, and reflections move with a different parallax
than the surface (that is exactly why SSR has its own temporal pass). TAA's
clipping removes most of the mismatch. If mirror-like surfaces still trail
under camera motion, add `reactive += reflectivity · k_ssr` from the SSR
reflection descriptor, with `k_ssr = 0` by default and tuned in phase 3 against
the mirror test scene.

### 5.4 The HUD

The HUD must not be accumulated. Phase 3 writes `reactive = 1` for HUD pixels
(from the HUD geometry pass), which is correct but leaves the HUD with plain
jittered edges. The proper fix, also phase 3, is to draw HUD meshes after TAA,
into `taa_out` at post size with the **unjittered** camera. Their pass is
already separate, so this means moving it, not rewriting it.

## 6. Where it sits, and what changes around it

```
render size:  geometry (+velocity) → shading → transparents (+reactive) → composite → SSR
TAA:          composite (R) → taa_out (O)                       ← new, compute
post size:    HUD (moved here, unjittered) → SMAA? no → bloom → volumetrics sample → effects → display
```

- **MSAA and SMAA are off** in TAA mode. `AntiAliasing` gains
  `taa: Option<TaaConfig>`, and validation rejects TAA combined with MSAA or
  SMAA, rather than picking one silently. Switching to TAA recompiles the
  geometry pass (velocity target) and the transparent pass (reactive target),
  like any AA change.
- **Bloom, effects (haze, DoF), display** run at post size and read
  `taa_out`.
- **Depth for post effects.** Haze and DoF read depth at render size. Add
  one helper, `load_depth_post(coords_post)`, that maps to render pixels and
  takes the **nearest** depth of the 2×2 footprint. "Nearest" is the same
  rule the DoF silhouette fix uses (nearest sample), for the same reason. DoF's
  `dof_prepare` then writes its CoC buffer at post size.
- **Display:** at `s < 1` with TAA, display is 1:1 (post size = surface).
  The supersampling downsample variant stays for `s > 1` and is incompatible
  with TAA in v1 (validate).
- **Picker:** reads the visibility buffer at render size, already mapped
  through `render_size()`. No change.
- **Volumetrics:** the froxel volume is resolution independent. Its integrate
  sample happens in effects, at post size. No change.
- **Zero cost when off.** The velocity and reactive targets, prev buffers,
  their copies, and the resolve exist only in TAA mode (shader variants +
  lazy resources), matching the renderer's zero-cost-off rule.

## 7. Texture LOD bias

With jitter, and especially when upscaling, textures must be sampled at the
**output** pixel's footprint, or they blur by `1/s`:

```
mip_bias = log2(s) + extra          // s = R/O; extra = 0 to start, tune towards −0.5
```

- Opaque: multiply the gradients from the barycentric-derivative target by
  `exp2(mip_bias)` in the one helper that builds them (`helpers/mipmap.wgsl`).
- Transparent: `textureSampleBias(…, mip_bias)`.
- FSR2 recommends `extra = −1`. That is sharper and shimmers more on
  high-frequency textures. Decide with the moiré test scene (05), not by eye.

## 8. Resets (G9)

History is discarded (`frame_index = 0`, every pixel `α = 1`, `n = 0`) on:

- the first frame and any frame after the history textures were recreated
  (post size change);
- an AA-mode change, a scene load or teardown;
- `AwsmRenderer::reset_temporal_history()`, for cuts and teleports, called by
  apps and by the scene loader on a camera switch;
- a camera jump bigger than a threshold (a cut nobody flagged): translation
  more than about 20% of the view distance, or rotation more than about 30°,
  in one frame. A guard, not the main mechanism.

A **render-scale change is not a reset.** History lives at post size, so a
resolution governor can move `s` every few seconds without visible pops. Only
`N` (the jitter cycle) is recomputed.

## 9. Sharpening (optional, phase 4)

A separate pass on `taa_out`: RCAS (FSR1's robust contrast-adaptive
sharpening), strength 0.2–0.3, **off by default and always off for the
ghosting tests** (G12). It compensates for the softness of upscaling. It must
never be used to hide a resolve that blurs at native resolution, which is a
bug in §4.2 or §4.5.

## 10. Parameters (initial values; every one is a uniform for tuning)

| Parameter | Start | Range to test |
|---|--:|---|
| Jitter phases N | `ceil(8 / s²)` | — |
| γ (clip box) still / moving | 1.25 / 0.75 | 0.6–1.5 |
| τ (disocclusion) | 0.05 | 0.02–0.1 |
| N_max native / upscaling | 16 / 24 | 8–32 |
| Motion alpha at 8 px / frame | 0.2 | 0.1–0.4 |
| Reactive: transparents | 0.8 · alpha | — |
| Reactive: flagged materials | 0.6 | — |
| Reactive: reflections `k_ssr` | 0 | 0–0.5 |
| Mip bias extra | 0 | −1–0 |
| RCAS strength | off | 0.2–0.35 |
