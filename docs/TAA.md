# TAA and temporal upscaling

Temporal anti-aliasing (TAA) jitters the camera by a fraction of a pixel each
frame and blends this frame with the reprojected result of earlier frames.
Below a render scale of 1 the same pass upscales: the scene is rendered
smaller and the resolve rebuilds the output resolution from several frames of
samples.

This document is the reference: when to use which anti-aliasing mode, the
API, how the resolve works, what an app has to do, the known limits, and what
was measured. The design and the phase plan it came from are in
`docs/plans/taa-upscaling/` until that folder is removed; everything needed
after that is here.

## Contents

1. [Choosing a mode](#1-choosing-a-mode)
2. [API](#2-api)
3. [How it works](#3-how-it-works)
4. [The ghosting rules](#4-the-ghosting-rules)
5. [What an app must do](#5-what-an-app-must-do)
6. [Known limits](#6-known-limits)
7. [Measured](#7-measured)

## 1. Choosing a mode

`AntiAliasing` has three independent modes, plus the combination that existed
before TAA. The renderer default is MSAA 4x.

| Mode | Constructor | What it fixes | What it does not | Cost |
|---|---|---|---|---|
| None | `AntiAliasing::none()` | nothing | | none |
| MSAA 4x | `AntiAliasing::msaa4()` (the default) | geometric edges | shading and specular aliasing, sub-pixel detail between samples | +0.71 to +2.50 ms over no AA on the synthetic scenes, +4.1 ms in Dreidel Derby mid-round (1920x1435, §7.6) |
| SMAA | `AntiAliasing::smaa()` | geometric edges, as a post-process | sub-pixel detail, temporal shimmer | one post pass: +0.20 to +0.42 ms |
| MSAA + SMAA | `AntiAliasing { smaa: true, ..AntiAliasing::msaa4() }` | as MSAA; SMAA on top adds little | | both |
| TAA native | `AntiAliasing::taa(TaaConfig::native())` | geometric edges, shading and specular aliasing, sub-pixel detail, thin geometry | trails on things without motion vectors (§6); one frame of aliasing at every revealed pixel | +1.17 to +1.25 ms over no AA on the synthetic scenes (resolve 1.03-1.17 ms), +1.5 ms in Dreidel Derby mid-round |
| TAA Quality | `TaaConfig::preset(TaaPreset::Quality)`, scale 0.67 | as native, at 45% of the pixels shaded | 5.0% of isolated sub-render-pixel features lost in a still image; softer thin detail; opaque-path HUD soft | −0.38 to +0.32 ms against no AA (sharpening off; RCAS adds 0.16-0.19 ms); Dreidel Derby mid-round 7.30 ms against 10.74 with no AA (unpaced, §7.6) |
| TAA Balanced | `TaaPreset::Balanced`, scale 0.58 | as Quality, at 34% of the pixels | 31.8% of isolated sub-render-pixel features lost | not measured |
| TAA Performance | `TaaPreset::Performance`, scale 0.5 | as Quality, at 25% of the pixels | converged error above native MSAA's; 73% of isolated sub-render-pixel features lost | −1.01 to +0.03 ms against no AA; Dreidel Derby mid-round 6.16 ms (unpaced) |

Measured image error against the reference (§7.1; `taa-thin`, converged,
mean absolute error in 0-255 display units, lower is better):

| | native | 0.67 | 0.58 | 0.5 |
|---|--:|--:|--:|--:|
| TAA | 1.449 | 2.991 | 4.233 | 5.759 |
| MSAA 4x | 5.309 | | | |
| SMAA | 5.269 | | | |
| None | 6.078 | | | |

The same measurement in the real game (Dreidel Derby at 1920x1435, a frozen
frame after 32 frames; §7.7):

| | Title screen | Mid-round |
|---|--:|--:|
| TAA native | 1.217 | 0.873 |
| TAA 0.67 | 1.761 | 1.538 |
| MSAA 4x | 1.310 | 1.864 |
| SMAA | 1.114 | 1.675 |
| None | 1.167 | 1.885 |

So, in short:

- **TAA native** is the best still image by a wide margin (3.7x lower error
  than MSAA on `taa-thin`) and fixes the aliasing MSAA cannot reach. It trails
  on content without a correct motion vector (particles, transparents, a
  moving silhouette's first uncovered pixels); §6.
- **Quality (0.67)** is still better than native MSAA on the gate scene (2.991
  against 5.309) while shading 45% of the pixels. It is the preset to use
  when frame time matters.
- **Performance (0.5)** is close to no AA on fine detail (5.759 against MSAA's
  5.309 and no AA's 6.078) and loses most isolated sub-pixel features. Use it
  only when frame time decides everything.
- **Balanced (0.58)** sits between the two: below native MSAA on `taa-thin`
  (4.233 against 5.309), above it on `taa-orbit` and `taa-reveal`; 31.8% of
  isolated sub-pixel features lost. Its frame cost was not measured.

### Should TAA be the default?

The facts, as measured (§7):

- **Quality against MSAA 4x.** TAA native has the lower error on `taa-thin`
  (1.449 against 5.309), in Dreidel Derby mid-round (0.873 against 1.864) and
  on most other test scenes. It is worse on two converged INFO scenes,
  `taa-skin` (0.341 against 0.168) and `instancing-stress` (1.179 against
  1.033), and only narrowly better on the Dreidel Derby title screen (1.217
  against 1.310, where SMAA and no AA are lower still).
- **Cost against MSAA 4x.** TAA native is cheaper on two of the four synthetic
  scenes (`instancing-stress` −1.26 ms, `taa-thin` −1.07 ms) and in Dreidel
  Derby mid-round (−2.6 ms), and dearer on the other two (`kitchen-sink`
  +0.27 ms, `ssr` +0.52 ms). TAA 0.67 is cheaper than MSAA everywhere it was
  measured (−0.54 to −2.70 ms on the synthetic scenes, −7.4 ms in Dreidel
  Derby mid-round, unpaced).
- **Gates.** Not every verification gate is met (trails, transparent trails,
  the scale-change test, the resolve and native-frame cost targets;
  `docs/plans/taa-upscaling/05-verification.md`, Results).

The default was left at MSAA 4x for that last reason.

TAA needs no particular device feature: devices that cannot take the fifth
geometry target get motion vectors from the prep pass instead (§3.10). If a
TAA pipeline fails to compile on a device, the renderer switches to MSAA 4x
once and says why (`taa_fallback_reason`, §2.9).

## 2. API

### 2.1 Turning it on

```rust
use awsm_renderer::anti_alias::AntiAliasing;
use awsm_renderer::taa::{TaaConfig, TaaPreset};

renderer
    .set_anti_aliasing(AntiAliasing::taa(TaaConfig::preset(TaaPreset::Quality)))
    .await?;
renderer.commit_load(|_| {}).await?;
```

`AntiAliasing` has a field `taa: Option<TaaConfig>`. `Some` requires
`msaa_sample_count: None` and `smaa: false`: `AntiAliasing::validate` rejects
the combination (`TaaCombinedWithOtherAa`) rather than picking one. The
builder takes the same value (`with_anti_aliasing`).

Switching into or out of TAA is structural like any AA change: pipelines
recompile at the next `commit_load`, and the history is reset. A change
*inside* `TaaConfig` (render scale, sharpening, tuning) takes a light path:
no pipeline or texture rebuild and no history reset. Sharpening on or off
builds or drops only the sharpening pass.

Supersampling and TAA are exclusive: under TAA, `set_render_scale` with any
value other than 1 is rejected, and turning TAA on while a supersampling scale
is set is rejected too (both `TaaCombinedWithOtherAa`). Under TAA the render
scale is `TaaConfig::render_scale`.

### 2.2 `TaaConfig`

| Field | Default | Meaning |
|---|--:|---|
| `render_scale` | 1.0 | Render size / output size, `0.5..=1.0`. Quantised to 0.01 (`RENDER_SCALE_STEP`); NaN becomes 1.0. |
| `sharpen` | 0.0 | RCAS strength after the resolve, `0..=1`. 0 = no pass. |
| `tuning` | `TaaTuning::default()` | The resolve's tuning, §2.4. |

`TaaConfig::native()`, `TaaConfig::preset(p)`, `matching_preset()`,
`clamped_render_scale()`, `upscales()`. `MIN_RENDER_SCALE` is 0.5.

### 2.3 `TaaPreset`

| Preset | Render scale | Pixels shaded | Sharpen |
|---|--:|--:|--:|
| `Native` | 1.0 | 100% | 0 |
| `Quality` | 0.67 | 45% | 0.25 |
| `Balanced` | 0.58 | 34% | 0.25 |
| `Performance` | 0.5 | 25% | 0.25 |

The presets are exact 0.01 steps. Native does not sharpen: a native resolve
that needed sharpening would be blurring, which is a bug. The 0.25 for the
upscaling presets comes from a sweep (§7.5).

### 2.4 `TaaTuning`

Every field is a uniform of the resolve: changing one never recompiles and
never resets the history. `Default` is the shipped tuning. Speeds are in
output pixels per frame.

| Field | Shipped | What it does | Raise it | Lower it |
|---|--:|---|---|---|
| `gamma_still` | 1.25 | Variance-clip box half-width (standard deviations) for a still pixel. Widened automatically when upscaling (§3.7). | Less flicker on still detail, more history survives (ghosting risk) | Tighter, more shimmer on thin and specular detail |
| `gamma_moving` | 0.75 | The same for a moving pixel | Longer trails | Shorter trails, more flicker under motion |
| `gamma_moving_speed` | 2.0 | Speed at which the box has narrowed from still to moving | Slow motion keeps the still box longer | Narrows sooner |
| `disocclusion_threshold` | 0.05 | Relative depth slack of the disocclusion test | Misses more reveals (ghosts) | More false reveals (one frame of aliasing each, flicker) |
| `max_history_native` | 16 | History-length cap at native resolution (frames) | Smoother still image, slower to forget | Noisier still image |
| `max_history_upscaling` | 64 | Upper bound of the upscaling cap (the cap itself is `max_history_native` times the pixel ratio: 35.6 at 0.67, 64 at 0.5) | | Caps the upscaled history shorter; 0.5 flickers more |
| `motion_alpha` | 0.2 | Minimum current-frame weight at `motion_alpha_speed` (ramps from 0) | Sharper and noisier under motion | Softer under motion |
| `motion_alpha_speed` | 8.0 | The speed at which `motion_alpha` is reached | | |
| `max_history_moving` | 2.0 | History cap for a pixel moving at `max_history_moving_speed` or faster | Smoother motion, slower to sharpen after stopping | Sharpens again faster after motion stops |
| `max_history_moving_speed` | 1.0 | The speed at which the cap reaches `max_history_moving` | | |
| `motion_edge_alpha` | 1.0 | Minimum current-frame weight on a motion boundary (3x3 with surfaces moving differently) | | Smoother moving silhouettes, more trail on them |
| `motion_edge_speed` | 2.0 | The relative speed at which `motion_edge_alpha` is reached | | |
| `reactive_transparent` | 0.3 | Reactive value per unit of alpha written by transparent surfaces, particles and lines | Fewer trails behind and on particles, more shimmer on still glass and water | More trail, steadier glass |
| `reactive_material` | 0.2 | Reactive value of materials resolved `Reactive` (§2.6) | Less smearing of animated colour, more aliasing on those surfaces | More smearing |
| `reactive_ssr` | 0.0 | Extra reactivity from SSR reflectivity | For mirrors that trail under camera motion | |
| `mip_bias_extra` | 0.0 | Added to `log2(render_scale)` for the texture LOD bias | Blurrier textures | Sharper textures, more shimmer (−0.5 raised flicker p99 from 4 to 6 at 0.67) |

Two values are constants, not tuning: the clip weight (`CLIP_ALPHA`, 1.0 per
unit of clip amount) and the reconstruction kernel's exponent
(`RECONSTRUCTION_SHARPNESS`, 2.29), which the reference capture shares.

### 2.5 Render scale at runtime

```rust
renderer.set_taa_render_scale(0.58)?;   // errors without TAA, or on NaN
let scale = renderer.taa_render_scale(); // Option<f32>, quantised
```

This is the entry point for a resolution governor. Change this, never the
canvas size. It is light: no pipeline compile, no commit, no history reset.
Step it every few seconds at most, with hysteresis (separate thresholds for
lowering and raising, and a hold time after each step), never per frame: a
step is a one-frame hitch.

On the next frame, only if the quantised scale changed:

- **Rebuilt** at the new size: every render-size target (visibility,
  barycentrics, normals, derivatives, velocity, reactive, depth, opaque,
  transparent, composite, the prep and SSR targets, HUD depth) and the bind
  groups that view them, the HZB pyramid, the light-culling and froxel grids.
  The old textures are destroyed a few frames later, so memory briefly holds
  both sizes. Nothing compiles.
- **Kept**: the TAA history (output size; the previous depth is carried
  across), bloom and the effects targets, LOD and coverage LOD (measured in
  output pixels, so nothing pops), the jitter cycle (its phase count follows
  the new ratio).
- **Restarted**: SSR's temporal accumulation (re-converges in about ten
  frames) and the volumetric haze's froxel history.

Measured cost of a step (§7.4): the image error jumps by +1.03 on the
step-up frame and decays to +0.18 within 4 frames; a step down is better than
a constant 0.5 for about 6 frames.

### 2.6 Materials: `temporal`

`MaterialTemporal` says how TAA treats a material's colour over time:

| Value | Meaning |
|---|---|
| `Auto` (default) | `Reactive` for flipbooks and for custom materials whose WGSL reads `frame_globals` (unless the custom material's registration says otherwise); `Stable` otherwise. The scene loader resolves UV flow and animation clips that drive a factor, texture transform or uniform to `Reactive`. |
| `Stable` | The colour is stable: accumulated normally. |
| `Reactive` | The colour changes over time: history is distrusted (`reactive_material`). |

- `AwsmRenderer::set_material_temporal(material_key, temporal)`
- `AwsmRenderer::set_custom_material_temporal(shader_id, temporal)` sets what
  `Auto` resolves to for every instance of a custom material.
- Scene files: `temporal = "stable" | "reactive"` on a material asset
  (`MaterialDef::temporal`) and on a custom material definition
  (`MaterialDefinition::temporal`); absent = auto.

No effect without TAA.

### 2.7 Resets

| Call | When |
|---|---|
| `AwsmRenderer::reset_temporal_history()` | Camera cuts. Forgets all TAA history and restarts the jitter cycle. |
| `AwsmRenderer::reset_motion_history(transform_key)` | An object teleported: its subtree, instances and skins get zero motion this frame. Call it in the frame of the jump, after setting the new transform. |
| `Transforms::reset_history(transform_key)` | The same for one transform only (children not included). |
| `Skins::reset_history`, morph `reset_history` | Lower-level, per skin and morph set (`reset_motion_history` calls these). |

All are free when TAA is off. The renderer resets the TAA history itself on
an AA-mode change, an output-size change, a perspective/orthographic flip,
scene load and teardown, entering or leaving the `FreezeJitter` view, and when
its camera-jump guard fires (rotation over 30°, or translation over 20% of the
distance to the scene bounds, in one frame). The guard is a backstop only: it
cannot see a teleport inside a world larger than that distance.

### 2.8 Debug views

`AwsmRenderer::set_taa_debug_view(TaaDebugView)`, `taa_debug_view()`.
Views that need TAA data show the normal image when TAA is off.

| View | Shows |
|---|---|
| `None` | Normal output |
| `Velocity` | Motion vectors: hue = direction, brightness = length (1 px/frame = full); sky grey |
| `ReprojectionError` | \|current − previous frame warped by v\|, as a heatmap |
| `HistoryLength` | `n / N_max`, black to white |
| `Rejection` | R = off-screen reset, G = disocclusion, B = clip amount |
| `Reactive` | The reactive mask |
| `Confidence` | Upscaling only: the weight of the nearest current-frame sample |
| `FreezeJitter` | Normal output with the jitter held at zero (separates jitter bugs from rejection bugs; resets the history on entry and exit) |
| `HistoryOnly` | Only the clipped history side of the blend |
| `CurrentOnly` | Only this frame's reconstruction |

When a ghost appears, look in this order: `FreezeJitter` (gone? jitter is
leaking into the vectors), `Velocity` and `ReprojectionError` under the ghost
(a wrong or undilated vector), `Rejection` (should it have been clipped or
flagged?), `HistoryLength` (`n` should drop where history is clipped),
`Reactive` (an unflagged transparent or animated material). Only then the
tuning, and re-run every test in §7, because each value trades ghosting
against flicker.

### 2.9 Device path and fallback

- `AwsmRenderer::taa_velocity_path() -> TaaVelocityPath`:
  `GeometryTarget` (a fifth geometry target; needs
  `maxColorAttachmentBytesPerSample >= 36` and 10 storage buffers per shader
  stage) or `PrepReconstruction` (the prep pass reconstructs vectors; §3.10).
- `AwsmRenderer::set_taa_velocity_path_override(Option<TaaVelocityPath>)`:
  forces a path for testing (takes effect at the next `set_anti_aliasing`).
- `AwsmRenderer::taa_fallback_overlay() -> TaaFallbackOverlay`: on the
  fallback path, which deforming meshes get exact vectors from the velocity
  overlay (§3.10): `All` (10 or more storage buffers per stage), `NoVertexHooks`
  (7-9: vertex-hook meshes stay reactive) or `Off` (fewer than 7).
- `AwsmRenderer::set_taa_fallback_storage_limit_override(Option<u32>)`:
  replaces the device's storage-buffer limit in that decision, for testing
  each outcome on a capable device (next `set_anti_aliasing`).
- `AwsmRenderer::taa_fallback_reason() -> Option<&str>`: why this renderer
  replaced TAA by MSAA 4x, if it did. That happens once, when a TAA pipeline
  fails to compile at a commit or the TAA pass cannot be built. Sticky for the
  renderer's lifetime; editors and players show it instead of offering TAA
  again.

### 2.10 Debug readbacks and test hooks

Not for per-frame use (each allocates and syncs).

| Call | Returns |
|---|---|
| `debug_read_velocity()` | The last frame's vectors, render size, UV units (`uv_now − uv_prev`); sky `(0, 0)`, behind last frame's camera `(2, 2)` |
| `debug_read_depth()` | The last frame's raw depth |
| `debug_read_reactive()` | The last frame's reactive mask (exactly 1.0 = an opaque-path HUD pixel) |
| `set_debug_motion_history_check(true)` + `debug_verify_motion_history()` | A byte-for-byte check that what the frame's shaders read as "previous" (transforms, joints, morph weights, instances) equals the previous frame's current contents, except in ranges marked fresh, which must equal this frame's |
| `set_taa_debug_repeat(n)` | Dispatch the resolve `n` times a frame, for cost measurement: `(T(n) − T(1)) / (n − 1)` |
| `set_jitter_override(Some(JitterOverride::Fixed(j)))` | Force a camera jitter even with TAA off (the reference capture uses it) |

Hidden harness hooks: `set_taa_debug_view_raw` (internal views 10-13),
`set_taa_experimental_tuning` (overrides `CLIP_ALPHA`),
`set_taa_debug_seed_repeat`, `set_taa_debug_overlay_repeat` (the fallback's
velocity overlay drawn `n` times), `set_taa_debug_overlay_check(true)` +
`debug_read_taa_overlay_check()` (the overlay built as a variant that counts,
per mesh it drew, the pixels it wrote and how many of them a depth-EQUAL or
the shipping depth compare would have lost).

GPU pass timings (`profiling`): `Taa` (the resolve), `TaaRcas` and
`TAA Overlay` (§3.10).

### 2.11 Scenes, the loader, the editor and MCP

- Scene schema: `post_process.anti_aliasing.taa` (`TaaSettings`:
  `render_scale`, `sharpen`, `tuning` with every `TaaTuning` field; every
  field defaults to the shipped value, so a file may carry only what it
  changes). With `taa` present the loader ignores `msaa` / `smaa`.
- `load_scene_for_player` applies the scene's anti-aliasing before its commit
  and replaces the renderer's mode. A scene with no AA block applies MSAA 4x,
  as in 0.45. An authored TAA that cannot be set up falls back to MSAA 4x with
  a warning.
- Editor: the AA mode, preset, sharpening and tuning knobs are in the scene
  settings, saved with the project and bundle; debug views are transient.
- MCP: `set_anti_aliasing` (`mode: none | msaa | smaa | msaa_smaa | taa`,
  `taa_preset`, `taa_render_scale`, `taa_sharpen`, one flat `taa_<name>`
  param per tuning field, `taa_debug_view`); read back with
  `get_post_process` and `get_view_options`.

## 3. How it works

### 3.1 Sizes

- **Render size** `R`: what the scene is rasterised and shaded at (surface ×
  render scale).
- **Post size** `O`: what TAA writes, and what bloom, effects (haze, DoF) and
  display run at. Under TAA it is the surface size. Without TAA, post size
  equals render size, which keeps supersampling exactly as in 0.45.

`FrameSizes { surface, render, post }` (`size.rs`) carries the three.

### 3.2 Jitter

- Halton base 2 (x) and base 3 (y), minus 0.5, in render pixels. Index
  `(frame_index % N) + 1`: index 0 would land on a pixel corner every cycle.
- `N = ceil(8 · (O.w / R.w)²)` phases: 8 at native, 18 at 0.67, 32 at 0.5, so
  each output pixel gets about 8 render samples per cycle.
- Constant amplitude, never scaled with motion.
- Applied as `translate(2·j.x / R.w, −2·j.y / R.h, 0) · projection`, which is
  right for perspective and orthographic projections and either depth
  convention. Render pixel `p` then holds the scene at unjittered position
  `p + 0.5 − j`; every formula in the resolve uses that.
- The jitter restarts at phase 0 on every history reset.
- Jittered matrices: the geometry raster and everything that reads the
  depth buffer (shadow-receiver reconstruction, SSR trace, prep's world
  position, DoF and haze depth reads). Unjittered: motion vectors on both
  ends, history reprojection, frustum culling, LOD screen error.
- SSR's march jitter and the volumetric froxel jitter offset their index by 7
  under TAA so they do not move in step with the camera jitter.

The camera uniform grew from 560 to 656 bytes in every mode:
`view_projection_unjittered`, `jitter` (xy = this frame, zw = previous, render
pixels) and `prev_position` (billboards rebuild their previous facing from
it; `.w` carries the texture LOD bias). Under TAA, `prev_view_projection` is
the last **rendered** frame's matrix, not the last camera set.

### 3.3 Motion vectors

A vector is this surface point's unjittered screen position now minus the
same point's unjittered screen position last frame, in UV units:

```
clip_now  = view_projection_unjittered · world_now(point)
clip_prev = prev_view_projection        · world_prev(point)
v         = uv(clip_now) − uv(clip_prev)
```

`world_prev` is the same vertex evaluated with last frame's world matrix,
instance transform, joint matrices, morph weights and frame time (for vertex
hooks), interpolated with this frame's barycentrics. The vertex shader runs
the vertex function twice; the fragment divides per fragment (interpolating
divided positions is wrong under perspective). The target is `rg16float` at
render size, a fifth geometry target, in TAA mode only. Masked and
custom-vertex variants write it too. Sky pixels get the rotation-only vector
from the view direction.

### 3.4 Previous-frame state

The previous transforms (full 112-byte stride), instance transforms, joint
matrices and morph weights live in **one** storage buffer, `prev_state`, in
three regions: Chrome on the M1 grants 10 storage buffers per stage, and
separate buffers needed 12 in the largest variant. The region bases and
`prev_time` ride in spare words of `frame_globals` (zero with TAA off).

- After the last pass of a frame, each current buffer that was uploaded since
  the last copy is copied to its previous region (`copy_buffer_to_buffer`). A
  static scene copies nothing.
- At the start of the frame, before any pass, every new, reused or relocated
  range gets `prev = current` (G10): an object that appears has zero motion,
  never a stale vector from a recycled slot. A relocated instancer block gets
  one frame of zero motion.
- The previous depth (a depth-format copy, bound as `texture_depth_2d`) is
  copied right after the resolve, its only reader.
- On the 32-byte fallback all of it is kept when the velocity overlay exists
  (§3.10); without the overlay only the transforms are.
- With TAA off none of this exists and nothing is copied.

### 3.5 The resolve

One compute pass at post size (8x8 workgroups, one invocation per output
pixel), after the SSR composite and before bloom, effects and display.
Inputs: the render-size composite (lit HDR), depth, velocity, reactive mask,
previous depth, previous history, the camera uniform. Outputs: this frame's
history (`rgba16float`, post size, colour in the tonemapped working space, `.a`
= history length `n`) and `taa_out` (linear HDR, post size). Every 3x3 loop is
unrolled at template time; no `switch`, no per-invocation arrays (the Apple
GPU rules).

For output pixel `q`:

1. **Where it lands.** `r_pos = (q + 0.5) / O · R`, `p0 = floor(r_pos)`.
2. **Current colour.** For the 3x3 render pixels around `p0`: tonemap
   (`c / (1 + max(c))`, the Karis weighting, G11), weight by
   `exp(−2.29 · d²)` where `d` is the distance from `r_pos` to where the sample
   really is (`p + 0.5 − j`), accumulate the weighted colour and the
   unweighted YCoCg moments. `confidence` = the largest weight (1 when a
   sample landed on the pixel centre).
3. **Dilated vector (G3).** The vector of the nearest-depth pixel of the 3x3.
   The vector of the farthest is kept too, for the motion-boundary test.
4. **History fetch (G4).** `uv_hist = uv_q − v`. Off screen: reset this
   pixel. Otherwise a 5-tap Catmull-Rom fetch (clamped to ≥ 0), `n` from the
   nearest texel.
5. **Disocclusion (G6).** Last frame's linear depth at `uv_hist` (2x2
   footprint) against the expected previous distance of this surface point;
   flagged when something nearer was there, with relative slack
   `disocclusion_threshold`. Both distances are previous-frame distances,
   so a fast dolly does not trip it. A second test on the pixel's **own**
   surface: a pixel beside a moving object takes the object's vector, so the
   first test would compare the object with itself. For a surface that did
   not move, the expected distance is exact and the slack is the surface's
   depth slope across a pixel. Away from nearer surfaces this test takes the
   farthest depth of the previous 2x2 (was the surface visible anywhere in
   it); beside a nearer surface it fires only when that surface moves relative
   to the pixel's own, and then takes the nearest. Errors fall on the safe
   side: a false reveal costs one frame of aliasing, a missed one a ghost.
6. **Variance clip (G5).** `mean ± γ·σ` of the plain current 3x3 in YCoCg of
   the tonemapped colour; the history is clipped along the line to the mean
   (never a per-channel clamp). `γ` goes from `gamma_still` to `gamma_moving`
   with speed. The box has a floor of 1.5e-3, a few steps of the history's
   `rgba16float` quantisation. `clip_amount` = how much of the history was
   cut. The clip is unconditional; a test pins it.
7. **Blend weight (G7).** History cap: the native or upscaling cap (§3.7),
   shortened towards `max_history_moving` with speed. Then

   ```
   n      = min(n_prev, cap)
   alpha  = 1 / (n + 1)
   alpha  = max(alpha, motion_edge_alpha · edge_factor)       // motion boundary
   alpha  = max(alpha, motion_alpha · ramp(speed))
   alpha  = max(alpha, reactive)                               // 3x3 max of the mask
   alpha  = max(alpha, clip_amount · CLIP_ALPHA)
   alpha *= mix(0.3, 1, confidence)      // upscaling only
   alpha  = 1 on reset, off screen, disoccluded
   result = mix(history_clipped, current, alpha)
   n_next = min(n + 1, cap) · (1 − clip_amount)                // 1 on reset
   ```

   The motion boundary is a 3x3 whose nearest and farthest surfaces move
   differently: a moving silhouette's coverage changes every frame, and its
   history is the average coverage of the last frames, not this one's. The
   speed-dependent cap exists because history resampled every frame softens
   a little each time; with a 16-frame memory, 60% of that softness was still
   there 8 frames after the camera stopped.
8. **Outputs.** History = `(result, n_next)`, `taa_out = untonemap(result)`.
   A non-finite result writes the current colour with `n = 0`.

Reset frames (§2.7) write the current colour with `n = 1` everywhere.

### 3.6 Reactive mask

`reactive` (`r8unorm`, render size) says how much to distrust history: 0 =
trust, 1 = ignore. The resolve takes the max over the same 3x3.

- **Transparent surfaces, particles, lines** write
  `alpha · reactive_transparent` as a second target of the transparent pass,
  blended with max.
- **Reactive materials** (§2.6) and the fallback path's deforming meshes
  that its velocity overlay does not cover (§3.10) are written by a "seed"
  draw: one full-screen triangle at the start of the
  transparent pass that reads the material meta of the opaque surface at each
  pixel (`r8unorm` is not a storage format, so a compute pass cannot write
  it). Drawn only on frames that need it: a reactive material is live, a
  HUD is drawn, the fallback path marks deforming meshes, or SSR runs with
  `reactive_ssr > 0` (the seed then adds `reflectivity · reactive_ssr`).
- Every reactive value is capped at 0.99. Exactly 1.0 means "HUD" (§3.8).
- Content drawn through the public transparent-pass hook writes no reactive
  value.

### 3.7 Upscaling

- The scene renders at `R = O · render_scale`; the resolve reconstructs `O`.
  Kernel distances are in render pixels, so the kernel does not change with
  scale.
- **History cap** = `max_history_native` × (output pixels / render pixels), up
  to `max_history_upscaling`: 35.6 frames at 0.67, 64 at 0.5. The history
  then holds about as many render samples per output pixel as at native. A
  fixed cap of 24 left fine detail of an upscaled still image flickering.
- **Still clip box** widens by 0.8 per unit of (output / render − 1): 1.25
  native, 1.74 at 0.67, 2.25 at 0.5. A feature smaller than a render pixel is
  seen only by some jitter phases, and the native box clipped it out on the
  frames that missed it: at 0.67 one isolated feature in six disappeared from a
  still image, one in twenty with the wider box. The cost is about 12% more
  error on the strip a moving object leaves.
- **Confidence**: with no render sample near the output pixel this frame, the
  current weight is scaled down (`mix(0.3, 1, confidence)`). Measured, it is
  near 1 on 95% of the image and barely modulates.
- **Texture LOD bias** `log2(render_scale) + mip_bias_extra` (§3.9).

### 3.8 HUD

The HUD must not be accumulated or jittered.

- HUD meshes with a transparent pipeline are drawn after the resolve, into
  `taa_out` at post size, with the unjittered view-projection and a post-size
  HUD depth. They are sharp at any render scale and never sharpened.
- HUD meshes shaded by the opaque pass cannot move there without an opaque
  pass at post size. Under TAA their geometry is rasterised with an
  unjittered twin of the camera uniform, the seed marks their pixels with
  exactly 1.0, and the resolve passes those pixels through (bilinear from the
  render image, no history, no reconstruction, no spread to neighbours).
  Below scale 1 they are therefore rasterised at render size and soft.

### 3.9 Texture LOD bias

`bias = log2(render / post) + mip_bias_extra` (negative when upscaling: a
texture sampled at render resolution picks the mip the output resolution
needs). It rides in the camera uniform's `prev_position.w` (0 without TAA).

- Opaque materials: the geometry pass scales the barycentric derivatives it
  stores by `2^bias` (a `mip_bias` geometry variant, on in every TAA path).
  Those derivatives feed only texture gradients, so custom materials get it
  without doing anything.
- Transparent pass: `textureSampleBias` in the texture-pool helpers.
- Custom WGSL that computes its own LOD (`textureSampleLevel`) should add
  `camera_raw.prev_position.w`.
- Alpha-test cutouts sample LOD 0 and need none.

### 3.10 The 32-byte fallback (`PrepReconstruction`)

On devices below 36 bytes per sample (or with fewer than 10 storage buffers
per stage) there is no fifth geometry target. The prep pass, which already
reconstructs world position from depth per pixel, writes the vectors to an
`rg32float` storage texture (`rg16float` is not a core storage format):

```
P      = world position from depth (jittered inverse)
P_prev = M_prev · inverse(M) · P        // inverse(M) = transpose of the stored normal matrix
v      = uv(VP_unjittered · P) − uv(prev_VP · P_prev)
```

That is exact for the camera and rigid objects (measured identical to the
geometry path on rigid scenes, §7.3). It cannot see skinning, morphs, vertex
hooks or per-instance motion.

**The velocity overlay** gives those meshes exact vectors. Right after the
prep pass, a render pass draws only the meshes that deformed this frame (a
joint matrix, morph weight or instance transform written with a new value)
and the vertex-hook meshes (every frame), with the geometry pass's TAA vertex
stage verbatim: the current and the previous evaluation, unjittered on both
ends (§3.3). Its fragment divides per fragment and `textureStore`s the vector
into the same `rg32float` texture, over the prep's. Masked and vertex-hook
variants are covered; the HUD is not drawn. One frame after the last
deformation the previous state equals the current one, the mesh is not
drawn, and the prep's rigid vector is exact again. Same frame, same draw
commands as the geometry pass (buffers, LOD cut, indirect arguments), so the
same triangles.

- **One writer per pixel.** A fragment stores only if its triangle index,
  mesh meta offset and instance id equal what the geometry pass wrote into
  the visibility and barycentric targets at that pixel. That makes the result
  independent of depth precision between the two pipelines, and masked meshes
  need no alpha test (a cut-out fragment never matches). Depth is attached
  read-only with the main compare, only to cull early. Measured with the
  debug check (`set_taa_debug_overlay_check`): a depth-EQUAL test would have
  lost 0.7-1.7% of the overlay's pixels on the test scenes (the two pipelines'
  depths differ by ulps); the shipping compare loses none, and every drawn
  mesh wrote pixels.
- **Limits.** The overlay's vertex stage binds what the geometry pass's TAA
  vertex stage binds: 7 storage buffers (transforms, `prev_state`, mesh meta
  array, morph weights and values, joint matrices and weights), 10 for a
  vertex hook (plus materials, material meta, the geometry pool and texture
  transforms on group 0; a hook can sample textures). `TaaFallbackOverlay`
  is a pure function of the device's limit: 10 or more covers everything;
  7-9 (the WebGPU default is 8) covers skins, morphs and instances and leaves
  vertex-hook meshes reactive; below 7 there is no overlay.
  `wgsl_validation` counts the bindings of each generated variant against
  those constants. A hook variant's fragment stage also sees the texture
  pool's arrays plus the two identity targets; past the device's
  sampled-texture limit that hook stays reactive.
- **Previous state.** With the overlay the joint matrices, morph weights and
  instance transforms keep previous copies as on the geometry path (§3.4,
  G10 included).
- **What stays reactive.** A deforming mesh the overlay does not cover (the
  limit, or a vertex-hook variant not compiled yet) is made reactive at 0.5
  while it deforms (2-frame linger), a vertex hook always, as before the
  overlay.
- **Cost.** Nothing on frames where no deforming mesh is visible (no render
  pass). Otherwise one draw per such mesh with two vertex evaluations and,
  per fragment, two texel loads and a compare (§7.3).

### 3.11 Sharpening

RCAS (FSR1's robust contrast-adaptive sharpening) on `taa_out`, a separate
compute pass at post size, only when `sharpen > 0`. It runs before the
HUD-after-resolve pass and skips HUD pass-through pixels, so no HUD is
sharpened. It is off for every ghosting test (G12) and must never hide a
native resolve that blurs.

### 3.12 Where it sits

```
render size: geometry (+velocity) → shading → transparents (+reactive, seed) → composite → SSR
TAA:         composite (R) → resolve → taa_out (O) → RCAS (optional) → HUD after resolve
post size:   bloom → effects (haze, DoF) → display
```

Haze and DoF read depth through `load_depth_post`, which maps a post pixel to
render pixels and takes the nearest depth of the 2x2 footprint; DoF's CoC
buffer is at post size. The picker maps the pointer through the render size.
Under TAA, LOD screen error and coverage counts are measured in output pixels.

### 3.13 Cost when off

With TAA off there is no velocity or reactive target, no previous-state
buffer or copy, no resolve, no seed, no sharpening pass. The TAA-off geometry
shader sources are byte-identical to 0.45, and TAA-off frames match 0.45
pixel for pixel on the six baseline scenes (36/36). The camera uniform is 96
bytes larger in every mode.

Memory with TAA on (from the formats; at 1920x1435 output and native scale,
2.76 M pixels): velocity 4 B/px, reactive 1 B/px, previous depth 4 B/px at
render size; two history textures and `taa_out` at 8 B/px each at post size;
previous-state copies at the size of the current buffers (112 B per
transform). On the fallback path the velocity texture is 8 B/px; its
velocity overlay allocates nothing of its own (bind groups and pipelines
only) and exists only on that path.

## 4. The ghosting rules

Ghosting is history that should have been thrown away and wasn't. The
earlier forward-renderer TAA (removed in `c42b6805`) broke most of these.

| # | Cause | Rule | Where enforced | Pinned by |
|---|---|---|---|---|
| G1 | Wrong motion vectors | A vector is the same surface point's screen motion, including camera, object, instance, skin, morph and vertex-hook motion | geometry pass second vertex evaluation, `prev_state` | device: analytic vectors within 0.02 px and supersampled reprojection error per scene; `taa_prev_functions_mirror_current`, `taa_motion_vector_divides_per_fragment`; motion-history byte-exact self-check |
| G2 | Jitter in the vectors | Unjittered matrices on both ends; jitter removed where the current frame is sampled | camera uniform, resolve step 2 | `taa_resolve_shader_validates` (sample at `p + 0.5 − j`); jitter unit tests in `taa.rs`; device G2 vector rows |
| G3 | Edge pixels with background velocity | Dilate: nearest-depth vector of the 3x3 | resolve step 3 | `taa_resolve_shader_validates` (depth convention); trail tests |
| G4 | Blurry history accumulates | 5-tap Catmull-Rom, never plain bilinear | resolve step 4 | `taa_resolve_shader_validates` (5 taps); stop and pan tests |
| G5 | Clip box too big | Variance clip in YCoCg of tonemapped colour, clip to mean, unconditional | resolve step 6 | `taa_resolve_shader_validates` (clip lines at top level of `cs_main`); flicker and trail tests |
| G6 | Revealed background keeps old foreground | Depth disocclusion test, plus the own-surface test | resolve step 5 | trail tests, static false-flag counts (§7.2) |
| G7 | Memory too long | `1/(n+1)` with a cap (16 native, scaled when upscaling, shortened with speed) | resolve step 7 | stop, trail and flicker tests; `history_cap` unit tests |
| G8 | Things with no correct vector | Reactive mask: transparents, particles, lines, reactive materials, HUD | transparent pass target, seed draw | `taa_reactive_transparent_and_line_shaders_validate`, `taa_reactive_seed_validates_and_reads_the_right_meta_words`; phase-3 tests |
| G9 | History reused across a cut | Resets on cuts, loads, size and AA changes, camera-jump guard | `reset_temporal_history`, renderer-side resets | cut tests (flagged and unflagged); `jump_guard` unit test |
| G10 | Objects appearing or teleporting | No valid previous value means prev = current | frame-start fix-ups, `reset_motion_history` | history burst / density / reset device tests; `fresh_ranges_*` unit tests |
| G11 | Blending in linear HDR | Blend and clip in tonemapped space | resolve steps 2, 6 | `taa_resolve_shader_validates` |
| G12 | Sharpening the errors | Sharpening separate, optional, off in ghosting tests | RCAS pass | `rcas_shader_validates_and_keeps_the_apple_rules`, RCAS unit tests |

Two things look like ghosting but are not: blur when nothing moves (jitter,
reconstruction or history filter; check the converged test, do not shorten
the history), and shimmer on thin or specular detail (too little history;
shortening history to cure ghosting makes it worse, which is why the tests
measure flicker too).

## 5. What an app must do

1. **Call the reset APIs.** `reset_temporal_history()` on every camera cut,
   `reset_motion_history(key)` in the frame an object teleports. Nothing
   detects an object jump, and the camera-jump guard cannot see a teleport in
   a large world. Shared-arena mode (a physics worker writes world matrices
   directly): the owner thread must call `reset_motion_history` for any jump
   the worker makes, in the frame the jump is rendered.
2. **Mark app-animated materials `temporal: reactive`.** `Auto` only sees
   flipbooks, UV flow, animation clips the scene authors and custom WGSL that
   reads `frame_globals`. Colour an app writes at runtime (a per-frame uniform
   or factor) is invisible to it: set `Reactive` with
   `set_material_temporal`, or in the scene file. Conversely, a custom
   material that reads the frame clock only for a subtle effect is always
   reactive under `Auto` and loses some anti-aliasing; set `Stable` there
   (§6).
3. **Choose the AA before the scene load commits.** `load_scene_for_player`
   applies the scene's authored anti-aliasing and replaces the renderer's. To
   use an app setting, either copy it into the scene config before loading
   (`scene.post_process.anti_aliasing.taa = Some(..)` with `msaa` and `smaa`
   false), which compiles it in the load's single commit, or call
   `set_anti_aliasing` + `commit_load` after the load (a second compile).
4. **Do not combine TAA with supersampling.** `set_render_scale` other than
   1 is rejected under TAA. Use `TaaConfig::render_scale` / `set_taa_render_scale`
   for resolution control.
5. **Drive a governor gently.** Step `set_taa_render_scale` every few seconds
   at most, with hysteresis.
6. **Custom WGSL with its own LOD** adds `camera_raw.prev_position.w` (§3.9).
7. **Hook-drawn geometry** (`begin_world_geometry_extension_pass`, the public
   transparent-pass hook) writes no vector and no reactive value. Keep it
   static, or mark nearby content reactive.

## 6. Known limits

- **Trails at the silhouette of a moving object.** On the first frame a pixel
  is uncovered, no history method can know its colour: even an oracle with
  perfect disocclusion has maxima of 18-39 at that age on the test scenes
  (§7.2). TAA's band error outside a 1.5 px ring around the silhouette is
  close to 2 (1.999 on `taa-motion`); inside the ring it is 12-16. The 05
  bound "max < 16 at every frame" is not reachable by any 1-sample-per-pixel
  temporal method on a high-contrast checker.
- **Contact pixels.** Where a moving object touches a surface, the two depths
  are within the disocclusion slack: worst max 127 on `taa-motion` against
  the oracle's 16.
- **Transparent and particle trails.** Particles, glass, UV-flow water and
  lines over a moving background trail for every AA mode, TAA included
  (§7.2); the reactive weight 0.3 trades some of that for still-image
  stability of glass and water. At 0.67 these trails are about twice the
  native ones (band over the content 23.63 against 12.68; the content's own
  trail 18.43 against 5.83), and still-image flicker over that scene reaches
  p99 4, the bound.
- **Skinned meshes** trail at the silhouette more than rigid ones (worst
  band mean 7.4 on `taa-skin`, 1.4 outside the ring) and their converged
  image is worse than MSAA's (0.341 against 0.168): a walking character's
  silhouette changes shape every frame.
- **Upscaled thin detail.** At 0.67 about 5% of isolated sub-render-pixel
  features drop out of a still image, at 0.58 about 32%, at 0.5 about 73%;
  thin text and wires look softer than native.
- **Opaque-path HUD below scale 1** is rasterised at render size and soft
  (§3.8): at 0.67 its pixels differ from a TAA-off render by 0.755 on average
  (worst frame 0.887, max 127), against 0.068 at native. Transparent-pipeline
  HUD meshes are sharp.
- **Hook-drawn geometry** writes no vectors and no reactive value (§5).
- **A render-scale step** restarts the volumetric froxel history and SSR's
  temporal accumulation (about ten frames to re-converge). It costs +0.5 to
  +1.1 ms of CPU on the step frame, and the frame after a 0.5 → 0.67 step
  costs +5.3 to +5.8 ms of CPU once (§7.6).
- **Time-reading materials are always reactive.** `Auto` marks any custom
  material whose WGSL reads `frame_globals` as reactive, even when the time
  dependence is a subtle twinkle. Those surfaces keep less history and look
  more aliased, most visibly at upscaled scales on high-contrast silhouettes.
  In Dreidel Derby every dreidel body is such a material, and the bodies hold
  most of TAA's remaining mid-round error (§7.7). Remedy: `temporal: stable`
  on materials whose time dependence is subtle.
- **Bright emissive highlights.** On the Dreidel Derby title screen about 30%
  of TAA's error sits on the menorah's candle flames and brass highlights;
  there TAA native is only narrowly better than MSAA and worse than SMAA and
  no AA (§7.7).
- **The resolve costs 1.03-1.17 ms** at 1920x1435 output, about 1.8x the
  0.6 ms the plan targeted (§7.6).
- **The 32-byte fallback** on a device with fewer than 10 storage buffers per
  stage loses vertex-hook motion (those meshes are reactive instead), and
  below 7 also skin, morph and per-instance motion (§3.10). With 10 or more
  its vectors match the geometry path's (§7.3).
  Two of the overlay's cases are verified only indirectly: no bundled scene
  has a masked (alpha-tested) deforming mesh (the identity test needs no
  alpha test, and masked meshes use the plain uniform-meta variant), and no
  bundled scene writes instance transforms (the instanced variant was checked
  with harness-side instance motion, `?mode=overlay-parity&move-instances`).
- **Once per jitter cycle**, static edges see a burst of false disocclusions
  at the largest jitter step (the 3x3 and the previous 2x2 are point samples
  at different sub-pixel positions). It passes the flicker gate and was left
  as is.
- **`rg16float` vectors** resolve to about 1/2048 of their length: 0.24 px on
  a 272 px teleport. Every non-teleport frame is within ±0.02 px.

## 7. Measured

Hardware and method unless noted: M1 Max, Chrome, the `taa-tests` harness
(`examples/taa-tests`) on the `taa-*` scenes of `examples/test-scenes`, 960x720,
fixed timestep and seeds. Colour errors are mean absolute differences on the
tonemapped, display-encoded image in 0-255 units. Gate thresholds are the
ones in `docs/plans/taa-upscaling/05-verification.md`, unrelaxed unless
stated. Native results are from `a2be12b1` with the shipped tuning (bit-exact
to `3c975597`); phase 4 from `19acddee`; phase 5 from `3c975597`; Balanced
0.58, phase 3 at 0.67, frame cost and Dreidel Derby from `0d878578` (its TAA
frames are bit-identical to `3430bf4b`, 30/30).

### 7.1 The reference, and which space decides

A reference frame for frame `k`: freeze time at `k`, render 64 jittered frames
with TAA off (64 Halton phases) and average them with the resolve's kernel.
That is what a perfect TAA converges to at that instant, for any scene and
any frame. At scale below 1 the reference is at full resolution.

Two averaging spaces were tried. Averaging in display-linear space, the
converged TAA image at the `taa-orbit` stop view sits at 1.185 from the
reference: the stop test (< 1) could not pass against it even with a perfect
history. Averaging with the resolve's own Karis weights (tonemapped space)
puts the same image at 0.819. **Decision:** the Karis-space reference decides
("what a perfect TAA converges to with the resolve's weights"), and the
linear-space result is reported next to it. The only verdict that differs is
the `taa-orbit` stop test (Karis 0.773 PASS, linear 1.080 FAIL).

The reference's own floor (64 reference phases against an 8-phase resolve
model): 0.450 on `taa-thin`, 0.114 on `taa-motion`.

Method deviations from 05:

- **Reprojection error** uses 64-phase supersampled frames warped with a
  Catmull-Rom filter, with masks for mixed-coverage pixels and for content
  without vectors, and a coverage requirement (≥ 50% of drawn pixels and of
  each moving object). The literal variant (no-AA frames, nearest warp)
  fails on aliased content even with exact analytic vectors (`taa-orbit` k90
  3.26 / p99 35, `instancing-stress` 8.07 / 68): that is the aliasing floor of
  the method, not a vector error.
- **Trail oracle.** To separate "TAA is wrong" from "no method can do this",
  an oracle with perfect disocclusion (it knows exactly which pixels were
  revealed) runs on the same frames.
- `lod-nanite`'s bundle has no `clusters.bin`; the `taa-cluster` scene
  replaced it.

### 7.2 Native gates

| Gate | Scene | Threshold | Measured (Karis) | Linear | Verdict |
|---|---|---|---|---|---|
| Vectors, analytic | `taa-motion`, `taa-orbit`, all G2 rows | \|v − v_exact\| ≤ 0.02 px | max 0.0037 px, 0 px over | | PASS |
| Vectors, reprojection | every scene | mean < 2, p99 < 12, coverage ≥ 50% | worst `instancing-stress` 1.90 / 9, `prefab-skinned-morph` 1.48 / 9 | | PASS except `taa-vertex-hook` k30/k60 |
| Vectors, reprojection | `taa-vertex-hook` k30, k60 | as above | errors 0.25 / 0.27, but one thin grazing sliver is under 50% coverage after the masks | | FAIL (coverage) |
| Motion-history self-check | 48 scenes, 120 frames | 0 mismatching bytes | 0 | | PASS |
| History burst / density / reset | `taa-motion`, `prefab-skinned-morph` | clean self-check, exact vectors | 0 mismatching, max 0.0037 px | | PASS 3/3 |
| Trail | `taa-motion` | band mean < 2, max < 16, every frame | worst mean 3.672 @k31, max 152 | 3.227 / 158 | FAIL |
| Trail | `taa-reveal` | same | worst mean 1.931, max 88 | 2.788 / 91 | FAIL (max) |
| Trail | `taa-skin` | same | worst mean 7.425 @k2, max 135 | 7.082 / 142 | FAIL |
| Stop | `taa-motion` | mean < 1, 8 frames after stopping | 0.606 | 0.805 | PASS |
| Stop | `taa-orbit` | same | 0.773 | 1.080 | PASS (linear FAIL) |
| Cut, flagged | `taa-cut` k120 | first frame vs TAA-off < 3 | 2.249 | | PASS |
| Cut, unflagged (guard) | `taa-cut` k240 | same | 1.883 | | PASS |
| Pan | `taa-orbit` strafe | every frame < 3 | worst 1.134 | 1.217 | PASS |
| Converged | `taa-thin` | TAA < MSAA 4x | 1.449 (MSAA 5.309, SMAA 5.269, none 6.078) | 1.979 (MSAA 5.525) | PASS |
| Flicker | 16 scenes | mean f2f < 0.5, p99 < 4 | worst `taa-transparent` 0.292 p99 3, `taa-thin` 0.272 p99 2 | | PASS 16/16 |
| HUD | `taa-hud` | HUD px vs TAA-off < 1 | worst 0.068, max 1 | | PASS |
| Transparent trail, band ∩ content | `taa-transparent` | mean < 2, max < 16 | TAA 12.68 (avg 5.58); MSAA 10.78 (avg 6.22); no AA 18.06 | | FAIL (every mode) |
| Transparent, content's own trail | `taa-transparent` | same | TAA 5.83 max 192 (avg 1.12); MSAA 5.35 (avg 3.09) | | FAIL (every mode) |
| TAA-off baseline | 6 scenes × MSAA/SMAA/none × 2 frames | bit-exact to 0.45 | 36/36, max diff 0 | | PASS |
| Player tests (Layer B) | | all | 77/77 | | PASS |

Converged image on the other scenes (INFO, not gate scenes; TAA / MSAA):
`taa-motion` 0.633 / 1.935, `taa-orbit` 1.037 / 3.075, `taa-reveal`
0.329 / 1.703, `taa-transparent` 0.690 / 2.866, `taa-hud` 0.939 / 2.497,
`taa-cut` 0.675 / 2.747, `taa-vertex-hook` 0.703 / 1.516, `taa-cluster`
0.607 / 1.045, **`taa-skin` 0.341 / 0.168 (TAA worse)**,
**`instancing-stress` 1.179 / 1.033 (TAA worse)**.

Trail broken down (Karis): band minus the 1.5 px ring around the current
silhouette: `taa-motion` 1.999 (max 85), `taa-reveal` 1.655 (max 41),
`taa-skin` 1.365 (max 72). The ring alone: 12.6, 16.0, 12.9. The oracle at
age 1 (first uncovered frame): `taa-motion` k55 oracle 0.58 / max 30, TAA
1.85 / 127, MSAA 3.85 / 56, no AA 3.84 / 56; `taa-reveal` k45 oracle 0.65 /
18, TAA 1.63 / 91, no AA 4.37 / 47. The oracle exceeds max 16 at age 1 on
every band measured (18-39) and is within it from age 2-3.

Static false disocclusions (30 still frames, Rejection view, G pixels per
frame): 0 on most frames of every scene, with the once-per-cycle burst (e.g.
`taa-motion` 1 px, `taa-orbit` 423 px, `taa-thin` 1008 px on the burst frame);
no pixel is flagged on all 30 frames.

Transparent content per region (static flicker at the shipped reactive
weights): particles 0.000, glass 0.339 (p99 6), UV-flow water 1.284 (p99 5),
lines 3.104 (p99 17), rest 0.044; whole image 0.292 p99 3.

### 7.3 Fallback path (`PrepReconstruction`, forced on the M1)

With the velocity overlay (§3.10), 960x720, the M1's 10 storage buffers
(`TaaFallbackOverlay::All`):

| | Geometry target | Fallback, overlay | Fallback, no overlay (before it; a limit below 7 measures the same) |
|---|---|---|---|
| Vectors (`?mode=vectors`, 120 lines) | 120/120 | 120/120 | 103/115 |
| `taa-vertex-hook` analytic max, k30 / k60 | 0.00155 / 0.00151 px | 0.00029 / 0.00030 px | 2.14 / 2.11 px (hook motion absent) |
| `prefab-skinned-morph` reprojection, k30 / k60 | 1.4771 / 1.4261 | 1.4775 / 1.4265 | 7.563 / 6.051 |
| `taa-skin` reprojection, k30 / k60 | 0.1772 / 0.1128 | 0.1772 / 0.1128 | 0.4961 / 0.4817 |
| `instancing-stress` reprojection, k30 / k60 | 1.8466 / 1.9037 | 1.8465 / 1.9037 | 1.8465 / 1.9037 |
| Per-pixel against the geometry path (`overlay-parity`, 30 frames) | | max 0.0003-0.009 px, none over max(0.02 px, 1 rg16float ulp) | `taa-skin` max 14.1 px |
| Instances moved every frame (`instancing-stress`) | | max 0.00097 px | max 0.65 px |
| Trail `taa-skin`, worst mean / max | 1.979 / 135 | 1.979 / 135 | 9.555 / 96 |
| Trail `prefab-skinned-morph` | 10.590 / 110 | 10.590 / 110 | 10.590 / 105 |
| Stop `taa-skin` / `prefab-skinned-morph` / `taa-vertex-hook` | 0.0966 / 0.1835 / 0.4172 | 0.0966 / 0.1835 / 0.4172 | 0.0905 / 0.1702 / 0.4172 |
| Converged `taa-skin` / `taa-vertex-hook` / `prefab-skinned-morph` | 0.1925 / 0.6344 / 0.2873 | 0.1925 / 0.6343 / 0.2873 | 0.2442 / 0.6953 / 0.3309 |
| Flicker `taa-vertex-hook` (others unchanged) | 0.0770 | 0.0770 | 0.1139 |

The vectors are the geometry path's to within its `rg16float` rounding (the
fallback stores `rg32float`); every gate line equals the geometry path's to
three decimals. Rigid scenes draw nothing in the overlay and their fallback
frames are bit-identical with and without it. The meshes that were reactive
now keep their history: converged error drops on all three deforming scenes,
`taa-skin`'s worst trail mean drops from 9.555 to 1.979, and the stop error
of `taa-skin` / `prefab-skinned-morph` rises by about 0.01 (all far below the
bound), as on the geometry path. With the WebGPU default limit (8),
`taa-vertex-hook` keeps the no-overlay numbers and the skinned scenes get the
overlay's.

The overlay's own pixels: a depth-EQUAL test would have lost 0.75-1.67% of
them (the two pipelines' depths differ by ulps); the shipping compare loses
none (debug check over all 48 bundled scenes, 40 frames each).

Cost at 1920x1435 (busy window, median of 3 runs), forced fallback minus
geometry target: `taa-skin` +0.096 ms with the overlay against +0.179 ms
without it (the reactive seed it replaces cost more), `kitchen-sink` +0.013
against −0.078, `instancing-stress` (no overlay draw) +0.075 against +0.080.
The overlay's draws by repeat-N (N = 1 against 32): 0.007 ms on `taa-skin`,
0.016 on `taa-vertex-hook`, 0.022 on `prefab-skinned-morph`, 0.015 on
`kitchen-sink`. That leaves out the pass's own setup (a read-only depth
attachment); the fallback-minus-native figures above bound the total.

`inverse(M)` as the transpose of the normal matrix against a per-pixel
inverse: prep pass 1.118 ms against 1.142 ms at 1920x1435 (repeat-N), equal
within noise.

### 7.4 Upscaling gates (`19acddee`)

Relaxed thresholds per 05 §4.5 (1.5x at 0.67, 2x at 0.5) for converged and
pan; flicker at the literal bound; stop is INFO at scale (its floor is the
upscaled converged image). Sharpening 0. Full-resolution Karis reference.

| Gate | Scene | 0.67 | Verdict | 0.5 | Verdict |
|---|---|---|---|---|---|
| Converged (MSAA 5.309) | `taa-thin` | 2.991 | PASS (below 1x MSAA) | 5.759 | PASS via 2x only (above 1x MSAA) |
| Converged (INFO) | `taa-orbit` (MSAA 3.075) | 2.332 | | 4.351 | above MSAA |
| Converged (INFO) | `taa-motion` (MSAA 1.935) | 1.175 | | 2.239 | above MSAA |
| Converged (INFO) | `taa-reveal` (MSAA 1.703) | 1.333 | | 2.658 | above MSAA |
| Pan | `taa-orbit` | worst 2.683 | PASS (also < 3) | worst 4.553 | PASS via 2x (FAIL at 3) |
| Flicker | `taa-thin` | 0.292 p99 2 | PASS | 0.284 p99 2 | PASS |
| Flicker | `taa-orbit`, `taa-motion`, `taa-reveal` | ≤ 0.149 p99 1 | PASS | ≤ 0.161 p99 2 | PASS |
| Stop (INFO) | motion / reveal / orbit / thin | 1.137 / 1.316 / 1.912 / 2.990 | | 2.371 / 2.616 / 3.816 / 5.856 | |
| Scale change 0.67 → 0.5 → 0.67 | `taa-orbit` | no frame > 3 | worst 4.589 @k82 | FAIL (literal) | |
| Isolated-detail dropout (INFO) | `taa-thin`, 4822 px | 5.0% dropped | | 73.0% dropped | |
| Trail (INFO) | `taa-motion` | 6.32 max 163; minus ring 3.77; ring 23.74 | | not measured | |

The scale-change failure is the 0.5 steady state under orbit (a constant-0.5
run is itself above 3, 4.4-4.6), not a reset pop: the step down is better than
constant 0.5 for about 6 frames (k60 3.381 against 4.508), the step up costs
+1.033 decaying to +0.175 in 4 frames.

Balanced (0.58, `0d878578`; 05 defines no threshold between 1.5x at 0.67 and
2x at 0.5, so both are shown; Karis, linear in parentheses):

| Gate | Scene | 0.58 | vs 1.5x | vs 2x |
|---|---|---|---|---|
| Converged | `taa-thin` (MSAA 5.309) | 4.233 (4.171) | PASS (also below 1x MSAA) | PASS |
| Converged (INFO) | `taa-orbit` (MSAA 3.075) | 3.191 (3.160) | PASS | PASS |
| Converged (INFO) | `taa-motion` (MSAA 1.935) | 1.631 (1.521) | PASS | PASS |
| Converged (INFO) | `taa-reveal` (MSAA 1.703) | 1.939 (2.000) | PASS | PASS |
| Pan | `taa-orbit` | worst 3.485 | PASS (FAIL at the unrelaxed 3) | PASS |
| Flicker | `taa-thin` / orbit / motion / reveal | 0.290 p99 2 / 0.158 p99 1 / 0.137 p99 1 / 0.138 p99 2 | PASS | PASS |
| Isolated-detail dropout (INFO) | `taa-thin` | 31.8% dropped | | |

Phase 3 at 0.67 (INFO, `0d878578`, Karis; native in parentheses): HUD pixels
against TAA-off worst 0.887, average 0.755, max 127 (0.068, max 1), still
under the < 1 bound; `taa-transparent` band over the content worst 23.63
(12.68; MSAA 10.78, no AA 18.06); the content's own trail 18.43 (5.83);
whole-image static flicker 0.361 p99 4, FAIL by equality (0.292 p99 3).
Per region: glass 0.391 p99 6, water 1.565 p99 6, lines 3.157 p99 17.

### 7.5 The sweeps behind the shipped values

| Value | Sweep | Chosen |
|---|---|---|
| `max_history_moving` 2 (new) | default 4: `taa-orbit` stop 1.144, pan 1.483, motion flicker 0.057; 2: 1.079, 1.206, 0.075; 1: stop 1.035 | 2 |
| `motion_edge_alpha` 1.0 (was 0.6), `CLIP_ALPHA` 1.0 (was 0.5) | combined with the above: `taa-motion` trail 3.23 (ring 12.24, contact 5.06), stop 1.063, pan 1.195, motion flicker 0.102 | 1.0 / 1.0 |
| `reactive_transparent` 0.3 (was 0.8) | whole-image static flicker of `taa-transparent` at `reactive_material` 0.2: 0.1 → 0.27 (p99 3), 0.3 → 0.30 (3), 0.5 → 0.32 (4), 0.8 → 0.36 (4); particle trail avg 8.1 at 0.8, 9.8 at 0.3 (MSAA 6.12) | 0.3 |
| `reactive_material` 0.2 (was 0.6) | 0.4 fails flicker (0.51-0.61) | 0.2 |
| Reactive clip-box narrowing | tried, settled on none (scale 1.0) and removed | none |
| `mip_bias_extra` 0 | `taa-thin` at 0.67: 0 → 3.158, flicker 0.376 p99 4; −0.25 → 2.977, 0.424 p99 4; −0.5 → 2.994, 0.483 p99 6; −1 → 3.348, 0.629 p99 9 | 0 |
| Preset sharpening 0.25 | `taa-thin` at 0.67: 0 → 3.158 / 0.376; 0.15 → 2.940 / 0.391; 0.25 → 2.795 / 0.403; 0.35 → 2.669 / 0.417 | 0.25 |
| Upscaling history cap 16 × pixel ratio | 0.67: cap 24 → flicker 0.376 p99 4, stop 1.930, pan 2.689; cap 35.6 → 0.328 p99 3, 1.970, 2.698. 0.5: cap 24 → 0.484 p99 5; 48 → 0.379 p99 4; 64 → 0.352 p99 4, stop 3.927, pan 4.580 | ratio-scaled |
| Still γ widening 0.8 per unit of (O/R − 1) | 0.67, `gamma_still` 1.25 / 1.75 / 2.5 / 4: isolated features dropped 16.5 / 5.0 / 2.4 / 1.3 %, trail ring 21.3 / 23.8 / 25.9 / 27.2; 0.5: dropped 76.6 / 73.7 / 72.9 / 72.4 % | 1.74 at 0.67, 2.25 at 0.5 |
| Confidence term | near 1 on 95% of the image | kept as designed |

The shipped values were checked bit-exact against `?tune=` overrides (10/10).

### 7.6 Frame cost

**Method** (05 §5): M1 Max, Chrome, 1920x1435 output, one tab, nothing else
rendering. Synthetic scenes (`0d878578`): 5 interleaved runs, 120 frames per
configuration after 60 warm-up frames, GPU frame = timestamp span per frame.
Frames are submitted back to back and drained every 4th, so the GPU does not
clock down between frames: draining every frame read light frames 1-1.5 ms
high at random. A sentinel configuration (`kitchen-sink`, no AA) before and
after every run read 4.686-4.921 ms, with at most 0.151 ms drift within a run.
A pass is timed by repeating it N times and taking `(T(N) − T(1)) / (N − 1)`,
because Apple GPUs overlap pass timestamps.

| Row (05 §5) | Target | Measured | Verdict |
|---|--:|---|---|
| Velocity target + second vertex evaluation (+ seed) | ≤ 0.2 ms | 0.093-0.123 ms (TAA − no AA − resolve, N=32) | met |
| Resolve, native 1920x1435 | ≤ 0.6 ms | 1.03-1.17 ms (frame and pass-timestamp slopes, N=8 and N=32) | not met (about 1.8x) |
| Resolve, 0.67 → 1920x1435 | ≤ 0.6 ms | about 1.10 ms (the resolve runs at output size) | not met |
| Dreidel Derby mid-round, TAA native | ≤ 11.5 ms | 12.13 ms paced, 12.22 ms unpaced (no AA 10.64, MSAA 14.75) | not met |
| Dreidel Derby mid-round, TAA 0.67 | ≤ 7.5 ms | 8.43 ms paced, 7.30 ms unpaced | met only unpaced |
| TAA off | unchanged | Dreidel Derby no AA 10.64 against 0.45's 10.67, MSAA 14.75 against 14.74; synthetic scenes within ±0.114 ms of 0.45 (one configuration consistently +0.06 / +0.09 ms); 36 baseline captures bit-identical | met |

Synthetic scenes, GPU frame median (ms; delta against no AA):

| Scene | No AA | MSAA 4x | SMAA | TAA native | TAA 0.67 | TAA 0.5 | 0.67 + RCAS 0.25 | Forced fallback |
|---|--:|--:|--:|--:|--:|--:|--:|--:|
| `kitchen-sink` | 4.813 | 5.713 (+0.90) | 5.012 (+0.20) | 5.980 (+1.17) | 4.922 (+0.11) | 4.562 (−0.25) | 5.101 (+0.29) | 6.040 (+1.23) |
| `instancing-stress` | 3.393 | 5.893 (+2.50) | 3.701 (+0.31) | 4.633 (+1.24) | 3.710 (+0.32) | 3.424 (+0.03) | 3.878 (+0.49) | 4.700 (+1.31) |
| `ssr` | 3.799 | 4.507 (+0.71) | 4.103 (+0.30) | 5.026 (+1.23) | 3.963 (+0.16) | 3.639 (−0.16) | 4.156 (+0.36) | 5.141 (+1.34) |
| `taa-thin` | 3.714 | 6.037 (+2.32) | 4.136 (+0.42) | 4.968 (+1.25) | 3.337 (−0.38) | 2.705 (−1.01) | 3.539 (−0.18) | 5.063 (+1.35) |

- Resolve per scene (`kitchen-sink` / `instancing-stress` / `ssr` /
  `taa-thin`): frame slope N=8 1.050 / 1.046 / 1.033 / 1.126 ms, N=32
  1.074 / 1.089 / 1.084 / 1.165 ms; the `Taa` pass-timestamp slope agrees
  within 0.013 ms.
- RCAS (`TaaRcas`): 0.164-0.193 ms.
- Reactive seed: no timestamp of its own; the transparent pass grows by
  0.076 ms with TAA on `kitchen-sink`, which bounds it; 0.03-0.04 ms by
  repeat-N.
- Forced fallback (`PrepReconstruction`) against the geometry target:
  +0.03 to +0.13 ms; with the velocity overlay (§3.10, §7.3) +0.01 to
  +0.10 ms.
- TAA native against MSAA 4x: cheaper on `instancing-stress` (−1.26) and
  `taa-thin` (−1.07), dearer on `kitchen-sink` (+0.27) and `ssr` (+0.52).
  TAA 0.67 is cheaper than MSAA on all four (−0.54 to −2.70) and within
  ±0.4 ms of no AA.
- Zero cost off against v0.45.0 (A B A B, 3 runs each): all 12 no-AA / MSAA /
  SMAA configurations within ±0.114 ms with mixed signs between passes;
  `taa-thin` no AA is +0.060 / +0.086 ms in both passes (3.71 against 3.65,
  about 2%), the only consistent sign.
- Resolution governor (`kitchen-sink`, 0.67 ↔ 0.5 every 120 frames, two
  runs): CPU `render()` median 2.10 ms, +0.5 to +1.1 ms on the step frame;
  the frame after a 0.5 → 0.67 step costs 7.4 / 7.9 ms of CPU (+5.3 to
  +5.8 ms) once, in both runs. The GPU cost of the step could not be
  separated from the idle-clock noise of that measurement.

**Dreidel Derby** (the real game, the same machine, canvas 1920x1435, the
game's own stats: the renderer's whole-frame GPU timestamp span, averaged over
2.5 s windows; median of run medians; 5 interleaved runs paced, 3 unpaced;
0.45 = the same game source built against awsm-renderer 0.45.0):

| | Mid-round paced | Mid-round unpaced | Title paced | Title unpaced |
|---|--:|--:|--:|--:|
| 0.45 no AA | 10.67 | 10.74 | 13.88 | 14.04 |
| 0.45 MSAA 4x | 14.74 | 14.80 | 19.37 | 19.21 |
| No AA | 10.64 | 10.74 | 13.86 | 13.98 |
| MSAA 4x | 14.75 | 14.77 | 19.35 | 19.15 |
| TAA native | 12.13 | 12.22 | 15.43 | 15.45 |
| TAA 0.67 | 8.43 | 7.30 | 10.28 | 10.31 |
| TAA 0.5 | 8.17 | 6.16 | 8.97 | 8.60 |

Run-to-run spread 0.03-0.1 ms (0.3-0.6 ms for the paced 0.67 and 0.5 runs).
Load average 1.4-2.8 during every run, with only the browser's GPU and
renderer processes and the window server above 5% CPU.

**Paced and unpaced.** The display runs at 120 Hz. The game halves its frame
rate to 60 fps when its smoothed GPU time exceeds 7.17 ms (0.86 of the
refresh period), so every mode above ran at 60 fps. A frame that takes 7-8 ms
in a 16.7 ms period leaves the GPU idle for half the time, and the GPU clocks
down between frames: the cheap modes then read 1.1-2.0 ms high (0.67: 8.43
paced against 7.30 unpaced; 0.5: 8.17 against 6.16), with a wider spread. The
modes that keep the GPU busy (no AA, MSAA, TAA native) read the same either
way, within 0.1 ms. "Unpaced" is a scratch switch that draws on every refresh
(0.67 and 0.5 then run at 120 fps, TAA native at 82, no AA at 93, MSAA at 68);
those numbers are closer to the real GPU cost of the cheap modes.

### 7.7 Converged quality in Dreidel Derby

The 05 §4.3 gate on the game itself: Dreidel Derby (scratch build of the game
with debug hooks, renderer `0d878578`), canvas 1920x1435, the title screen
and a mid-round frame.

- **Freeze.** The app's poses and camera are no longer read, clips and
  particles advance by 0, and the frame time is pinned. Two captures of the
  same frozen frame were bit-identical, with no AA and with TAA native after
  32 frames from two separate resets (max difference 0, both scenes).
- **Reference.** 64 frames with no AA and the camera jitter
  `jitter_offset(i, 64)`, 6 frames per phase, accumulated by the harness's own
  reference code (`examples/taa-tests/src/reference.rs`). Captures are
  lossless swap-chain readbacks. The Karis and display-linear references
  differ by 0.114 (title) and 0.224 (mid-round).

Mean error against the reference (Karis; linear in parentheses):

| Mode | Title | Mid-round |
|---|--:|--:|
| MSAA 4x | 1.310 (1.335) | 1.864 (1.877) |
| SMAA | 1.114 (1.128) | 1.675 (1.705) |
| No AA | 1.167 (1.188) | 1.885 (1.936) |
| TAA native, 32 frames | 1.217 (1.264) | 0.873 (0.921) |
| TAA 0.67, 32 frames | 1.761 (1.789) | 1.538 (1.517) |
| TAA 0.67, 64 frames | 1.844 (1.873) | 1.615 (1.595) |
| TAA 0.67 with sharpening 0.25 (INFO) | 1.713 (1.740) | 1.417 (1.398) |

**Verdict: PASS** (TAA native below MSAA 4x) on both scenes in both spaces,
with these caveats:

- On the title screen the margin is narrow (1.217 against 1.310), and TAA is
  above SMAA (1.114) and no AA (1.167) there. About 30% of TAA's error sits
  on the menorah's candle flames and brass highlights; its other edges are
  clean, while MSAA's and no AA's error is spread over every edge.
- Mid-round, TAA's error is half of MSAA's. What remains sits mostly on the
  dreidel bodies (a material that reads the frame clock, so always reactive)
  and the candle flames.
- TAA 0.67 is below MSAA mid-round but above it on the title screen.
- TAA 0.67 after 64 frames is slightly worse than after 32 in both scenes
  (+0.08). Not investigated.
