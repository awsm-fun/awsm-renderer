# 02 — What exists today

Everything TAA touches, as of 0.45.0. Paths are under
`packages/crates/renderer/src/`.

## Frame order (`render.rs`, `AwsmRenderer::render`)

1. Geometry raster: opaque, then HUD (own depth) into the same visibility
   targets.
2. Coverage tally, shadow maps, light culling.
3. Material classify, prep (world position, UVs, vertex colour, shadow
   visibility; shadow denoise blur), opaque shading (`cs_opaque`, or under
   MSAA `cs_shade` + `cs_shade_edge` + `final_blend`).
4. Opaque mip chain (only when something is transmissive).
5. Opaque → transparent blit (inside the transparent pass under MSAA 4×).
6. HZB, decals, occlusion cull (for the next frame).
7. Lines + material transparent pass (forward; particles are transparent
   meshes), then the HUD transparent pass.
8. Composite blit (no-AA path) or MSAA resolve into `composite`.
9. **SSR** on `composite`, with its own temporal pass.
10. SMAA edge + weight pre-pass (when SMAA is on).
11. Bloom pyramid.
12. Volumetrics (froxel inject + integrate, with its own temporal).
13. **Effects**: SMAA blend, haze, bloom composite, depth of field.
14. **Display**: exposure + tonemap (Khronos PBR Neutral or ACES) to the
    surface, with a downsample variant for supersampling.
15. `last_pass` hook.

TAA goes between 9 and 10 (03 §6).

## Resolution: already split

`AwsmRenderer::render_size()` (`anti_alias.rs`) is the swap-chain size times
`render_scale`, and every viewport-derived consumer reads it: render targets,
light-culling froxels, LOD screen error, the camera viewport uniform, the
picker. Only the display pass and swap-chain readbacks are canvas-sized.

`set_render_scale` currently clamps to [1.0, 2.0] (supersampling), and the
display shader has a downsample variant for it. Upscaling extends the same
mechanism below 1.0. What does not exist yet is a third size: the **post size**,
the output resolution at which TAA writes and bloom / effects / display run.
Today those all run at render size (03 §6).

## Camera (`camera/buffer.rs`)

- The uniform carries view, projection, view-projection and their inverses,
  position, frustum rays, viewport, DoF parameters, and, end-appended for SSR,
  **`prev_view_projection`: last frame's unjittered view-projection**. Frame 0
  gets the current one (zero motion). TAA can use this field as it is.
- `last_matrices` keeps last frame's *unjittered* matrices; `camera_moved`
  compares them.
- Jitter: `APPLY_JITTER = false`. When on it multiplies a translation into the
  projection. Two bugs (01): `jitter / width` where one pixel is `2 / width` in
  NDC, and amplitude 0.8 still / 0.2 moving. Its Halton generator is shared
  (`camera::halton`) and volumetrics uses it, so keep the function and fix the
  camera use.
- Nothing records the jitter offset itself, so no shader can remove it. TAA
  needs `jitter_px` (current and previous) uploaded (03 §1).
- Everything that reconstructs position from depth uses
  `inv_view_projection` / `inv_proj`. Once jitter is on those must be the
  *jittered* inverses (the depth buffer was rasterised jittered). They already
  are, because they are derived from the same `projection`. Do not "fix" that.

## Transforms (`transforms.rs`)

- World matrices live in one GPU storage buffer (`pack_world_transform`:
  model matrix + normal matrix, 112 bytes each), uploaded by dirty ranges.
  A previous-frame copy only needs the model matrix (64 bytes). In shared-arena mode the app
  writes local transforms into a SharedArrayBuffer and the renderer
  propagates them.
- **There is no previous-frame copy.** Per-object motion vectors need one
  (03 §2.3).
- Slots are recycled (`remove` / `insert`), so a "previous" buffer must never
  hand a new object the last occupant's matrix (G10).
