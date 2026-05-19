# Shadows

This document covers the shadow subsystem from an authoring &
integration perspective. Per-subsystem design rationale lives in the
relevant code comments — see the "Implementation notes" map at the
bottom for where to look.

## TL;DR

* **Directional lights** cast cascaded shadow maps (up to 4 cascades,
  texel-snapped + bounding-sphere-stable; soft PCF and EVSM available
  on far cascades).
* **Point lights** cast cube-map shadows. The cube convention is
  D3D-style; if you're authoring matrices yourself, see
  [crates/renderer/src/shadows/mod.rs](../crates/renderer/src/shadows/mod.rs)
  for the canonical Y-flip + `front_face = Cw` pattern.
* **Spot lights** cast a single perspective shadow map.
* **Transparent geometry** receives shadows in the same way opaque
  does (16.B landed the bind-group consolidation that unblocks this);
  transparent receivers honour the same `Mesh::receive_shadows` flag
  as opaque.
* **Sample-site filters**: `Hard` (1-tap), `Soft` (16-tap rotated
  Poisson PCF with a world-unit-sized kernel), `Pcss` (variable-
  kernel PCF with blocker search — 2D only). EVSM is selected
  per-cascade via `evsm_cutoff` and gives a single-bilinear-fetch
  path for the farthest cascades.
* **Screen-space contact shadows (SSCS)** refine the dominant
  directional light's term in the opaque pass.

## Configuration surface

### Per-light

`awsm_scene_schema::LightShadowConfig` (and its runtime mirror
`awsm_renderer::shadows::LightShadowParams`). Available in the editor
under each Light node's **Shadows** sub-panel.

| Field                       | Type / range                                            | Notes                                                                                       |
| --------------------------- | ------------------------------------------------------- | ------------------------------------------------------------------------------------------- |
| `cast`                      | bool                                                    | Master toggle. Defaults `true` for newly-authored lights.                                   |
| `depth_bias`                | f32 ≥ 0                                                 | Constant comparison bias. Default `0.0005`.                                                 |
| `normal_bias`               | f32 ≥ 0                                                 | Receiver offset along surface normal *before* projection. Default `0.05`.                   |
| `resolution`                | 512 / 1024 / 2048 / 4096                                | Per-cascade / per-spot map resolution. Point lights ignore this — they use the global pool. |
| `hardness`                  | `Hard` / `Soft` / `Pcss`                                | Sample-site filter. `Pcss` runs on every light kind; point-light Pcss is a fixed-kernel widened-Soft (see Known limits). |
| `pcss_penumbra_scale`       | f32, default 1.0                                        | Only consulted when `hardness == Pcss`.                                                     |
| `max_distance`              | f32 (m)                                                 | Camera-distance cutoff. Beyond this, the light's shadow fades.                              |
| `cascade_count`             | 1..=4 (directional only)                                | Default 4. Lower for cheaper but coarser shadow coverage.                                   |
| `cascade_split_lambda`      | 0.0..=1.0 (directional only)                            | PSSM split bias. `0` = uniform; `1` = logarithmic. Default `0.5`.                           |
| `evsm_cutoff`               | `Off` / `LastCascade` / `LastTwoCascades` (directional) | Which trailing cascades store EVSM moments instead of PCF.                                  |
| `far_cascade_update_rate`   | `EveryFrame` / `Every{2,4,8}Frames` (directional)       | Default `Every4Frames`. Drift check still invalidates immediately on user-driven motion.    |
| `cube_face_update_rate`     | `EveryFrame` / `Every{2,4,8}Frames` (point only)        | Per-face throttle for point shadows. Default `EveryFrame`.                                  |

### Per-mesh

`awsm_scene_schema::MeshShadowConfig`. In the editor each mesh-bearing
node (Primitive / Mesh / Sweep / Instances / Model) has cast / receive
toggles in its inspector.

| Field      | Type | Notes                                                                                                                              |
| ---------- | ---- | ---------------------------------------------------------------------------------------------------------------------------------- |
| `cast`     | bool | Mesh appears in shadow generation. Default `true` for opaque, `false` for transparent (the loader flips it via `TRANSPARENT_DEFAULT`).|
| `receive`  | bool | Mesh's shaded pixels darken under shadow lookup. Default `true`.                                                                   |

Sprites, lines, particles are hardcoded no-cast / no-receive in v1 —
they don't expose toggles.

### Renderer-wide

