# Release notes — 0.45

## 0.45.0

0.45 is about frame cost. It comes from profiling a small game scene (a table
top under 17 punctual lights, a PCSS spot shadow, SSR, bloom, and depth of field
on its title screen) on an M1 Max at 1920×1435, pass by pass. With MSAA 4× that
scene's GPU frame cost 2.9× the frame without AA. It now costs 1.4×.

| | 0.44.2 | 0.45.0 |
|---|--:|--:|
| Title screen, MSAA 4× | 53.5 ms | 19.2 ms |
| Title screen, no AA | 18.6 ms | 13.8 ms |
| Mid-round, no AA | | 10.5 ms |

The image is unchanged except where noted (SMAA, and DoF at silhouettes).

## MSAA

- **No `return` inside a `switch`.** The texture-pool samplers (opaque and
  transparent) and `flipbook_cell` returned from inside their `switch`. On
  Apple GPUs that alone made one full-screen quad cost 8.9 ms at 4 samples
  against 0.77 ms at 1, and 0.47 / 0.44 ms with a single exit. The
  transparent pass under MSAA went from 12.8 to 1.2 ms. The cause is the
  driver's, and unknown; `switches_have_a_single_exit` keeps every shader
  free of the pattern.
- **Edge pixels shade in a dispatch of their own** (`cs_shade_edge`, over the
  edge-pixel list). Inside the full-screen `cs_shade`, one edge pixel made
  every lane of its group wait through per-sample shading, and edges are thin
  lines crossing many tiles. The new kernel queues each edge pixel's distinct
  surfaces and shares them out over the group. Opaque shading under MSAA went
  from 15–18 to 8 ms.
- **The opaque image is blitted inside the transparent pass** under MSAA 4×,
  when no decal composite or `before_transparent_pass` hook needs it first.
  On its own the blit stored every sample only for the next pass to load each
  one back. New in `awsm-renderer-core`: `blit_draw` and
  `blit_get_pipeline_for_depth_pass`.
- **SSR reads a single-sample copy of depth** (sample 0, copied once a frame
  under MSAA). Random loads from the multisampled depth target cost about 2.7×
  single-sampled ones. SSR went from 4.1 to 1.6 ms, the same as without MSAA.
  The resolve and temporal shaders no longer have an MSAA variant.
- **Depth of field was rewritten.** `dof_prepare` writes each pixel's circle
  of confusion once, from the nearest of its depth samples, with each 8×8
  tile's largest. The gather reads that buffer and skips tiles no disc
  reaches. DoF under MSAA went from 10.6 to 2.4 ms, the same as without MSAA.
  Taking the nearest sample also removes a jagged halo: reading sample 0 made
  a silhouette pixel "background" to its neighbours.

## Every AA mode

- **SMAA finds edges in dark scenes.** The edge threshold was applied to
  linear luma, where a dark object on a dark ground is a tiny difference.
  It is now applied to gamma-encoded luma, the space the reference threshold
  is defined in.
- **The light walk lost a dead branch.** Every opaque kernel chose at runtime
  between the prep pass's shadow buffer and sampling shadow maps itself,
  though nothing samples them itself any more. The choice is now made at
  compile time: 0.64 ms with 17 lights.
- **The shadow denoise blur** gathered its taps into per-invocation arrays
  first. Arrays indexed by a loop counter live in memory rather than
  registers on Apple GPUs; adding the taps up directly took each axis from
  0.6 to 0.27 ms.
- **Light culling** ran a 64-wide workgroup per froxel: 345k workgroups to
  place 17 lights at 1920×1435. It now runs one per screen tile, one
  invocation per depth slice, with the tile's candidates read once into
  workgroup memory: 0.45 to 0.18 ms.
- **Switched-off lights are culled.** A light with zero intensity or colour
  was still in every froxel it touched and walked by every pixel there.

## Breaking

Only for code that reaches into render-pass internals:

- `ShaderCacheKeySsrResolve` / `ShaderCacheKeySsrTemporal` and their templates
  lost `multisampled_geometry`.
- `ShaderTemplateShadowBlurCompute` lost `tap_count`.
- `RenderTextureViews` has `ssr_depth`, and `RenderTexturesInner` has
  `ssr_depth` / `ssr_depth_view`.
- `CompileInstallTarget::EdgeResolveShade` and `EdgeResolvePipelineKeyId` have
  `edge_pixels` (`cs_shade_edge` rather than `cs_shade`).
- The light-culling shader declares `lights_storage` as `array<u32>`: no
  write to it is atomic any more.
