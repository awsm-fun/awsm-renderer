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
| MSAA 4x | `AntiAliasing::msaa4()` (the default) | geometric edges | shading and specular aliasing, sub-pixel detail between samples | +0.74 to +2.46 ms over no AA on the synthetic scenes, +4.1 ms in Dreidel Derby mid-round (1920x1435, §7.6) |
| SMAA | `AntiAliasing::smaa()` | geometric edges, as a post-process | sub-pixel detail, temporal shimmer | one post pass: +0.22 to +0.46 ms |
| MSAA + SMAA | `AntiAliasing { smaa: true, ..AntiAliasing::msaa4() }` | as MSAA; SMAA on top adds little | | both |
| TAA native | `AntiAliasing::taa(TaaConfig::native())` | geometric edges, shading and specular aliasing, sub-pixel detail, thin geometry | trails on things without motion vectors (§6); one frame of aliasing at every revealed pixel | +1.68 to +2.08 ms over no AA on the synthetic scenes (resolve 1.53-1.83 ms), +1.5 ms in Dreidel Derby mid-round |
| TAA Quality | `TaaConfig::preset(TaaPreset::Quality)`, scale 0.67 | as native, at 45% of the pixels shaded | 4.3% of isolated sub-render-pixel features lost in a still image; softer thin detail; opaque-path HUD soft | +0.29 to +0.76 ms against no AA (sharpening off; RCAS adds 0.17-0.20 ms); Dreidel Derby mid-round 7.30 ms against 10.74 with no AA (unpaced, §7.6) |
| TAA Balanced | `TaaPreset::Balanced`, scale 0.58 | as Quality, at 34% of the pixels | 7.3% of isolated sub-render-pixel features lost | not measured |
| TAA Performance | `TaaPreset::Performance`, scale 0.5 | as Quality, at 25% of the pixels | 14.5% of isolated sub-render-pixel features lost; softer than 0.67 | −0.36 to +0.54 ms against no AA; Dreidel Derby mid-round 6.16 ms (unpaced) |

Measured image error against the reference (§7.1, display-linear;
`taa-thin`, converged, mean absolute error in 0-255 display units, lower is
better):

| | native | 0.67 | 0.58 | 0.5 |
|---|--:|--:|--:|--:|
| TAA | 0.560 | 1.254 | 1.783 | 2.679 |
| MSAA 4x | 5.525 | | | |
| SMAA | 5.268 | | | |
| None | 6.274 | | | |

> **TODO (lead, Dreidel Derby pass B):** the Dreidel table below and the
> Dreidel numbers in the mode table and the bullets are round 1's.

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