`awsm_scene_schema::ShadowsConfig` (mirrors
`awsm_renderer::shadows::ShadowsConfig`). In the editor under
**Environment → Shadows…**.

| Field                       | Type                                | When it takes effect                                            |
| --------------------------- | ----------------------------------- | --------------------------------------------------------------- |
| `sscs_enabled`              | bool                                | Live (`set_shadows_config`).                                    |
| `sscs_step_count`           | u32 (1..=64)                        | Live.                                                           |
| `atlas_size`                | 1024 / 2048 / 4096 / 8192           | Live (tears down PCF atlas) + auto-grow up to 8192 on row-pack overflow. |
| `evsm_atlas_size`           | 512 / 1024 / 2048 / 4096            | Live (tears down EVSM atlas + ping-pong + bind groups).         |
| `evsm_exponent`             | f32 (default `10`, hard-clamped to `18` by `EVSM_EXPONENT_MAX_FP16`) | Live. Higher = harder shadow contact; pushing past ~18 saturates fp16 moments and the Chebyshev curve collapses into a near-binary mask. |
| `evsm_blur_radius`          | u32 (default `6`, clamped to `8` by the shader) | Live.                                                           |
| `max_point_shadows`         | 0 / 2 / 4 / 8 / 16                  | Live (tears down the cube pool).                                |
| `point_shadow_resolution`   | 256 / 512 / 1024 / 2048             | Live (tears down the cube pool).                                |
| `debug_cascade_colors`      | bool                                | Live.                                                           |

The "tears down" fields recreate the underlying GPU textures + bind
groups at the start of the next `write_gpu` — fine from the inspector
(the user-facing latency is one frame) but don't drive them at frame
rate. The 2D PCF atlas additionally auto-grows when the row-pack
allocator overflows, on top of the explicit size users may set here.

## Player / runtime integration

Non-editor consumers (in-game runtime, headless build) load the
serialized `EditorProject` from disk and feed shadow settings into the
renderer the same way the editor bridge does:

```rust
use awsm_renderer::{AwsmRendererBuilder, shadows};
use awsm_scene_schema::EditorProject;

let project: EditorProject = serde_json::from_str(&project_json)?;

// Resource-shaped fields land at construction time.
let mut renderer = AwsmRendererBuilder::new(gpu)
    .with_shadows_config(project.shadows.clone().into())
    .build()
    .await?;

// Live tunables can be pushed any time after build.
renderer.set_shadows_config(project.shadows.into());

// Per-light shadow params — once the light is inserted.
let light_key = renderer.lights.insert(my_light)?;
renderer.set_light_shadow_params(light_key, light_cfg.shadow().clone().into())?;

// Per-mesh flags — once the mesh is inserted.
renderer.set_mesh_shadow_flags(mesh_key, mesh_shadow.into())?;
```

The `From<schema::ShadowsConfig> for renderer::ShadowsConfig` (and
sibling conversions for `LightShadowParams`, `MeshShadowFlags`, the
update-rate enums, etc.) are gated behind the `scene-schema` feature
on the renderer crate:

```toml
awsm-renderer = { path = "...", features = ["scene-schema"] }
```

without that feature the renderer never compiles `awsm_scene_schema`
into its tree — keeping the renderer scene-schema-free for consumers
that have their own serialization pipeline.

The editor frontend keeps a hand-rolled bridge in
`scene-editor/src/renderer_bridge/{node_sync,shadows_sync}.rs` for
historical reasons; new player code should prefer the From impls.

## Performance implications

### Cost knobs, biggest-impact first

* **`cascade_count`** — every cascade is a full scene re-rasterization
  at `resolution`. 4 cascades × 2048² ≈ 64 MB of depth atlas. Dropping
  to 2 for low-end devices is a 2× win.
* **`max_point_shadows` × `point_shadow_resolution`** — VRAM grows as
  `24 · res² · max_lights` bytes. The defaults (8 lights × 1024²) cost
  ~24 MB. Mobile-class browsers usually want `512²` × `4` lights
  (~3 MB) or smaller.
* **`hardness`** at the receiver site:
  * `Hard`: 1 comparison tap per fragment.
  * `Soft`: 16-tap rotated Poisson disk PCF. Kernel half-width is
    sized in *world units* (`soft_world_radius`, ~25 cm at default
    light angles) divided by `world_per_texel`, clamped to
    `[3, 20]` texels. With hardware 2×2 bilinear comparison per tap
    that's ~64 effective samples — a clearly soft edge across the
    full cascade chain.
  * `Pcss`: 16-tap blocker search + 16-tap variable-kernel PCF. AAA
    quality, AAA cost. Reserve for hero lights.
