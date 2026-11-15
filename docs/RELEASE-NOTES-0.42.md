# Release notes — 0.42

0.42 is about the GPU side of a frame: measuring it from an app, and cutting
the two costs that measurement exposed in a simple scene.

It started from a report that the awsm.fun physics examples ran at ~75–90 fps
on a 120 Hz MacBook while their stats panel read "0.6 ms" per frame. That stat
was render-thread CPU time; the GPU was the bottleneck and nothing reported it.

## Measured

Apple M1 Max, Chrome 157, Metal, 120 Hz display, GPU otherwise idle. The
`box3d-multithreaded` example (floor, walls, a ball, one directional light +
one shadowed point light, IBL, contact shadows, denoise on) at native
2400×1794. GPU frame = latest pass end − earliest pass begin, from timestamp
queries on every pass.

| | 0.41.1 (MSAA 4× + SMAA) | 0.42.0 (MSAA 4×) |
|---|---|---|
| GPU frame | 10.5 ms | 6.7 ms |
| presented fps | 88 | 120 |
| Shadow Denoise Blur H + V | 4.15 ms | 0.63 ms |
| SMAA pre-pass | 0.22 ms | — (app default now MSAA only) |

The physics-mujoco and physics-isaac examples at native: 84 → ~105 fps and
92 → ~113 fps.

## Shadow denoise blur: shared-memory tiles, a sky skip, a radius knob

The separable edge-aware blur on the shadow-visibility buffer loaded, per pixel
and per axis, 9 texels of the depth target (4× multisampled under MSAA) plus 9
visibility texels, all from memory. It now runs in 16×16 workgroups that load
their tile plus a `R`-texel apron along the blur axis into workgroup memory
once — each texel's linear view-z reconstructed once, its visibility packed to
a `u32` (lossless: the source is `Rgba8unorm`) — and the taps read from there.

- **Sky is skipped.** `cs_prep` never writes visibility for empty pixels and
  nothing reads it there, so sky centres do no work and no store, and sky
  neighbours weigh 0. Sky = raw depth equals the convention's clear value
  (`reverse_z` is now part of the blur's shader key).
- **`ShadowsConfig::denoise_radius`** (new, default 4 = the previous 9-tap
  kernel, clamped to `1..=8`). The spatial Gaussian now has variance `R²/4`, so
  R=4 reproduces the old weights and the outer tap weighs `e⁻²` at any R. It is
  baked into the blur shader: after changing it on a running renderer, call
  `ensure_config_pipelines` (like `denoise` and the SSCS fields). Each MSAA
  branch remembers the radius its blur was compiled with, so an MSAA flip
  after a radius change recompiles instead of reusing the old one.

At R=4 the image is unchanged apart from sky texels (whose visibility nobody
reads), up to rounding of the 8-decimal weight table.

## GPU frame time for apps

- **`profiling::gpu_frame_timing()`** — the whole-frame GPU span as a
  `TimingStat` (`GPU_FRAME_SCOPE`, `"Frame"`). Turn on with
  `AwsmRendererLogging { gpu: TimingTier::Frame, .. }`; silently absent without
  the `timestamp-query` feature.
- **`TimingStat::total_ms`** (new field) — cumulative, so with `count` an app
  gets exact per-window means instead of an EMA.
- **More passes are timed:** shadow generation (summed over views), EVSM
  moments/blur, prep edge, the denoise blur, the opaque final blend,
  transparent (+ HUD) and lines. `?gputime` no longer hides the shadow and
  denoise cost. A name timed several times in a frame is summed before it is
  recorded, so every row reads "per frame".
- The query set grew from 64 to 256 slots (point-light cube faces).
- `TimingTier::Frame` is documented as what it is: per-pass timestamps plus the
  frame span (previously documented as "one begin+end per frame").

## Light culling: skip a dispatch that only re-zeroes zeros

On a frame with lights but no point/spot lights, the cull dispatch still ran a
workgroup per screen tile and per froxel just to clear counts. Once a dispatch
has left the counts zero for the live froxel grid, later directional-only
frames skip it until a punctual light appears or the grid changes. The "clean"
state is committed only after the frame's commands are submitted (a frame that
errors after recording the dispatch never reached the GPU), and is keyed on
the grid, because a viewport shrink keeps the buffers and can bring stale
counts back into view on a regrow.

(The box3d scene above has a shadowed point light, so it still culls: ~1.1 ms
at 4.3 MP for one light. Making the cull cheaper for few lights is open.)

## API changes

- `ShadowsConfig::denoise_radius: u32` (+ `DENOISE_RADIUS_MIN/MAX/DEFAULT`).
- `PrepPassConfig::denoise_radius` + `clamped_denoise_radius()`.
- `ShaderCacheKeyShadowBlur` gains `radius` and `reverse_z`.
- `MaterialPrepPipelines::merge_resolved` takes `&MaterialPrepPrewarmDescriptors`
  (was `&[PrepPipelineSlot]`); the descriptors carry `blur_radius`.
  `has_branch_for` takes the live `denoise_radius`.
- `TimingStat::total_ms`; `profiling::{gpu_frame_timing, GPU_FRAME_SCOPE}`.
- `LightCullingRenderPass` gains `clean_grid` / `pending_clean_grid` +
  `commit_submitted()`.

A struct literal of `ShadowsConfig` without `..Default::default()` needs the
new field. A scene's `[shadows]` block doesn't carry `denoise_radius` (like
`denoise`, it's a runtime quality knob), so `From<schema::ShadowsConfig>`
fills the default; set it after applying a scene config if you change it.

## Why the frame costs what it does (for context)

What's left at 6.7 ms is mostly the renderer's architecture: a visibility
buffer shaded by full-screen compute passes. On a tile-based GPU (Apple, and
most phones) a forward renderer keeps depth and the 4× MSAA samples on-chip
and writes the resolved image once; here every intermediate goes to memory
and back, and MSAA needs its own edge passes. That fixed per-pixel cost buys
scaling to many materials, lights and heavy geometry, and dominates in a
five-object scene. Pixel count matters as much: three.js and Babylon render at
CSS-pixel resolution unless the app opts in, a quarter of the pixels of a
DPR-2 canvas. The awsm.fun examples now default to native resolution capped at
a 2.8 MP budget (never below an effective DPR of 1), with a Settings slider.