- **TAA native** is the best still image by a wide margin (about 10x lower
  error than MSAA on `taa-thin`) and fixes the aliasing MSAA cannot reach. It trails
  on content without a correct motion vector (particles, transparents, a
  moving silhouette's first uncovered pixels); §6.
- **Quality (0.67)** is still far better than native MSAA on the gate scene
  (1.254 against 5.525) while shading 45% of the pixels. It is the preset to use
  when frame time matters.
- **Performance (0.5)** is still below native MSAA on fine detail (2.679
  against 5.525) but loses 14.5% of isolated sub-pixel features and is above
  MSAA on `taa-orbit` (3.555 against 3.184). Use it when frame time decides.
- **Balanced (0.58)** sits between the two: below native MSAA on `taa-thin`
  (1.783 against 5.525), `taa-orbit` (2.637 against 3.184) and `taa-reveal`
  (0.667 against 1.770); 7.3% of isolated sub-pixel features lost. Its frame
  cost was not measured.

### Should TAA be the default?

The facts, as measured (§7):

- **Quality against MSAA 4x.** TAA native has the lower error on `taa-thin`
  (0.559 against 5.525), in Dreidel Derby mid-round (0.873 against 1.864;
  **TODO (lead, pass B)**) and on 9 of the 10 other converged test scenes. It
  is level on `taa-skin` (0.174 against 0.173).
- **Cost against MSAA 4x.** TAA native is cheaper on two of the four synthetic
  scenes (`instancing-stress` −0.72 ms, `taa-thin` −0.45 ms) and in Dreidel
  Derby mid-round (−2.6 ms; **TODO (lead, pass B)**), and dearer on the other
  two (`kitchen-sink` +0.71 ms, `ssr` +1.03 ms). TAA 0.67 is cheaper than MSAA
  everywhere it was measured (−0.10 to −2.24 ms on the synthetic scenes,
  −7.4 ms in Dreidel Derby mid-round, unpaced).
- **Gates.** Not every verification gate is met: the trail mean on the two
  `taa-motion` frames where even the oracle is above it, the trail maximum
  (as written, and on a few frames as restated), the transparent trails as
  written, the resolve and native-frame cost targets
  (`docs/plans/taa-upscaling/05-verification.md`, Results).

The default was left at MSAA 4x for that last reason.

TAA needs no particular device feature: devices that cannot take the fifth
geometry target get motion vectors from the prep pass instead (§3.10). If a
TAA pipeline fails to compile on a device, the renderer switches to MSAA 4x
once and says why (`taa_fallback_reason`, §2.9).

### Choosing the TAA options

Each option below fixes one kind of ghost, and each costs nothing when it is
off (a level not selected compiles, allocates, copies and binds nothing; a
tuning value at 0 is a uniform branch). Ghost counts are from
`?mode=ghost-track` (`examples/taa-tests`): a pixel-frame where TAA kept
history (alpha < 0.99) and is more than 16 display units worse than both no
AA and the oracle (MSAA where it moves), counted in the trail band a moving
object leaves and in the still parts of the frame; events are such pixels
followed along their motion. Costs are repeat-N GPU time at 1920x1435 on the
M1, `kitchen-sink` / `taa-thin`. Evidence crops are in `docs/taa/` (panels:
TAA | MSAA 4x | oracle | reference | class map, red = ghost).

| Option | Default | What it fixes | Evidence | Cost when on |
|---|---|---|---|---|
| `disocclusion: Depth` | | reveals where the uncovered surface is farther than what covered it | | resolve 1.23 / 1.30 ms; previous depth 4 B/px |
| `disocclusion: DepthAndMotion` | | + the contact line under an object moving over a static surface, where the two depths touch | | +0.24 / +0.34 ms over `Depth`; previous vectors 4 B/px |
| `disocclusion: DepthMotionAndId` | **on** | + contact lines on moving surfaces and between two objects (limbs crossing, a character on a moving platform); a revealed pixel's new history from its own object only. together with the two rows below and the history-length rule (§3.5 step 7), `taa-motion` trail ghosts 300 → 24 pixel-frames and events longer than one frame 36 → 1 against round 2; `prefab-skinned-morph` / `anim-skinned` trail events longer than one frame 1 → 0, moving-object ghosts 799 → 597 / 139 → 98 against `DepthAndMotion` | `skin-legs-k18-before-after.png`, `skin-morph-k91-before-after.png` (round 2 → round 3) | +0.07 / +0.18 ms over `DepthAndMotion`; previous visibility 8 B/px and its copy |
| `motion_edge_speed` 0.5 | **on** (2.0 before) | slow silhouettes kept part of their history and ghosted for 15-45 frames | | none (a uniform) |
| `reactive_particles` 1.0 | **on** | particle sprites' soft skirts kept a stale colour for up to 32 frames | | none: the reactive value was already written |
| `reactive_lines` 1.0 | off (0.3) | the faint double behind an animated line (lines have no vectors): `taa-transparent` moving-region ghost pixel-frames 184,562 → 15,633 | `lines-k123-before-after.png` (0.3 → 1.0), `leftover-transp-k123.png` | no GPU time; still lines alias: `taa-transparent` still-image flicker 0.291 → 0.372, p99 3 → 4 (fails 05's p99 < 4), hence off by default |
| `edge_release` 1.0 | off (0) | the one stale-history event left at the default: a resting edge released by a sub-pixel move (`taa-motion` k211, 1 px for 4 frames); `taa-motion` trail ghosts 24 → 7, events longer than one frame 1 → 0 | `k211-edge-release.png` | resolve +0.07 / +0.23 ms; still-image flicker `taa-thin` 0.301 → 0.323, `instancing-stress` 0.105 → 0.151 (both still pass, p99 unchanged) |
| `max_history_moving` 1.0 | **on** (2.0 before) | softness in motion: moving-region error `taa-motion` 2.52 → 2.20, `taa-orbit` 1.20 → 1.06; frame-to-frame change of the error on the `taa-orbit` strafe 1.21 → 1.06 (MSAA 4.00); pan worst mean 1.01 → 0.86 (2.50 → 2.33 at 0.67) | `max-history-moving-orbit-k70-before-after.png` | none (a uniform); still-image flicker unchanged |

**Recommended configurations.**

| Content | AA | `disocclusion` | Tuning |
|---|---|---|---|
| Games and scenes with objects moving over other objects (the default) | TAA native | `DepthMotionAndId` | defaults |
| The same when frame time decides | TAA Quality (0.67), §2.3 | `DepthMotionAndId` | defaults |
| Camera-only motion (walkthroughs, a static world): no object moves over another, so the id test finds nothing | TAA native or Quality | `DepthAndMotion` (saves 0.07-0.18 ms) | defaults |
| Apps that animate lines (gizmos, debug draw that moves) | TAA | default | `reactive_lines: 1.0` |
| A zero-ghost requirement that accepts a little more shimmer on thin wires and small instances | TAA | `DepthMotionAndId` (required) | `edge_release: 1.0` |
| Only geometric edges matter and the content has no thin or specular detail | MSAA 4x | | |

Particle meshes an app builds itself must be flagged (§5).

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
| `disocclusion` | `DepthMotionAndId` | `TaaDisocclusion`: which tests drop a pixel's history: `Depth`, `DepthAndMotion` (adds last frame's motion vectors), `DepthMotionAndId` (adds last frame's object id). Structural: a change rebuilds the TAA pass and restarts history. A level that is off allocates and binds nothing. |
| `tuning` | `TaaTuning::default()` | The resolve's tuning, §2.4. |

`TaaConfig::native()`, `TaaConfig::preset(p)`, `matching_preset()`,
`clamped_render_scale()`, `upscales()`. `MIN_RENDER_SCALE` is 0.5.

**`TaaDisocclusion`.** Each level adds to the one before; a level that is
not selected compiles, allocates, copies and binds nothing of its own
(pinned by `taa_resolve_shader_validates` over all levels). Resolve cost by
repeat-N at 1920x1435 (`kitchen-sink` / `taa-thin`), and what each level
fixes (ghost counts as in §1, "Choosing the TAA options"):

| Level | Keeps from last frame | Resolve | Fixes |
|---|---|--:|---|
| `Depth` | depth (4 B/px) | 1.23 / 1.30 ms | reveals where the uncovered surface is farther than what covered it |
| `DepthAndMotion` | + motion vectors (4 B/px; 8 on the fallback path) | 1.47 / 1.64 ms | + the contact line under an object moving over a static surface (the two depths touch there): missed first-frame ring pixels on `taa-motion` 7,548 → 1,791 |
| `DepthMotionAndId` (default) | + the visibility target's mesh id (8 B/px) | 1.54 / 1.83 ms | + the contact line on a moving surface and wherever two objects touch in depth; a revealed pixel's new history starts from its own object only (§3.5). `taa-motion` trail-band ghost pixel-frames 300 → 24, events longer than one frame 36 → 1 (with §2.4's `motion_edge_speed`) |

Recommended: `DepthMotionAndId` everywhere. The cheaper levels are for a
device or scene where 0.1-0.2 ms matters more than contact-line ghosts (a
camera-only scene gains nothing from the id test; one with objects moving
over others does).

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
| `max_history_moving` | 1.0 | History cap for a pixel moving at `max_history_moving_speed` or faster (2.0 before round 3; 1.0 is closer to the reference in motion and steadier frame to frame, §1 "Choosing the TAA options") | Smoother motion, softer, slower to sharpen after stopping | Sharpens again faster after motion stops |
| `max_history_moving_speed` | 1.0 | The speed at which the cap reaches `max_history_moving` | | |
| `motion_edge_alpha` | 1.0 | Minimum current-frame weight on a motion boundary (3x3 with surfaces moving differently) | | Smoother moving silhouettes, more trail on them |
| `motion_edge_speed` | 0.5 | The relative speed at which `motion_edge_alpha` is reached (2.0 before round 3: slow silhouettes kept part of their history and ghosted for 15-45 frames) | Slow silhouettes keep more history (ghosting risk) | Even slow edges show only this frame |
| `reactive_transparent` | 0.3 | Reactive value per unit of alpha written by transparent surfaces (glass, water; not particles or lines) | Fewer trails behind transparents, more shimmer on still glass and water | More trail, steadier glass |
| `reactive_particles` | 1.0 | Reactive value on every pixel a particle mesh covers, whatever the sprite's alpha (meshes flagged by `set_mesh_taa_particles`; the scene loader flags every emitter). Nothing reprojects a particle (§3.6) | | Particles and their soft skirts keep some history (trails behind them) |
| `reactive_lines` | 0.3 | Reactive value per unit of alpha written by lines (§3.6). Lines have no motion vectors | An app that animates lines raises it to 1.0: no faint double behind a moving line, but aliased line edges | Steadier still lines, longer double behind moving ones |
| `edge_release` | 0.0 (off) | `DepthMotionAndId` only: the current-frame weight of a pixel whose history came from a 2x2 that held an object now absent from its 3x3 (§3.5 step 5, "resting-edge release") | 1.0 removes the last measured trail ghost (a 1-px, 4-frame event on `taa-motion`), but raises still-image flicker where a thin wire or small instance was hit by one jitter phase's sample and by no current one (§6) | |
| `reactive_material` | 0.2 | Reactive value of materials resolved `Reactive` (§2.6) | Less smearing of animated colour, more aliasing on those surfaces | More smearing |
| `reactive_ssr` | 0.0 | Extra reactivity from SSR reflectivity | For mirrors that trail under camera motion | |
| `mip_bias_extra` | 0.0 | Added to `log2(render_scale)` for the texture LOD bias | Blurrier textures | Sharper textures, more shimmer (−0.5 raised flicker p99 from 4 to 6 at 0.67) |

Two values are constants, not tuning: the clip weight (`CLIP_ALPHA`, 1.0 per
unit of clip amount) and the reconstruction kernel's exponent
(`RECONSTRUCTION_SHARPNESS`, 2.29 per output pixel squared), which the
reference capture shares. Below scale 1 the resolve scales it to render
pixels (§3.7).

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
- **Kept**: the TAA history (output size; the previous depth and velocity
  are carried across), bloom and the effects targets, LOD and coverage LOD (measured in
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

The views live in a separate variant of the resolve, so the production
resolve carries no view code (the view branch cost about 0.12 ms at
1920x1435 even when no view was selected). The variant compiles the first
time a view is selected; until it is ready, or if it fails to compile, the
normal image shows. `taa_debug_variant_ready()` is true once it is in use (and
whenever no view is selected or TAA is off); a test that reads a view waits
for it. `FreezeJitter` needs no variant.

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

Hidden harness hooks: `set_taa_debug_view_raw` (internal views: 10 previous
depth against expected, 11 history length before this frame, 12 current
weight `alpha`, 13 motion-boundary factor, 14 R dilated / G test 1 / B
test 2, 15 R `alpha` ≥ 0.99 / G motion-boundary term / B clip term, 16 test 2's
parts R depth / G MIXED / B COVERED; they use the debug variant, §2.8),
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
  `render_scale`, `sharpen`, `disocclusion` (`"depth"` |
  `"depth_and_motion"` | `"depth_motion_and_id"`, the renderer's
  `TaaDisocclusion`), `tuning` with every `TaaTuning` field; every field
  defaults to the shipped value, so a file may carry only what it changes).
  With `taa` present the loader ignores `msaa` / `smaa`.
- `load_scene_for_player` applies the scene's anti-aliasing before its commit
  and replaces the renderer's mode. A scene with no AA block applies MSAA 4x,
  as in 0.45. An authored TAA that cannot be set up falls back to MSAA 4x with
  a warning.
- Editor: the AA mode, preset, sharpening, disocclusion test and tuning
  knobs are in the scene settings, saved with the project and bundle; debug
  views are transient. Changing the disocclusion test while TAA is on
  rebuilds the TAA pass and restarts history, like a mode change.
- MCP: `set_anti_aliasing` (`mode: none | msaa | smaa | msaa_smaa | taa`,
  `taa_preset`, `taa_render_scale`, `taa_sharpen`, `taa_disocclusion`
  (structural while TAA is on: `wait_render_settled` after), one flat
  `taa_<name>` param per tuning field, `taa_debug_view`); read back with
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
- The previous depth (a depth-format copy, bound as `texture_depth_2d`) and
  the previous velocity (a copy of the velocity target, same format) are
  copied right after the resolve, their only reader. On the fallback path
  that is after the velocity overlay (§3.10), so both paths keep the same
  vectors. The pair has one size; it is reallocated together on a render-size
  or velocity-format change and keeps last frame's data until the first copy
  at the new size.
- The TAA pass keeps the last two unjittered view-projections for the
  resolve's motion test (§3.5, step 5).
- Which of these previous-frame targets exist follows the disocclusion level
  (§2.2): depth always; velocity from `DepthAndMotion`; at
  `DepthMotionAndId` also a copy of the visibility target (`rgba16uint`,
  whose BA holds the per-pixel mesh id), with a copy-source flag on the
  visibility target at that level only.
- On the 32-byte fallback all of it is kept when the velocity overlay exists
  (§3.10); without the overlay only the transforms are.
- With TAA off none of this exists and nothing is copied.

### 3.5 The resolve

One compute pass at post size (8x8 workgroups, one invocation per output
pixel), after the SSR composite and before bloom, effects and display.
Inputs: the render-size composite (lit HDR), depth, velocity, reactive mask,
last frame's depth and velocity (§3.4), last frame's history, the camera
uniform. Outputs: this frame's history (`rgba16float`, post size, `.a` =
history length `n`) and `taa_out` (linear HDR, post size). Every 3x3 is
unrolled at template time; no `switch`, no per-invocation arrays (the Apple
GPU rules).

**Two colour spaces (G11).** Everything that averages (the reconstruction,
the clip box, the blend) works in the display's own space, `W =
D(exposure · c)`, where `D` is the curve the display pass applies:

- Khronos PBR Neutral, exactly as the display applies it, with its exact
  inverse;
- ACES (Narkowicz) without its final clamp to 1: the rational is monotonic,
  so it inverts, and highlights above white stay distinct for bloom;
- with `ToneMapping::None`, the Karis map `c / (1 + max(c))` of the exposed
  colour (the display's clip at white has no inverse).

The tonemapper is a template axis of the resolve: changing it with TAA on
rebuilds only the resolve pipeline and keeps the history. Exposure is a
uniform.

The history is **stored** in the Karis map of unexposed linear HDR, as
before: it does not depend on exposure or the tonemapper, and keeps `f16`
precision for highlights. It is fetched, un-mapped and moved to `W`; the
result is moved back, Karis-mapped, rounded to `f16` (`taa_round_f16`; a
truncating store would darken the history every frame) and stored.

Why: tonemapping after averaging scene-linear colour (or Karis-weighted
colour) is not what the eye sees, and every high-contrast edge converges to
a biased colour. Far from any motion, the converged error was 0.570 with
Karis weights against the oracle's 0.095, and is 0.145 in the display's
space.

For output pixel `q`:

1. **Where it lands.** `r_pos = (q + 0.5) / O · R`, `p0 = floor(r_pos)`.
2. **Current colour, from a tile.** The workgroup first loads the 10x10
   render texels its 64 pixels' 3x3s touch into workgroup memory (`W` colour
   as YCoCg and depth, plus the reactive value in the reactive variant), in
   two rounds, followed by a barrier that every invocation reaches before
   out-of-bounds pixels return. Each texel is read and converted once,
   instead of up to nine times. For the 3x3 around `p0`, the weight is
   `exp(−k · d²)`, where `d` is the distance from `r_pos` to where the sample
   really is (`p + 0.5 − j`). The kernel is separable, so 6 `exp` per pixel.
   `k = 2.29 · (O / R)²` (§3.7). The pass accumulates the weighted colour
   (YCoCg in `W`) and the unweighted YCoCg moments. A tap outside the image
   takes no part in either: clamped to the edge it would count the edge
   sample twice, narrow the box and make the border row flicker. `confidence` = the
   largest weight. Opaque-path HUD pixels (§3.8) take no part.
3. **Dilated vector (G3).** The vector of the nearest-depth pixel of the
   3x3. The vector of the farthest is kept too, for the motion-boundary
   term. With sky everywhere in the 3x3, the vector is the camera's rotation.
4. **History fetch (G4).** `uv_hist = uv_q − v`. Off screen: reset this
   pixel. Otherwise a 5-tap Catmull-Rom fetch (clamped to ≥ 0), `n` from the
   nearest texel.
5. **Disocclusion (G6).** Both tests compare two previous-frame distances:
   where the surface point was last frame (through last frame's
   view-projection) against what last frame's depth buffer holds where its
   history is fetched, so a fast dolly does not trip them. A test fires only
   when something nearer was there. Errors fall on the safe side: a false
   reveal costs one frame of aliasing, a missed one a ghost.
   - **Test 1**, the surface whose vector the pixel uses: the nearest depth
     of the previous 2x2 footprint against its expected distance, relative
     slack `disocclusion_threshold`.
   - **Test 2**, the pixel's **own** surface, with its own depth and vector.
     Test 1 alone has two blind spots on the strip a moving object has just
     left. A pixel beside the object takes the object's vector, so test 1
     compares the object with itself. And an object resting on the surface
     behind it is within the slack of that surface near the contact line.
     For a surface that did not move (its vector is its static reprojection,
     within 0.1 px), the expected distance is exact, and the slack shrinks to
     twice the surface's depth slope across a pixel, plus 0.002.
     - *Beside a nearer surface that moves relative to this one* (dilated,
       vectors differ by more than 0.25 px): the nearest depth of the own
       footprint.
     - *Not dilated*: the farthest depth of the footprint. Was this surface
       visible anywhere in it? A static edge or a wire that the jitter moves
       across the footprint is not a reveal.
     - *Dilated without relative motion* (beside a static nearer surface, or
       on a slope): no depth test. The jitter sweeps such edges across the
       footprint every cycle, and resetting there makes thin geometry
       shimmer.
     - **Motion test**, for a static own surface where depth did not fire:
       last frame's vectors (§3.4) at the nearest and the farthest texel of
       the footprint. It flags *MIXED* when the two differ by more than
       `max(0.25 px, 25%` of the pixel's own speed`)`: two surfaces moving
       differently shared the footprint, so its history is a blend of them.
       It flags *COVERED* when every texel was nearer than this surface and
       the farthest one moved unlike this surface did last frame. That
       motion is the surface's static motion through the last two unjittered
       view-projections; comparing against this frame's motion instead
       fires on the whole screen when the camera starts or turns. Sky
       texels, and texels without a vector (cleared to 0, or the `(2, 2)`
       "behind last camera" marker), take no part. The motion test is what
       catches the contact line under a moving object, where depth cannot
       tell the two surfaces apart: on `taa-motion`, the missed first-frame
       ring pixels drop from 7,548 to 1,791. Under camera motion test 2
       fires on 1-px lines along parallax silhouettes, about half of them by
       motion (about 1,100 pixels a frame in all on the `taa-orbit` strafe at
       960x720). In a still scene it fires nowhere.
     - **Object-id test** (`DepthMotionAndId`), where neither fired: last
       frame's mesh id at the texel whose sample sat nearest the point
       (`floor(uv · size + previous jitter)`). It fires when that id is not
       this pixel's and the object there moved unlike this surface did last
       frame (the motion test's rule: a static edge or a thin wire that the
       jitter moves across the point has no relative motion and keeps its
       history). It decides the contact line on a moving surface too, and
       wherever two objects touch in depth. Not the 2x2 around the point: an
       edge lying on a texel boundary always finds the object in the other
       row. One mesh occluding itself, and instances of one instanced mesh
       (they share the mesh id), are left to the depth and motion tests: a
       contact between two instances of one instanced mesh is decided by
       depth and motion only, as at `DepthAndMotion`.
     - **Resting-edge release** (`edge_release`, off by default): a pixel
       whose history came from a 2x2 that held an object now absent from
       the current 3x3 takes at least `edge_release` of this frame. It
       catches the one case the tests above cannot see: an edge that rested
       over a pixel (so its history converged to the partial coverage, as
       the reference's does) and is then moved off by less than a pixel, so
       that no surface there shows relative motion. At one sample per pixel
       this is indistinguishable from a thin wire that one jitter phase's
       sample hit and no current sample hits, which is why it costs
       still-image flicker (§2.4, §6).
   - At `DepthMotionAndId` a pixel flagged here starts its NEW history from
     its own object only: the 3x3 reconstruction restricted to taps with the
     centre's mesh id is what is stored, so the object that just left does
     not live on in the history through the kernel. The displayed colour
     keeps the full reconstruction, like the reference (showing the
     own-object colour too made revealed edges aliased: class (b) +6k to
     +15k pixel-frames).
6. **Variance clip (G5).** `mean ± γ·σ` of the plain current 3x3 in YCoCg
   of `W`; the history is clipped along the line to the mean (never a
   per-channel clamp). `γ` goes from `gamma_still` to `gamma_moving` with
   speed. `clip_amount` = how much of the history was cut. The clip is
   unconditional; a test pins it.
   The box has a floor: `max(1.5e-3, three f16 steps of the stored history at
   the box mean, measured in W)`. The history is quantised in the stored
   space, and without the floor a flat area reads its own rounding as a
   clipped history and forgets it. Under Khronos at exposure 1, the
   three-step term is above 1.5e-3 for grey between HDR ~0.6 and ~0.85 (up to
   2.1e-3, just below the curve's shoulder) and on bright saturated colours
   (up to ~1e-2). Elsewhere it is below 1.5e-3.
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
   result = mix(history_clipped, current, alpha)               // in W
   n_next = min(n + 1, 1 / alpha_before_clip, cap) · (1 − clip_amount)  // 1 on reset
   ```

   `n_next` never exceeds what the boosts (motion boundary, speed, reactive)
   leave of the history: a pixel whose weight was raised to ~1 holds about
   one frame of history and must not be trusted as a 16-frame one the frame
   the boost ends. It equals `n + 1` whenever no boost applies.

   The motion boundary is a 3x3 whose nearest and farthest surfaces move
   differently: a moving silhouette's coverage changes every frame, and its
   history is the average coverage of the last frames, not this one's. The
   speed-dependent cap exists because history resampled every frame softens
   a little each time; with a 16-frame memory, 60% of that softness was still
   there 8 frames after the camera stopped.
8. **Outputs.** `taa_out = W⁻¹(result)`; history = (the Karis map of
   `taa_out`, rounded to `f16`, `n_next`). A non-finite result (NaN, or more
   than 0.1% above the curve's maximum) writes the current colour with
   `n = 0`.

Reset frames (§2.7) write the current colour with `n = 1` everywhere.

### 3.6 Reactive mask

`reactive` (`r8unorm`, render size) says how much to distrust history: 0 =
trust, 1 = ignore. The resolve takes the max over the same 3x3.

- **Transparent surfaces** write `alpha · reactive_transparent` as a second
  target of the transparent pass, blended with max; **lines** write
  `alpha · reactive_lines` (from the line uniform's fourth word, 0 without
  TAA).
- **Particles** (meshes flagged `Mesh::taa_particles`, which the scene
  loader sets on every emitter; `AwsmRenderer::set_mesh_taa_particles` for
  meshes an app builds itself) write `reactive_particles` on every covered
  pixel (alpha > 1/255), not scaled by alpha: nothing reprojects a particle,
  and a soft sprite's low-alpha skirt kept a stale colour for up to 32
  frames at `alpha · 0.3`. The flag is bit 1 of mesh-meta word 22; the
  weight rides in the camera uniform's `position.w` (0 without TAA).
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
  The reconstruction kernel is the reference's, `exp(−2.29 · d²)` with `d` in
  output pixels, so in render pixels it is narrower below scale 1:
  `k = 2.29 · (O / R)²`. A kernel one render pixel wide blurred the upscaled
  image by 1 / scale: converged `taa-thin` read 2.68 at 0.67 and 5.46 at 0.5
  with it, against 1.24 and 2.67 now (MSAA at native: 5.53).
- **History cap** = `max_history_native` × (output pixels / render pixels), up
  to `max_history_upscaling`: 35.6 frames at 0.67, 64 at 0.5. The history
  then holds about as many render samples per output pixel as at native. A
  fixed cap of 24 left fine detail of an upscaled still image flickering.
- **Still clip box** widens by 0.8 per unit of (output / render − 1): 1.25
  native, 1.74 at 0.67, 2.25 at 0.5. A feature smaller than a render pixel is
  seen only by some jitter phases, and the native box clipped it out on the
  frames that missed it: at 0.67 one isolated feature in six disappeared from a
  still image, one in twenty with the wider box (render-pixel kernel). With
  both the wider box and the output-pixel kernel: 4.3% at 0.67, 7.3% at 0.58,
  14.5% at 0.5. The cost is about 12% more error on the strip a moving object
  leaves.
- **Confidence**: with no render sample near the output pixel this frame, the
  current weight is scaled down (`mix(0.3, 1, confidence)`). With the
  render-pixel kernel it was near 1 on 95% of the image and barely
  modulated. The output-pixel kernel is narrower, so `confidence` is lower and
  does modulate: on converged `taa-thin` the Confidence view reads a mean of
  171 and a 5th percentile of 97 (display-encoded bytes) at 0.67, 142 and 109
  at 0.5, against 210 / 170 and 218 / 207 in round 1.
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
2.76 M pixels): velocity 4 B/px, reactive 1 B/px, previous depth 4 B/px,
previous velocity 4 B/px (from `DepthAndMotion`), previous visibility
8 B/px (`DepthMotionAndId`) at render size; two history textures and `taa_out` at 8 B/px each at post size;
previous-state copies at the size of the current buffers (112 B per
transform). On the fallback path the velocity texture is 8 B/px; its
velocity overlay allocates nothing of its own (bind groups and pipelines
only) and exists only on that path. The previous velocity follows the
velocity format (8 B/px on the fallback path).

## 4. The ghosting rules

Ghosting is history that should have been thrown away and wasn't. The
earlier forward-renderer TAA (removed in `c42b6805`) broke most of these.

| # | Cause | Rule | Where enforced | Pinned by |
|---|---|---|---|---|
| G1 | Wrong motion vectors | A vector is the same surface point's screen motion, including camera, object, instance, skin, morph and vertex-hook motion | geometry pass second vertex evaluation, `prev_state` | device: analytic vectors within 0.02 px and supersampled reprojection error per scene; `taa_prev_functions_mirror_current`, `taa_motion_vector_divides_per_fragment`; motion-history byte-exact self-check |
| G2 | Jitter in the vectors | Unjittered matrices on both ends; jitter removed where the current frame is sampled | camera uniform, resolve step 2 | `taa_resolve_shader_validates` (sample at `p + 0.5 − j`); jitter unit tests in `taa.rs`; device G2 vector rows |
| G3 | Edge pixels with background velocity | Dilate: nearest-depth vector of the 3x3 (opaque-path HUD pixels excluded) | resolve step 3 | `taa_resolve_shader_validates` (depth convention, the tile's depth); trail tests |
| G4 | Blurry history accumulates | 5-tap Catmull-Rom, never plain bilinear | resolve step 4 | `taa_resolve_shader_validates` (5 taps); stop and pan tests |
| G5 | Clip box too big | Variance clip in YCoCg of the working-space colour, clip to mean, unconditional; floor at the stored history's quantisation | resolve step 6 | `taa_resolve_shader_validates` (clip lines and the floor at top level of `cs_main`); flicker and trail tests |
| G6 | Revealed background keeps old foreground | Depth disocclusion test; the own-surface test by depth, by last frame's motion where depth cannot tell (contact lines), and by last frame's object id (`DepthMotionAndId`); a revealed pixel's new history from its own object only | resolve step 5, previous velocity and visibility copies (§3.4) | `?mode=ghost-track` (`ghost-persist` lines), trail tests (oracle and ghost lines), static false-flag counts (§7.2), `?mode=flag-count`; `taa_resolve_shader_validates` per level |
| G7 | Memory too long | `1/(n+1)` with a cap (16 native, scaled when upscaling, shortened with speed) | resolve step 7 | stop, trail and flicker tests; `history_cap` unit tests |
| G8 | Things with no correct vector | Reactive mask: transparents, particles (full weight on every covered pixel), lines, reactive materials, HUD | transparent pass target, seed draw | `taa_reactive_transparent_and_line_shaders_validate`, `taa_reactive_seed_validates_and_reads_the_right_meta_words`; phase-3 tests |
| G9 | History reused across a cut | Resets on cuts, loads, size and AA changes, camera-jump guard | `reset_temporal_history`, renderer-side resets | cut tests (flagged and unflagged); `jump_guard` unit test |
| G10 | Objects appearing or teleporting | No valid previous value means prev = current | frame-start fix-ups, `reset_motion_history` | history burst / density / reset device tests; `fresh_ranges_*` unit tests |
| G11 | Averaging in the wrong space | Reconstruct, clip and blend in the display's own space (the tonemapper's curve after exposure); store the history in the Karis map | resolve steps 2, 4, 6-8 | `taa_resolve_shader_validates` (the tile and the history converted with `to_work` before the moments; the curve per tonemapper), `taa_working_space_curves_invert` |
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
8. **Flag particle meshes you build yourself.** The scene loader flags every
   emitter. A particle or sprite mesh an app builds and moves itself (an
   instanced billboard updated every frame, not through the loader) has no
   motion vectors either: call `set_mesh_taa_particles(mesh, true)` once, or
   it trails like any transparent surface.

## 6. Known limits

- **The first frame a pixel is uncovered.** No history method can know its
  colour. On `taa-motion` the trail band is made only of such pixels on two
  frames (k31 and k211). There even an oracle with perfect disocclusion
  knowledge reads a band mean of 3.81 and 2.17, so 05's "mean < 2 at every
  frame" fails there for any 1-sample-per-pixel temporal method. TAA reads
  3.79 at k31. Every other frame of `taa-motion`, and every frame of
  `taa-reveal` and `taa-skin`, is below 2 (§7.2).
- **The maximum on a moving silhouette.** Within 1.5 px of a moving
  silhouette the pixel shows mostly this frame (the motion-boundary weight),
  whose 3x3 still holds samples of the object one pixel away, so single
  frames there can exceed MSAA's error. Those pixels show this frame, not
  history: `?mode=ghost-track` counts them as current-frame (class b), not as
  ghosts. 05's original "max < 16" is not reachable by any one-sample method:
  the oracle reads 18-39 on first-frame pixels. On the final head the band
  maximum is above max(MSAA, no AA) on 11 / 4 / 37 frames of `taa-motion` /
  `taa-reveal` / `taa-skin`, and TAA alone exceeds it on 4 / 2 / 3 of them
  (round 2: 9 / 2 / 5; §7.2).
- **What is left of ghosting** at the default (`DepthMotionAndId`), counted
  by `?mode=ghost-track` at 960x720 (events = a ghost pixel followed along
  its motion):
  - `taa-motion`: 24 trail-band ghost pixel-frames, one event longer than a
    frame: k211, a cube edge that rested over a pixel (whose history
    converged to the partial coverage, as the reference's does) and then
    moved off by less than a pixel, 1 px for 4 frames. No test can see it:
    nothing there shows relative motion. `edge_release: 1.0` removes it
    (§2.4), at a still-image flicker cost, and is off by default.
  - `taa-skin`, `taa-reveal`, `prefab-skinned-morph`, `anim-skinned`,
    `particles`: no trail or still-area event longer than one frame.
  - `taa-transparent`, `particles`: no trail or still-area event longer
    than one frame. (With the classifier's earlier "this frame only" cut of
    alpha ≥ 0.99, `taa-transparent` showed two 2-frame trail events on
    particle sprite edges, `docs/taa/transp-trail-k101.png`: a particle at
    `reactive_particles` 1.0 reads 252/255 = 0.988 in the `r8unorm` mask and
    was counted as kept history although it shows this frame's one jittered
    sample, the backdrop where the reference shows part of a sprite and the
    reverse, never the sprite where it was. The cut is 0.985 since; §7.2.)
  - `taa-thin` (a still-image flicker stress scene with moving parts): 3
    trail events of two frames, on the 1-2 px light rim of the moving red
    block (`docs/taa/thin-trail-k212.png`). The pixels are dilated and blend
    history fetched through a neighbour that moves with them (no relative
    motion), so neither the disocclusion tests nor the clip can tell it from
    correct history; TAA places the rim about half a pixel off for two
    frames, never redder than the reference behind the block. Fetching
    through the pixel's own vector instead removed two of the three but
    doubled `taa-thin`'s still-image flicker (0.300 → 0.587, p99 12);
    restricted to relative motion it was flicker-neutral but no longer
    touched them. They stay, as a thin-moving-feature limit like a wire's. Still-area events of 2-4 frames that repeat at the same pixels
    with the 8-frame jitter cycle (sub-pixel wires whose history the clip
    cuts on some jitter phases; `docs/taa/thin-static-flicker.png`). That is
    still-image shimmer, measured by the flicker gate (0.300, passes), not
    history of something that moved; `ghost-track` reports it as INFO. Its
    still-area error is TAA 0.514, MSAA 5.008, no AA 5.667.
  - On `taa-orbit` no object exists, so its trail band (the pixels whose
    motion falls below 0.05 px: near the pivot, and after the stop) holds no
    trail: its 2-4 frame events are single pixels on floor-checker edges
    softening under camera motion. It is judged by the moving-region error:
    TAA 1.20, MSAA 4.12, no AA 4.10.
- **Contacts the id test cannot separate.** One mesh occluding itself (a
  limb over the body of the same mesh) and two instances of one instanced
  mesh share a mesh id; there the depth and motion tests decide alone, as at
  `DepthAndMotion`.
- **Transparent and line trails.** Glass, UV-flow water and lines over a
  moving background trail for every AA mode, TAA included (§7.2). Their
  reactive weights (0.3) trade some of that for still-image stability. On
  `taa-transparent` (round 3, native, display-linear) the backdrop band under
  the content averages 2.96 (worst frame 7.67) against MSAA's 4.71 (8.44), and
  the content's own trail 0.81 (worst 1.88) against MSAA's 3.20 (5.64): both
  within the restated bound, not within 05's bound as written. An
  animated line leaves a faint double behind it (`docs/taa/leftover-transp-k123.png`);
  apps that animate lines set `reactive_lines: 1.0`. Particles flagged as such
  (§3.6, §5) keep no history at all (`reactive_particles` 1.0). At 0.67 the
  transparent trail lines are INFO and above MSAA and no AA (band under the
  content 26.6 at the worst frame, the content's own trail 11.5), because
  the reactive pixels show this frame's render-resolution image.
- **Upscaled thin detail.** Isolated features smaller than a render pixel
  still drop out of a still image sometimes: 4.3% at 0.67, 7.3% at 0.58,
  14.5% at 0.5.
- **Opaque-path HUD below scale 1** is rasterised at render size and soft
  (§3.8): at 0.67 its pixels differ from a TAA-off render by 0.829 on average
  (worst frame 0.984, max 132, just under the < 1 bound), against 0.018 at
  native. Transparent-pipeline
  HUD meshes are sharp.
- **Hook-drawn geometry** writes no vectors and no reactive value (§5).
- **A render-scale step** restarts the volumetric froxel history and SSR's
  temporal accumulation (about ten frames to re-converge). It costs +0.1 to
  +0.6 ms of CPU on the step frame (render targets, bind groups) and a few ms
  of GPU on that frame and the next (§7.6).
- **Time-reading materials are always reactive.** `Auto` marks any custom
  material whose WGSL reads `frame_globals` as reactive, even when the time
  dependence is a subtle twinkle. Those surfaces keep less history and look
  more aliased, most visibly at upscaled scales on high-contrast silhouettes.
  In Dreidel Derby every dreidel body is such a material, and the bodies hold
  most of TAA's remaining mid-round error (§7.7). Remedy: `temporal: stable`
  on materials whose time dependence is subtle. **TODO (lead, pass B):**
  re-check the Dreidel Derby claim against the final §7.7.
- **The resolve costs 1.54 ms** (1.83 ms on `taa-thin`) at 1920x1435
  output with the default `DepthMotionAndId`; 1.47 / 1.64 ms at
  `DepthAndMotion`, 1.23 / 1.30 ms at `Depth` (§1, "Choosing the TAA
  options"). The plan targeted 0.6 ms; the bar it was replaced by is "cheaper
  than MSAA 4x in Dreidel Derby" (§7.6). Where the `DepthAndMotion` time goes,
  measured by removing each part in round 2 (`kitchen-sink`, `taa-thin`):
  disocclusion 0.31-0.42 ms (two static reprojections, up to 8 footprint
  loads, about 10 depth linearisations, the motion test about 0.06);
  Catmull-Rom history 0.19; the clip-box floor's curve and inverse
  0.07-0.09; the display curve elsewhere 0.01-0.03; the rest (tile loads, 3x3,
  clip, blend, two stores) about 0.6. The id level adds the id test's loads,
  the own-object second sum over the 3x3, the tile's visibility loads and the
  8 B/px copy.
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
stated. Unless a row says otherwise, every number is from the round-2
renderer `34b6dc92` (the resolve of §3.5) with the shipped tuning, harness
`e82b11e7`, Chrome 158 at DPR 2. The display-linear reference decides (§7.1); the Karis-weighted one is shown
beside it. §7.3's overlay table and §7.5's sweeps were measured on the
round-1 resolve and are kept for the decisions they support.

### 7.1 The reference, and which space decides

A reference frame for frame `k`: freeze time at `k`, render 64 jittered frames
with TAA off (64 Halton phases) and average them with the resolve's kernel.
That is what a perfect TAA converges to at that instant, for any scene and
any frame. At scale below 1 the reference is at full resolution.

Two averaging spaces were tried. Averaging in display-linear space, the
converged TAA image at the `taa-orbit` stop view sits at 1.185 from the
reference: the stop test (< 1) could not pass against it even with a perfect
history. Averaging with the resolve's own Karis weights (tonemapped space)
puts the same image at 0.819. Round 1 let the Karis-space reference decide
("what a perfect TAA converges to with the resolve's weights"). The gap was
the resolve's, not the reference's: averaging Karis-weighted colour biases
high-contrast edges. Since round 2 the resolve averages in the display's
space (§3.5), and the `taa-orbit` stop view reads 0.344 against the
display-linear reference. **Decision (round 2):** the display-linear
reference decides, which is 05's literal reading and now also the resolve's
own space; the Karis-weighted result is reported next to it.

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

Final head (renderer `ec35a6fd`), 960x720, every frame, 64-phase references;
the full record with every log line is the round-3 gate record (the team's
report, `gates-record.md`; round 2's beside it).

| Gate | Scene | Threshold | Measured (linear, decides) | Karis | Verdict |
|---|---|---|---|---|---|
| Vectors, analytic and reprojection | every scene, both paths | \|v − v_exact\| ≤ 0.02 px; mean < 2, p99 < 12 | 120/120 lines on each path | | PASS |
| Motion-history self-check | 48 scenes, 120 frames | 0 mismatching bytes | 0 | | PASS |
| History burst / density / reset | `taa-motion`, `prefab-skinned-morph` | clean self-check, exact vectors | 3/3 | | PASS |
| Trail mean | `taa-motion` | band mean < 2, every frame | 2 of 208 frames: k31 3.79, k211 2.39, where the oracle reads 3.81 and 2.17 (the band is all first-frame pixels) | 5 frames | FAIL (oracle floor) |
| Trail mean | `taa-reveal` / `taa-skin` | same | worst 1.750 / 1.384 | 2.612 / 1.778 | PASS / PASS (Karis FAIL / PASS) |
| Trail max, as written | all three | max < 16 | 89 / 74 / 50 | 76 / 69 / 47 | FAIL (oracle 18-39) |
| Trail max, restated | `taa-motion` / `taa-reveal` / `taa-skin` | ≤ max(MSAA, no AA) on the same frame | over on 11 / 4 / 37 frames; TAA alone 4 / 2 / 3 (round 2: 9 / 2 / 5) | TAA alone 11 / 2 / 0 | FAIL |
| Ghost persistence (round 3) | 9 scenes | no ghost (a) event longer than one frame in the trail band or static areas | see "Ghost classification" below | | see below |
| Stop | `taa-motion` / `taa-orbit` | mean < 1, 8 frames after stopping | 0.187 / 0.296 | 0.674 / 0.970 | PASS (Karis too; round 2 `taa-orbit` Karis 1.006) |
| Cut, flagged / unflagged (guard) | `taa-cut` k120 / k240 | first frame vs TAA-off < 3 | 2.620 / 2.183 | same | PASS |
| Pan | `taa-orbit` strafe | every frame < 3 | worst 0.858 | 1.234 | PASS |
| Converged | `taa-thin` | TAA < MSAA 4x | 0.560 (MSAA 5.525) | 1.839 | PASS |
| Flicker | 17 scenes | mean f2f < 0.5, p99 < 4 | worst `taa-thin` 0.300 p99 2, `taa-transparent` 0.291 p99 3 | | PASS 17/17 |
| HUD | `taa-hud` | HUD px vs TAA-off < 1 | worst 0.018, max 1 | 0.018 | PASS |
| Transparent trail, band ∩ content | `taa-transparent` | mean < 2, max < 16 | TAA worst 7.67 (avg 2.96); MSAA 8.44 (avg 4.71); no AA 16.78 | 8.57 | FAIL (every mode) |
| Transparent trail, restated | same | average ≤ MSAA's, worst ≤ no AA's | 2.96 ≤ 4.71, 7.67 ≤ 16.78 (round 2: 4.66, 11.85) | 4.18 ≤ 6.22, 8.57 ≤ 18.06 | PASS |
| Transparent, content's own trail | `taa-transparent` | mean < 2, max < 16 | TAA worst 1.88 (avg 0.81); MSAA 5.64 (avg 3.20) | 2.63 | FAIL (every mode) |
| Content's own trail, restated | same | as above | 0.81 ≤ 3.20, 1.88 ≤ 5.69 (round 2: 1.01, 3.84) | 1.34 ≤ 3.09, 2.63 ≤ 5.41 | PASS |
| TAA-off baseline | 6 scenes × MSAA/SMAA/none × 2 frames | bit-exact to 0.45 | 36/36, max diff 0 (against a set recorded from a build itself 36/36 against 0.45) | | PASS |

Converged image on the other scenes (INFO, not gate scenes; TAA / MSAA,
linear): `taa-motion` 0.225 / 2.051, `taa-orbit` 0.839 / 3.184, `taa-reveal`
0.209 / 1.770, `taa-transparent` 0.511 / 2.867, `taa-hud` 0.799 / 2.584,
`taa-cut` 0.216 / 2.847, `taa-vertex-hook` 0.305 / 1.567, `taa-cluster`
0.373 / 1.071, `instancing-stress` 0.759 / 0.948, **`taa-skin` 0.174 / 0.173
(level)**.

Static false disocclusions (30 still frames, Rejection view): no pixel is
flagged on all 30 frames in any of 15 scenes; the once-per-cycle burst
remains (e.g. `taa-motion` max 1 px, `taa-orbit` 423, `taa-thin` 1008 on the
burst frame).

**Ghost classification** (`?mode=ghost-track`, every pixel of every frame;
(a) = history kept and more than 16 worse than both no AA and the oracle, or
MSAA where it moves; events follow a pixel along its motion). Events longer
than one frame / all events (longest), and `ghost-region`: TAA's mean error
over the region against MSAA 4x's and the oracle's, at the default
`DepthMotionAndId`. "History kept" is alpha < 0.985 (raw view 15): every
reactive value is capped at 0.99 because 1.0 marks a HUD pixel, so a fully
reactive pixel stores 252/255 = 0.988 in the `r8unorm` mask and shows this
frame only. The cut was 0.99 until the final record, which gives both:

| Scene | Trail band | Static areas | Region error, trail band: TAA / MSAA / no AA | Static: TAA / MSAA / no AA |
|---|---|---|---|---|
| `taa-motion` | 1 / 20 (4): k211, §6 | 0 / 0 | 0.529 / 3.771 / 3.762 | 0.163 / 1.876 / 1.876 |
| `taa-reveal` | 0 / 2 | 0 / 0 | 0.403 / 4.263 / 4.318 | 0.163 / 1.888 / 1.903 |
| `taa-skin` | 0 / 5 | 0 / 4 | 0.938 / 0.813 / 0.388 | 0.046 / 0.022 / 0.017 |
| `prefab-skinned-morph` | 0 / 2 | 0 / 0 | 0.704 / 0.913 / 1.525 | 0.030 / 0.007 / 0.011 |
| `anim-skinned` | 0 / 2 | 0 / 0 | 0.992 / 1.006 / 2.020 | 0.030 / 0.008 / 0.014 |
| `particles` | 0 / 0 (0.99 cut: 0 / 6) | 0 / 0 (0 / 34) | 0.892 / 0.795 / 1.790 | 0.030 / 0.008 / 0.014 |
| `taa-transparent` | 0 / 5 (0.99 cut: 2 / 109, particle edges, §6) | 0 / 65 (0 / 305) | 0.437 / 0.653 / 0.981 | 0.273 / 2.364 / 2.476 |
| `taa-thin` | 3 / 91 (2): a thin moving rim, §6 | INFO 448 / 5562 (4): jitter-cycle shimmer | 2.786 / 14.762 / 14.780 | 0.514 / 5.008 / 5.667 |
| `taa-orbit` | INFO (camera only) | INFO | 0.415 / 3.928 / 3.912 | 0.142 / 1.571 / 1.563 |

`ghost-region` passes on every verdict region (TAA ≤ the larger of MSAA's
and the oracle's on the same still pixel-frames). TAA's mean is above MSAA's
on `taa-skin`'s and `particles`' trail bands and on the static areas of the
skinned and particle scenes, where every mode is within 0.05 of the
reference and the oracle itself reads 0.11-0.30.

The other rows are the same at both cuts within a few events. Per level
(0.99 cut), trail-band events longer than one frame (`taa-motion` /
`prefab-skinned-morph` / `anim-skinned` / `taa-skin` / `taa-thin`):
`Depth` 266 / 2 / 1 / 17 / 105, `DepthAndMotion` 26 / 1 / 1 / 0 / 8,
`DepthMotionAndId` 1 / 0 / 0 / 0 / 3.

### 7.3 Fallback path (`PrepReconstruction`, forced on the M1)

On the final resolve (round 3), forced fallback with the overlay: vectors
120/120; every gate line equals the geometry path's to three decimals (trail
`taa-motion` 3.791 against 3.793, `taa-skin` 1.384 max 50 on both; stop
`taa-motion` / `taa-orbit` / `instancing-stress` / `taa-skin` /
`taa-vertex-hook` 0.187 / 0.296 / 0.419 / 0.073 / 0.162; converged `taa-skin`
0.174, `taa-vertex-hook` 0.305, `instancing-stress` 0.759; flicker equal).
The table below was measured on the round-1 resolve when the overlay was
built; its comparisons between the paths hold.

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

### 7.4 Upscaling gates

Relaxed thresholds per 05 §4.5 (1.5x at 0.67, 1.75x at 0.58, 2x at 0.5) for
converged, pan and stop; stop is INFO at scale (its floor is the upscaled
converged image). Sharpening 0. Full-resolution references; linear decides,
Karis in parentheses.

| Gate | Scene | 0.67 | 0.58 | 0.5 | Verdict |
|---|---|---|---|---|---|
| Converged (MSAA 5.525) | `taa-thin` | 1.254 (2.150) | 1.783 (2.536) | 2.679 (3.265) | PASS, below 1x MSAA at every scale |
| Converged (INFO) | `taa-orbit` (MSAA 3.184) | 2.036 | 2.637 | 3.555 | above 1x MSAA at 0.5 |
| Converged (INFO) | `taa-motion` (MSAA 2.051) | 0.551 | 0.775 | 1.132 | |
| Converged (INFO) | `taa-reveal` (MSAA 1.770) | 0.539 | 0.667 | 0.922 | |
| Pan | `taa-orbit` | worst 2.329 (2.534) | 2.934 (3.139) | 3.764 (3.963) | PASS (FAIL at the unrelaxed 3 at 0.5) |
| Flicker | `taa-thin` | 0.297 p99 2 | 0.297 p99 2 | 0.280 p99 2 | PASS at the literal native bound |
| Flicker | `taa-orbit`, `taa-motion`, `taa-reveal` | ≤ 0.122 p99 1 | ≤ 0.113 p99 1 | ≤ 0.102 p99 1 | PASS |
| Stop (INFO) | orbit / thin / reveal / motion | 0.819 / 1.122 / 0.426 / 0.413 | 1.206 / 1.672 / 0.522 / 0.626 | 1.768 / 2.553 / 0.753 / 0.976 | under the relaxed bound except `taa-thin` at 0.5 |
| Scale change 0.67 → 0.5 → 0.67 | `taa-orbit` | no frame > 3: worst 3.900 @k82 | | | FAIL (literal) |
| Scale change, restated | `taa-orbit` | pop +0.626 (bound +1.5); settled +0.008 (bound +0.25) | | | PASS |
| Isolated-detail dropout (INFO) | `taa-thin`, 4822 px | 4.4% | 7.3% | 14.6% | |
| Trail (INFO) | `taa-motion` / `taa-reveal` | worst 6.28 / 5.15, max 123 / 177 | | | |
| Phase 3 (INFO) | `taa-transparent` band ∩ content / own trail | 26.6 / 11.5 | | | above MSAA and no AA (native) |
| HUD | `taa-hud` | worst 0.984, average 0.829, max 132 | | | PASS (< 1) |

The literal scale-change failure is the 0.5 steady state under orbit (a
constant-0.5 run is itself above 3), not a reset pop: the switched run is at
most 0.626 above a constant-scale run at the same scale and frame (on the
up-step frame), and within 0.008 of it from 8 frames after each step.

### 7.5 The sweeps behind the shipped values

| Value | Sweep | Chosen |
|---|---|---|
| `max_history_moving` 1 (round 3; 2 in round 2) | round 2, default 4: `taa-orbit` stop 1.144, pan 1.483, motion flicker 0.057; 2: 1.079, 1.206, 0.075; 1: stop 1.035. Round 3 (on the round-3 resolve), 2 → 1: moving-region error `taa-motion` 2.52 → 2.20, `taa-orbit` 1.20 → 1.06; strafe shimmer 1.21 → 1.06; pan 1.006 → 0.858 (2.495 → 2.329 at 0.67); stop `taa-orbit` 0.343 → 0.296; still-image flicker unchanged | 1 |
| `motion_edge_speed` 0.5 (round 3; was 2.0) | slow silhouettes kept part of their history and ghosted for 15-45 frames at 2.0 | 0.5 |
| `reactive_particles` 1.0 (round 3) | coverage-based, not scaled by alpha; at `alpha · 0.3` soft sprite skirts kept a stale colour for up to 32 frames | 1.0 |
| `reactive_lines` 0.3 | 1.0: `taa-transparent` moving-region ghost pixel-frames 184,562 → 15,633, still-image flicker 0.291 → 0.372 p99 3 → 4 (fails) | 0.3 |
| `edge_release` 0 | 1.0: `taa-motion` trail events longer than one frame 1 → 0; flicker `taa-thin` 0.301 → 0.323, `instancing-stress` 0.105 → 0.151; resolve +0.07 / +0.23 ms | 0 (off) |
| `motion_edge_alpha` 1.0 (was 0.6), `CLIP_ALPHA` 1.0 (was 0.5) | combined with the above: `taa-motion` trail 3.23 (ring 12.24, contact 5.06), stop 1.063, pan 1.195, motion flicker 0.102 | 1.0 / 1.0 |
| `reactive_transparent` 0.3 (was 0.8) | whole-image static flicker of `taa-transparent` at `reactive_material` 0.2: 0.1 → 0.27 (p99 3), 0.3 → 0.30 (3), 0.5 → 0.32 (4), 0.8 → 0.36 (4); particle trail avg 8.1 at 0.8, 9.8 at 0.3 (MSAA 6.12) | 0.3 |
| `reactive_material` 0.2 (was 0.6) | 0.4 fails flicker (0.51-0.61) | 0.2 |
| Reactive clip-box narrowing | tried, settled on none (scale 1.0) and removed | none |
| `mip_bias_extra` 0 | `taa-thin` at 0.67: 0 → 3.158, flicker 0.376 p99 4; −0.25 → 2.977, 0.424 p99 4; −0.5 → 2.994, 0.483 p99 6; −1 → 3.348, 0.629 p99 9 | 0 |
| Preset sharpening 0.25 | `taa-thin` at 0.67: 0 → 3.158 / 0.376; 0.15 → 2.940 / 0.391; 0.25 → 2.795 / 0.403; 0.35 → 2.669 / 0.417 | 0.25 |
| Upscaling history cap 16 × pixel ratio | 0.67: cap 24 → flicker 0.376 p99 4, stop 1.930, pan 2.689; cap 35.6 → 0.328 p99 3, 1.970, 2.698. 0.5: cap 24 → 0.484 p99 5; 48 → 0.379 p99 4; 64 → 0.352 p99 4, stop 3.927, pan 4.580 | ratio-scaled |
| Still γ widening 0.8 per unit of (O/R − 1) | 0.67, `gamma_still` 1.25 / 1.75 / 2.5 / 4: isolated features dropped 16.5 / 5.0 / 2.4 / 1.3 %, trail ring 21.3 / 23.8 / 25.9 / 27.2; 0.5: dropped 76.6 / 73.7 / 72.9 / 72.4 % | 1.74 at 0.67, 2.25 at 0.5 |
| Confidence term | near 1 on 95% of the image with the render-pixel kernel; lower with the output-pixel kernel (§3.7) | kept as designed |

The shipped values were checked bit-exact against `?tune=` overrides (10/10).

### 7.6 Frame cost

**Method** (05 §5): M1 Max, Chrome 158, 1920x1435 output (DPR 2), one tab,
nothing else rendering (a shared lock and queue kept other runs off the GPU).
Synthetic scenes (renderer `ec35a6fd`, the default `DepthMotionAndId`): 5
interleaved runs, 120 frames per configuration after 60 warm-up frames, GPU
frame = timestamp span per frame. Frames are submitted back to back and
drained every 4th, so the GPU does not clock down between frames: draining
every frame read light frames 1-1.5 ms high at random. A sentinel
configuration (`kitchen-sink`, no AA) before and after every run read
4.62-4.77 ms, with at most 0.11 ms drift within a run. A pass is timed by
repeating it N times and taking `(T(N) − T(1)) / (N − 1)`, because Apple
GPUs overlap pass timestamps.

| Row (05 §5) | Target | Measured | Verdict |
|---|--:|---|---|
| Velocity target + second vertex evaluation (+ seed, + previous-velocity and previous-visibility copies) | ≤ 0.2 ms | 0.15-0.19 ms, `taa-thin` 0.25, ±0.05 (TAA − no AA − resolve, N=32) | at the target except `taa-thin` |
| Resolve, native 1920x1435 | ≤ 0.6 ms | 1.53-1.55 ms, `taa-thin` 1.83 ms (frame and pass-timestamp slopes, N=8 and N=32) | not met (about 2.5-3x); the bar became "cheaper than MSAA 4x in Dreidel Derby" |
| Resolve, 0.67 → 1920x1435 | ≤ 0.6 ms | 1.46-1.48 ms, `taa-thin` 1.72-1.73 ms (the resolve runs at output size) | not met |
| Dreidel Derby mid-round, TAA native | ≤ 11.5 ms | **TODO (lead, pass C)** | |
| Dreidel Derby mid-round, TAA 0.67 | ≤ 7.5 ms | **TODO (lead, pass C)** | |
| TAA off | unchanged | synthetic scenes within ±0.053 ms of the round-2 build (12 configurations, A B × 3, mixed signs), which was within ±0.107 ms of 0.45; 36 baseline captures bit-identical; Dreidel Derby **TODO (lead, pass C)** | met |

Synthetic scenes, GPU frame median (ms; delta against no AA):

| Scene | No AA | MSAA 4x | SMAA | TAA native | TAA 0.67 | TAA 0.5 | 0.67 + RCAS 0.25 | Forced fallback |
|---|--:|--:|--:|--:|--:|--:|--:|--:|
| `kitchen-sink` | 4.709 | 5.676 (+0.97) | 4.905 (+0.20) | 6.390 (+1.68) | 5.238 (+0.53) | 4.836 (+0.13) | 5.438 (+0.73) | 6.607 (+1.90) |
| `instancing-stress` | 3.412 | 5.873 (+2.46) | 3.691 (+0.28) | 5.156 (+1.74) | 4.177 (+0.76) | 3.953 (+0.54) | 4.354 (+0.94) | 5.283 (+1.87) |
| `ssr` | 3.857 | 4.556 (+0.70) | 4.035 (+0.18) | 5.582 (+1.72) | 4.453 (+0.60) | 4.017 (+0.16) | 4.624 (+0.77) | 5.695 (+1.84) |
| `taa-thin` | 3.670 | 6.196 (+2.53) | 4.162 (+0.49) | 5.748 (+2.08) | 3.957 (+0.29) | 3.314 (−0.36) | 4.127 (+0.46) | 5.881 (+2.21) |

- Resolve per scene (`kitchen-sink` / `instancing-stress` / `ssr` /
  `taa-thin`): frame slope N=8 1.542 / 1.547 / 1.530 / 1.827 ms, N=32
  1.533 / 1.552 / 1.528 / 1.826 ms; the `Taa` pass-timestamp slope agrees
  within 0.016 ms. At 0.67: N=8 1.475 / 1.474 / 1.461 / 1.723, N=32
  1.462 / 1.469 / 1.461 / 1.734. Round 2's resolve read 1.21-1.26 ms
  (`taa-thin` 1.39-1.41).
- Per disocclusion level (N=8, two reps agreeing within 0.01, `kitchen-sink`
  / `taa-thin`): `Depth` 1.23 / 1.30 ms, `DepthAndMotion` 1.47 / 1.64,
  `DepthMotionAndId` 1.54 / 1.83. `edge_release` 1.0 adds 0.07 / 0.23.
- Where the round-2 resolve's time went (each part removed in turn,
  `kitchen-sink` / `taa-thin`): disocclusion 0.31 / 0.42 ms (two static
  reprojections, up to 8 footprint loads, about 10 depth linearisations; the
  motion test about 0.06), Catmull-Rom history 0.19, the clip-box floor's
  curve and its inverse 0.07-0.09, the display curve elsewhere 0.01-0.03, the
  rest (tile loads, 3x3, clip, blend, two stores) about 0.6. Measured and not
  taken: one precomputed reprojection matrix (at most 0.025 ms, not
  bit-exact), the slope slack from raw depth (no gain), the floor only on
  bright boxes (not exact).
- RCAS (`TaaRcas`): 0.165-0.203 ms.
- Reactive seed: no timestamp of its own; round 1 bounded it at 0.03-0.04 ms
  by repeat-N.
- Forced fallback (`PrepReconstruction`, with the velocity overlay) against
  the geometry target: +0.11 to +0.22 ms.
- TAA native against MSAA 4x: cheaper on `instancing-stress` (−0.72) and
  `taa-thin` (−0.45), dearer on `kitchen-sink` (+0.71) and `ssr` (+1.03).
  TAA 0.67 is cheaper than MSAA on all four (−0.10 to −2.24) and +0.29 to
  +0.76 ms above no AA.
- Zero cost off (A = the round-2 build, B = `ec35a6fd`, A B × 3): all 12
  no-AA / MSAA / SMAA configurations within ±0.053 ms with mixed signs. Round
  2 measured that build within ±0.107 ms of v0.45.0.
- Resolution governor (round 2, `kitchen-sink`, 0.67 ↔ 0.5 every 120 frames,
  five runs, two with per-span CPU splits): CPU `render()` median 0.60 ms; a
  step frame 0.7-1.2 ms (render-target and bind-group rebuilds, light-culling
  buffers), the frames after it 0.5-0.8 ms. On the GPU the step frame and the
  next read a few ms above the median under this method's idle-clock noise
  (±2 ms).

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