* **`evsm_cutoff`** — promotes one or two trailing cascades to EVSM,
  which trades a moment-write + Gaussian blur compute pass per
  cascade for a single bilinear fetch + Chebyshev visibility at the
  receiver. Best for distant cascades where penumbras are large.
  Default `LastCascade`.
* **`far_cascade_update_rate`** — defaults to `Every4Frames` for the
  largest cascade. The drift check still invalidates the cache as
  soon as the camera or light moves above ~0.001 in VP-norm units, so
  user-driven motion is immediate; the throttle only matters when the
  scene is idle.
* **`cube_face_update_rate`** — same idea but per-cube-face.
  `Every2Frames` halves point-shadow cost; safe for slow-moving
  fixtures (architectural fills, torch flames).
* **`sscs_step_count`** — single-pass fragment ray-march; 16 steps is
  the AAA default. Drop to 8 on weaker GPUs.
* **`evsm_blur_radius`** — capped at 8; larger doesn't visibly
  improve. The blur runs as two separable compute passes per EVSM
  cascade.

### Memory budget rules of thumb

| Tier      | `atlas_size` | `point_shadow_resolution` | `max_point_shadows` | `evsm_atlas_size` | Approx VRAM  |
| --------- | ------------ | ------------------------- | ------------------- | ----------------- | ------------ |
| Mobile    | 2048         | 256                       | 2                   | 1024              | ~18 MB       |
| Desktop   | 4096         | 1024                      | 8                   | 2048              | ~108 MB      |
| Hero      | 8192         | 2048                      | 16                  | 4096              | ~450 MB      |

These are *only* the shadow subsystem; the geometry pass + IBL +
visibility buffer all have their own footprints.

### Quick wins

* **Mobile target?** Drop `cascade_count` to 2, `hardness` to `Soft`
  globally, and set `cube_face_update_rate` to `Every2Frames` for
  background lights.
* **Many static point lights?** Use `cube_face_update_rate =
  Every4Frames` (or `Every8Frames`) on lights that don't move.
* **Static directional sun?** Set `far_cascade_update_rate =
  Every8Frames` — the far cascade only re-renders when the camera
  moves enough to matter.
* **Hero shot** (cinematic camera move)? Crank everything up: `Pcss`
  hardness, `cascade_count = 4`, `evsm_cutoff = LastTwoCascades`,
  `cube_face_update_rate = EveryFrame`.

## Authoring guidelines

### Setting up a directional sun

1. Insert a Directional Light.
2. Set its rotation so the light points the direction you want
   (default points down −Z; rotation rotates that vector).
3. In **Shadows** sub-panel: keep defaults; check that **Cast** is on.
4. Tune `cascade_split_lambda` if you see banding between cascades:
   * `0.0` (uniform splits) gives even far-field detail but blocky
     near shadows.
   * `1.0` (logarithmic) gives the opposite. `0.5` is the AAA default
     and usually right.

### Setting up a point light with shadows

1. Insert a Point Light.
2. Position it and set `range` to cover what you want lit. `max_distance`
   gates the receiver-side fade; usually leave equal to `range`.
3. **Hardness** — `Soft` is the sweet spot. `Pcss` is not available
   for points (cube PCSS is deferred).
4. If the light is static, set `cube_face_update_rate` to
   `Every2Frames` or higher.

### Transparent geometry

The opaque pass writes a pre-shadowed lit colour buffer; the
transparent pass shades over the top *with the same shadow bind
group*. To make a glass / particle / etc. node receive shadows:

1. Mesh inspector → **Receive shadows = on**.
2. Don't bother enabling **Cast** unless the alpha mask is sharp
   enough to read as opaque in the shadow gen pass.

Note that transparent geometry does **not** participate in
**screen-space contact shadows** — sampling the in-progress depth
target on the same pass would deadlock. The shader gates `apply_sscs`
behind `sscs_available = false` on the transparent pipeline.

### Debugging