- Instanced meshes take their per-instance transform as vertex attributes
  (`instance_transform_row_0..3` in the geometry vertex shader); the instancer
  writes them. Those need a previous copy too.

## Skinning, morphs, vertex hooks

All evaluated in the **vertex** stage: `shared_wgsl/vertex/apply_vertex.wgsl`
includes `skin.wgsl` (joint matrices from `skin_joint_matrices`), `morph.wgsl`
(weights from `geometry_morph_weights`) and, for custom materials,
`custom_vertex.wgsl` (user WGSL that gets `frame_globals`, so time). The same
code runs in the geometry, transparent and shadow passes.

Motion vectors therefore need the *previous* joint matrices, morph weights and
frame time (03 §2.3). A vertex hook that reads anything else stateful cannot be
reprojected exactly. Those meshes go reactive (03 §5).

## The geometry targets: the MRT budget is exactly full

| Target | Format | Bytes / sample |
|---|---|--:|
| visibility (triangle index + material meta offset) | `rgba16uint` | 8 |
| barycentric (bary xy + instance id) | `rgba16uint` | 8 |
| normal + tangent | `rgba16float` | 8 |
| barycentric derivatives (texture LOD) | `rgba16float` | 8 |
| **total** | | **32** |

WebGPU's default `maxColorAttachmentBytesPerSample` is **32**. The renderer
requests the adapter's maximum (`DeviceRequestLimits::max_all`). An M1 Max in
Chrome reports **128**, so a fifth `rg16float` velocity target (4 bytes) fits
there. Many mobile adapters report 32. That is why the design has a
reconstruction fallback (03 §2.4). No existing channel is free to pack
velocity into: every one of the 16 is used.

## Texture LOD

Opaque shading samples with explicit gradients built from the barycentric
derivatives target (`textureSampleGrad`). A mip bias for upscaling is a
multiply on those gradients in one place, with no sampler changes (03 §7).
The transparent pass samples in the fragment stage with plain `textureSample`
(`material_transparent_wgsl/helpers/texture_uvs.wgsl`), so it needs
`textureSampleBias` with the same bias.

## HUD

HUD meshes use the world camera and the same visibility targets, with their own
depth, and are skipped by opaque shading (`is_hud`) and drawn again in the HUD
transparent pass, into the same image TAA would accumulate. They must not be
accumulated (03 §5.4).

## Anti-aliasing modes (`anti_alias.rs`)

`AntiAliasing { msaa_sample_count: Option<u32>, smaa: bool, mipmap: bool }`.
Switching recompiles pipelines (the page shows "Compiling Pipelines"). TAA
becomes a third mode and turns MSAA and SMAA off (03 §6).

## Other temporal passes

- **SSR** (`render_passes/ssr/shader/ssr_wgsl/temporal.wgsl`): reprojects with
  `prev_view_proj`, clamps history to the 3×3 neighbourhood unconditionally,
  and ping-pongs history by frame parity. It reconstructs world position from
  the full-resolution depth. With jitter on, that depth moves by up to half a
  pixel each frame. That is fine for SSR's own reprojection (it uses the
  matching jittered inverse), but its trace jitter and TAA's jitter must not
  share a phase (03 §1.4).
- **Volumetrics**: froxel temporal, jittered with `camera::halton`.
- **Shadow denoise**: spatial only.

## Apple GPU pitfalls learned in 0.45 (write the resolve with these in mind)

- No `return` inside a `switch`: 8.9 ms against 0.47 ms on one quad at 4×.
- No per-invocation arrays indexed by a loop counter (the shadow blur's
  `taps` / `weights` cost 2×). A 3×3 neighbourhood loop must accumulate
  directly, or be unrolled.
- Random loads from multisampled depth cost about 2.7×. Irrelevant under TAA,
  because MSAA is off, but don't read the multisampled target anywhere new.
- Measure a pass by repeating it N times: (T(N) − T(1)) / (N − 1). Pass
  timestamps overlap on Apple GPUs, so per-pass numbers from the stats panel
  add up to more than the frame.