* **Cascades look weird** → toggle `debug_cascade_colors`. Each cascade
  gets a coloured tint; you'll see exactly which one a receiver pixel
  landed in. The palette distinguishes PCF cascades (red / green /
  blue / cyan) from EVSM cascades (scarlet / orange / yellow / gold),
  so it doubles as the EVSM-is-actually-running indicator. Useful
  for tuning `cascade_split_lambda` and the per-cascade resolution.

### Testing EVSM visually

The default `evsm_cutoff = LastCascade` promotes only the
*farthest* cascade to EVSM, so you need to either get the camera
out to that cascade's coverage zone or shrink the cascade count.
Recipe for an unambiguous EVSM-vs-PCF demo:

1. Set `cascade_count = 2` on the directional light. With two
   cascades, cascade 0 is PCF and cascade 1 is EVSM under the default
   cutoff — the boundary between them is in the middle of the visible
   plane and both cascades appear on-screen simultaneously.
2. **Environment → Shadows… → Debug cascade colors = on**. You'll see
   a clear band: red (cascade 0, PCF) in the foreground and orange
   (cascade 1, EVSM) toward the back. The boundary is roughly where
   the soft transition between the two sampling paths happens.
3. Compare against `evsm_cutoff = Off`: both cascades go cool-toned
   (red + green) and the far cascade switches from EVSM Chebyshev to
   PCF, which on a typical test scene looks visibly *sharper* in
   that band (no Gaussian moment blur).
4. To exaggerate the EVSM softness, bump `evsm_blur_radius` to its
   max (`8`) and watch the orange cascade go almost cloud-soft.
5. Don't forget to flip `debug_cascade_colors = off` when done —
   the warm tint masks the actual shadow output, which is what you
   normally care about.
* **Shadow shimmers under camera motion** → the texel-snap is working,
  but with `far_cascade_update_rate > EveryFrame` you'll see drift
  catch-up jumps. Set the rate back to `EveryFrame` for that light.
* **Phantom doubled shadow on point lights** → very likely a stale
  shader cache; force a full rebuild. The cube Y-flip fix is in
  [shadows/mod.rs](../crates/renderer/src/shadows/mod.rs) and should
  produce single coherent shadows for all `up=-Y` cube face configs.
* **Peter Panning** (shadow detached from object) → bump `normal_bias`.
  `depth_bias` is the wrong knob for this — it controls comparison
  tolerance, not receiver offset.
* **Shadow acne** (zebra stripes on a lit surface) → bump
  `depth_bias` slightly. If acne shows up only at grazing angles,
  bump `normal_bias` instead.
* **PCSS edges look like noise** → reduce `pcss_penumbra_scale`.
  Higher values widen the kernel beyond the cascade's tile, where
  the blocker-search hits clamped texels.

## Known limits / deferred work

* **Cube `Pcss` is a fixed-kernel widened-Soft, not a true
  blocker-search PCSS** — `texture_depth_cube_array` doesn't expose
  raw depth reads in WGSL (only comparison sampling), so the cube
  path can't run the variable-kernel blocker-search the 2D path
  does. The `Pcss` hardness on a point light therefore samples the
  same 16-tap rotated-Poisson disc as `Soft` but widens the disc by
  `pcss_penumbra_scale * 3`. The slider still works for "more or
  less penumbra"; the visual difference vs true PCSS is subtle at
  typical point-light scales (range 1–30 m).
* **Spot light PCSS** — supported (it's a 2D shadow), but you may
  want to tune `pcss_penumbra_scale` per-spot since the cone half-
  angle affects the apparent penumbra.
* **EVSM atlas auto-grow** — the EVSM atlas is recreated on explicit
  `evsm_atlas_size` change, but unlike the 2D PCF atlas it does not
  auto-grow on packer overflow (an overflowing far cascade silently
  drops to PCF for the frame). Author the size up front instead.
* **Per-cascade frustum-cull broad-phase** — every shadow view
  iterates every mesh and tests AABB-vs-frustum. For a 100-mesh /
  4-cascade / 1-cube-light scene that's ~1000 checks/frame. A
  spatial-hash or sorted-by-X broad-phase would cut this to
  `meshes × overlapping_cascades`. Hold off until a test scene
  passes ~50 dynamic meshes; today the per-view cull is plenty.
* **Cascade-blend zone tuning** — `CASCADE_BLEND = 0.5` in
  [bind_groups.wgsl](../crates/renderer/src/render_passes/shared/shared_wgsl/shadow/bind_groups.wgsl).
  Receivers inside the blend zone pay 2× PCF cost (sample both
  cascades and lerp). On a 4K screen with a single directional light
  the blend zone is ~60% of receiver pixels (4 cascades × 15%
  boundary, summed up). For high-detail scenes consider dropping
  to `0.10` at the cost of more-visible seams.

## Implementation notes

The detailed design rationale for individual subsystems lives in
code comments where the relevant types / shaders are defined — those
move with the code if it gets refactored. Map of where to look:

| Subsystem                          | Where the rationale lives                                                                                                                  |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| Cascade fit (stable-fit, splits)   | [`shadows/cascade.rs`](../crates/renderer/src/shadows/cascade.rs)                                                                          |
| Atlas clear-once / cube clear      | [`shadows/render_pass.rs`](../crates/renderer/src/shadows/render_pass.rs)                                                                  |
| Cube Y-flip + CW winding           | [`shadows/mod.rs`](../crates/renderer/src/shadows/mod.rs) (search for `y_flip`)                                                            |
| EVSM moment write + blur shaders   | [`shadows/evsm.rs`](../crates/renderer/src/shadows/evsm.rs)                                                                                |
| Cascade-blend, PCF / PCSS taps     | [`shared_wgsl/shadow/bind_groups.wgsl`](../crates/renderer/src/render_passes/shared/shared_wgsl/shadow/bind_groups.wgsl)                   |
| Equal-resolution cascades          | [`shadows/cascade.rs::cascade_resolution`](../crates/renderer/src/shadows/cascade.rs)                                                      |
| Slope-scale + constant depth bias  | [`build_shadow_pipeline`](../crates/renderer/src/shadows/mod.rs) (`with_depth_bias(1).with_depth_bias_slope_scale(1.5)`)                  |
| 16.B bind-group consolidation      | [`material_transparent/bind_group.rs`](../crates/renderer/src/render_passes/material_transparent/bind_group.rs)                            |

If a future regression makes one of these decisions look wrong, the
comment in the code block typically explains the trade-off the
current choice is making — read that first before changing.

## References

The algorithms used here aren't novel — they're the AAA standard
playbook. If you're touching a subsystem and the in-code comment
doesn't fully justify a choice, the underlying paper usually does.

- **Shadow mapping (foundation)** — Williams 1978, *Casting Curved
  Shadows on Curved Surfaces*. The depth-from-light comparison that
  everything else extends.
- **Percentage-Closer Filtering (PCF)** — Reeves, Salesin, Cook 1987,
  *Rendering Antialiased Shadows with Depth Maps*. The basis for the
  Soft hardness.
- **Cascaded Shadow Maps** — Microsoft's reference write-up:
  <https://learn.microsoft.com/en-us/windows/win32/dxtecharts/cascaded-shadow-maps>.
  Plus the *Common Techniques to Improve Shadow Depth Maps* sibling:
  <https://learn.microsoft.com/en-us/windows/win32/dxtecharts/common-techniques-to-improve-shadow-depth-maps>.
- **Practical Split Scheme (PSSM)** — Zhang et al. 2006:
  <https://www.cse.chalmers.se/~uffe/xjobb/Practical%20Split%20Scheme%20for%20Parallel-Split%20Shadow%20Maps.pdf>.
  The blend between uniform and logarithmic splits used by
  `cascade::pssm_splits`.
- **EVSM** — Lauritzen 2007 (*Summed-Area Variance Shadow Maps*) and
  the follow-on EVSM presentations. The depth-warp exponent + four
  moments + separable Gaussian blur pattern in `shadows/evsm.rs`.
- **PCSS** — Fernando 2005, *Percentage-Closer Soft Shadows* (NVIDIA).
  Blocker-search + variable-kernel PCF, used on the 2D path and
  approximated on the cube path (see "Known limits").
- **Cube depth sampling in WebGPU** — `texture_depth_cube_array` spec:
  <https://www.w3.org/TR/webgpu/#texture-depth-cube-array>. WebGPU
  follows the D3D cube-face convention, which drives the Y-flip + CW
  winding fix on the cube pipeline.
- **Contact shadows (SSCS)** — Drobot 2017 SIGGRAPH course notes;
  Bungie's *Destiny* implementation overview. The world-space ray
  march in `apply_sscs`.
- **glTF `KHR_lights_punctual`** — units / direction conventions
  this renderer follows:
  <https://github.com/KhronosGroup/glTF/tree/main/extensions/2.0/Khronos/KHR_lights_punctual>.
