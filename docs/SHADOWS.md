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
  [packages/crates/renderer/src/shadows/mod.rs](../packages/crates/renderer/src/shadows/mod.rs)
  for the canonical Y-flip + `front_face = Cw` pattern.
* **Spot lights** cast a single perspective shadow map.
* **Transparent geometry** receives shadows in the same way opaque
  does (16.B landed the bind-group consolidation that unblocks this);
  transparent receivers honour the same `Mesh::receive_shadows` flag
  as opaque.
* **Sample-site filters**: `Hard` (1-tap), `Soft` (PCF with a
  world-unit-sized kernel), `Pcss` (variable-kernel PCF with blocker
  search — on every light kind, incl. point/cube). All kinds sample one
  shared **baked Vogel (golden-angle) disc** — even, clump-free coverage
  at a runtime-chosen tap count. The per-light `shadow_samples` knob sets
  that count (the cost lever); an optional screen-space
  [denoise blur](#shadow-denoise-blur) smooths residual penumbra speckle
  so a modest count suffices. EVSM is selected per-cascade via
  `evsm_cutoff` and gives a single-bilinear-fetch path for the farthest
  cascades.
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
| `kernel_slack`              | f32, default 2.0 (point only)                          | Soft/Pcss self-shadow acne slack, in cube-texels of quantization to forgive (scaled per-texel, NOT by kernel radius — see Known limits). `0` = off.  |
| `shadow_samples`            | u32, default 16, clamped `[8, 64]`                     | Soft/Pcss Vogel tap budget — the per-shadowed-pixel `textureSampleCompare` cost knob, all light kinds (PCSS blocker search uses ¾). Higher = smoother penumbra, more cost; reserve high counts for hero lights. `Hard` ignores it. |
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
| `denoise`                   | bool (default `true`)               | Live. Edge-aware denoise blur on the per-pixel shadow-visibility buffer (see [Shadow denoise](#shadow-denoise-blur)). Editor toggle: **Settings → Shadow denoise**. |

The "tears down" fields recreate the underlying GPU textures + bind
groups at the start of the next `write_gpu` — fine from the inspector
(the user-facing latency is one frame) but don't drive them at frame
rate. The 2D PCF atlas additionally auto-grows when the row-pack
allocator overflows, on top of the explicit size users may set here.

## Shadow denoise blur

`ShadowsConfig::denoise` (editor: **Settings → Shadow denoise**, default
**on**) enables an edge-aware blur on the per-pixel shadow-visibility
buffer that the deferred prep pass produces.

**Why it exists.** Soft/PCSS penumbras are sampled with a finite,
per-pixel-rotated tap disc. Over a wide penumbra (point-light Pcss can
reach a ~1 m world disc) that undersampling reads as a "furry" speckle
fringe at the shadow edge — there is no temporal accumulation to resolve
it. Rather than pay for more taps inside the (per-light) shadow sampler,
the denoise blur smooths the *result* once.

**How it works — one pass, all lights.** The
[`material_prep`](../packages/crates/renderer/src/render_passes/material_prep)
pass writes every shadowed light's per-pixel visibility into one packed
`Rgba8unorm` array (`prep_shadow_visibility`, 4 lights per layer). The
denoise pass blurs *that texture*, so its cost is **independent of light
count** — 1 shadowed light and 100 cost the same. It is:

* **Separable** — a horizontal then a vertical 1D pass (writes a temp,
  then writes back into `prep_shadow_visibility` in place, so the opaque
  reader's binding never changes).
* **Edge-stopped by linear depth** — a neighbour whose reconstructed
  view-z differs from the centre by more than a small *fraction* (5%)
  falls off sharply. So the penumbra smooths on a continuous surface but
  shadow never bleeds across a silhouette (or into sky).
* **Skipped entirely** when the toggle is off (the dispatch is gated;
  the bind groups + pipelines stay resident so the flip is free).

**Limitation.** Under MSAA the thin silhouette-edge samples are shaded
from a separate compact buffer (`prep_edge_shadow`) that this blur does
not touch — only the full-screen interior is denoised. In practice the
visible penumbra noise is interior, so this is rarely noticeable.

See `render_passes/material_prep/shader/shadow_blur_wgsl/` for the
shader and `material_prep/render_pass.rs::render_blur` for the dispatch.

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
`packages/frontend/editor/src/engine/bridge/ (e.g. node_sync.rs)` for
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
* **`shadow_samples`** (per-light, default 16) — THE tap-count cost
  lever. Every shadowed pixel does `shadow_samples` (Soft) or
  `shadow_samples` + ¾·`shadow_samples` (Pcss, blocker + PCF) cube/atlas
  `textureSampleCompare`s, so cost is ~linear in it. Lower for fill
  lights / low-end GPUs; raise only for hero lights. The denoise blur is
  the cheaper *global* noise lever, so 16 + denoise usually beats 48 raw.
* **`hardness`** at the receiver site:
  * `Hard`: 1 comparison tap per fragment.
  * `Soft`: `shadow_samples`-tap Vogel-disc PCF. Kernel half-width is
    sized in *world units* (`soft_world_radius`) divided by
    `world_per_texel`. With hardware 2×2 bilinear comparison per tap the
    effective sample count is ~4×, a clearly soft edge.
  * `Pcss`: blocker search + variable-kernel PCF. AAA quality, AAA cost;
    reserve for hero lights. Tap budget is `shadow_samples` (PCF) +
    ¾·`shadow_samples` (blocker), all light kinds.
* **`denoise`** — one separable, light-count-independent screen-space
  blur on the shared visibility buffer (see [Shadow denoise](#shadow-denoise-blur)).
  Cheap relative to the shadow sampling it cleans up; on by default.
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
   `range = 0` (the glTF "infinite range" convention) is valid and *does*
   cast shadows — the shadow's cube far plane is then derived from
   intensity via the same `influence_radius` the lighting and culling
   paths use, so shadow reach matches lit reach. Caveat: a very bright
   infinite light yields a large cube far plane (poorer depth precision →
   possible acne); if that bites, set an explicit `range`. (Spot lights
   behave the same way.)
3. **Hardness** — `Soft` is the sweet spot. `Pcss` runs a real
   16-tap blocker search on the cube pool's 2D-array depth view
   before the variable-kernel PCF — slide `pcss_penumbra_scale`
   from 0.5 to 5.0 to widen the penumbra.
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

### Troubleshooting & bias tuning

Almost every shadow problem someone reports falls into one of the
seven shapes below. Read the **scale-aware-bias** preamble first
because half the others reduce to "your biases are wrong for your
scene scale."

#### Scale-aware bias defaults

`depth_bias` and `normal_bias` are not unit-free. They scale with
your world:

| Scene scale (typical extent)        | `depth_bias` | `normal_bias` |
| ----------------------------------- | ------------ | ------------- |
| **Small** (1–10 m worlds, indoor)   | `0.0001`     | `0.001`       |
| **Medium** (10–100 m, default)      | `0.0005`     | `0.05`        |
| **Large** (100+ m, open world)      | `0.001`      | `0.1`         |

`LightShadowParams::default()` ships the **Medium** values because
they suit the majority of scenes the editor will see. If your project
authors at a different world scale, override these per-light or
adjust the defaults in
[`shadows/light_shadow.rs`](../packages/crates/renderer/src/shadows/light_shadow.rs).

The two have completely different jobs — don't conflate them:

* **`depth_bias`** is an NDC-space tolerance subtracted from the
  receiver's projected depth before comparing to the shadow map. It
  exists so a surface's depth comparison against itself doesn't
  flicker between "shadowed / lit" under floating-point noise.
* **`normal_bias`** is a *world-space* offset along the receiver's
  surface normal applied **before** projecting into shadow space.
  It pushes the receiver toward the light so its projected position
  no longer self-intersects.

`depth_bias` fights the "binary value flipping" failure; `normal_bias`
fights the "geometry-too-thin-vs-bias" failure. They're not
interchangeable.

#### Peter-panning (gap between caster and its shadow on the ground)

**Symptom:** the shadow looks "detached" — there's a strip of lit
ground between the caster's contact point and where the shadow
actually starts.

**Diagnosis ladder:**

1. **`normal_bias` too large for scene scale.** For a 1 m cube under
   a 4 m point light, a `normal_bias` of `0.05` (5 cm) shifts the
   receiver's projected angle enough that the cube's silhouette
   sample "misses" the box. Reduce to scale-appropriate (see table
   above). Drop to `0` momentarily to confirm — if the gap closes,
   `normal_bias` was the cause.
2. **`depth_bias` too large.** If the gap persists at `normal_bias =
   0`, the depth comparison is missing because the receiver's projected
   depth minus `depth_bias` is *smaller* than the caster's stored depth.
   Lower `depth_bias` until the gap closes. Watch for **acne**
   reappearing on the caster itself; that's the trade-off floor.
3. **Receiver right behind a thin caster** (point lights only). The
   cube map stores the caster's *back* face (front-face culling), so
   for a receiver one cube-edge length behind the box, the depth gap
   is tiny and any `depth_bias > gap_size` peter-pans. This is the
   geometric limit; you can either accept a hairline gap on Hard
   sampling, switch the light to Soft (PCF blurs over the gap), or
   lower the light's `range` so depth precision is finer in the
   relevant window.

#### Shadow acne (zebra stripes on a lit surface)

**Symptom:** stripe or moiré pattern on surfaces that *should* be
lit — the caster self-shadows because its depth comparison against
its own shadow-map texel flickers.

**Diagnosis ladder:**

1. **`depth_bias` too small.** Bump it up. Usually one decimal place
   at a time.
2. **Grazing-angle acne only.** If acne appears only on surfaces
   nearly parallel to the light direction, bump `normal_bias` instead
   — the shader divides `depth_bias` by `max(n_dot_dir, 0.05)` so
   grazing surfaces already get an amplified bias, but `normal_bias`
   addresses the geometric cause more directly.
3. **Acne survives even at `depth_bias = 0.001`.** Likely a
   precision problem — see the **z-fighting on flat receivers** entry.

#### Z-fighting on huge flat receivers

**Symptom:** flickering speckle pattern on the receiver, especially
when the camera moves. Often shows up on a 10 km ground plane with
no subdivisions.

**Cause:** vertex coordinates far from the origin (±5 km) chew up
floating-point precision. Triangle interpolation across the giant
quad produces sub-mm depth wobble that exceeds any reasonable
`depth_bias`.

**Fix (preferred):** add **subdivisions** to the plane (Plane primitive
has `subdivisions_x / _z` knobs). Shrinking each triangle scales
the interpolation error down proportionally. A 100×100 subdivided
ground plane behaves correctly where a single quad of the same
extent flickers.

**Fix (alternative):** keep scene scale modest (single-digit km
maximum). Origin-snap the world each frame if you really need
unbounded extents (advanced, not implemented here).

#### Shadow shimmer / swimming under camera motion

**Symptom:** shadow edges visibly crawl or wobble as the camera
moves.

**Diagnosis ladder:**

1. **Far cascade catching up.** If `far_cascade_update_rate >
   EveryFrame`, the far cascade only refreshes every N frames; on
   the catch-up frame it can pop visibly. Set to `EveryFrame` for
   that light if the pop is unacceptable.
2. **Texel-snap working as designed.** The cascade is pinned to a
   texel grid (rotation-invariant, translation-stable); small
   sub-texel shifts as the camera translates are *not* a bug. If
   the swim is more than ~1 texel, file as a regression.
3. **SSCS hashing on screen coordinates.** Fixed in this branch — if
   you see jitter only with `sscs_enabled = true`, force-reload the
   page to bust the shader cache.

#### Shadow disappears entirely on one light

**Symptom:** a specific light's shadow vanishes; other lights are
fine.

**Diagnosis ladder:**

1. **`cast_shadows` = `false`** on the caster mesh, or
   **`receive_shadows` = `false`** on the receiver. The mesh
   inspector shows both toggles.
2. **`max_distance` shorter than receiver distance.** The light's
   shadow fades over `CASCADE_BLEND = 0.5` of the final cascade and
   goes fully lit at `max_distance`. Receivers past that distance
   are by-design unshadowed by this light.
3. **View-slot budget exhausted.** `MAX_SHADOW_VIEWS = 96`. A point
   light burns 6 view slots; a directional with 4 cascades burns 4;
   a spot 1. The 17th point light at default config silently drops
   its shadow with a console warn. Check `tracing` output for
   `shadow descriptor / view budget exhausted (point needs 1 + 6)`
   or `directional needs N`.
4. **Cube-pool slot exhausted.** Default `max_point_shadows = 8`. If
   more point lights have `cast = true` than the pool holds, the
   excess silently drops with `point-light shadow cube pool
   exhausted` in the console.

#### PCSS / soft edges look like noise / speckle

**Symptom:** the soft penumbra has a noisy "furry" or stepped texture
instead of a smooth fall-off — most visible on a point light's wide
penumbra against a high-contrast (dark-shadow / bright-floor) receiver.

**Diagnosis ladder:**

1. **Denoise off.** The edge-aware denoise blur (**Settings → Shadow
   denoise**, on by default) exists to smooth exactly this. Confirm it's
   enabled; if a player build looks noisy, check `ShadowsConfig::denoise`
   is `true`. See [Shadow denoise](#shadow-denoise-blur).
2. **Too few samples.** Raise the light's `shadow_samples` (default 16).
   Low counts on a wide penumbra are the direct cause of speckle; with
   denoise on, 16 is usually clean, but a very wide PCSS penumbra on a
   hero light may want 24–48.
3. **Kernel reaching off-tile (directional/spot).** On the 2D path,
   `pcss_penumbra_scale` can widen the variable kernel beyond the
   cascade's atlas tile, where blocker-search taps hit the tile clamp.
   Reduce `pcss_penumbra_scale` (try `1.0` and work up). Confirm with
   `debug_cascade_colors` that the receiver isn't right at a cascade
   boundary where two PCSS kernels overlap.
4. **Underlying sampler.** All kinds sample a clump-free baked Vogel disc
   (far smoother than a rotated fixed-Poisson set), so the residual after
   denoise is minimal. If you still see structure with denoise on, it's
   likely the MSAA silhouette-edge band the blur skips (see the denoise
   limitation).

#### Phantom double / mirrored shadow on point lights

**Symptom:** the shadow appears in *two* places — a real one and a
mirrored ghost on the opposite side of the caster.

**Cause:** stale shader cache from before the cube Y-flip + CW
winding fix landed.

**Fix:** force a full shader rebuild
(`cargo clean -p awsm-renderer`, then rebuild the editor). The
fix lives in [`shadows/state.rs`](../packages/crates/renderer/src/shadows/state.rs)
(search for `y_flip`) and
[`shadows/helpers.rs`](../packages/crates/renderer/src/shadows/helpers.rs)
(search for `CullMode::Front` + the cube_face front-face override).

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

## Known limits / deferred work

* **Cube `Pcss` is a real blocker-search PCSS** — the cube pool
  now exposes a second view (`cube_2d_array_view`,
  `texture_depth_2d_array`) used by the PCSS path for raw
  `textureLoad` blocker reads. The face-projection math is
  inlined at the call site (a forward-declared helper tripped a
  Dawn validation error). Penumbra width follows
  `(z_recv − z_blocker_avg) / z_blocker_avg`, mapped to a
  world-space disc radius clamped to `[10 cm, 1 m]`. Taps use the shared
  baked Vogel disc at the light's `shadow_samples` count (¾ for blocker).
* **`kernel_slack` is per-texel, not per-kernel** — the point-light
  soft/Pcss self-shadow slack (which removes "acne rings" on a flat
  floor) is scaled by the world footprint of **one** cube texel, not by
  the kernel radius. Scaling by the (up to ~1 m) kernel radius — the
  original formulation — let a wide Pcss penumbra balloon the slack past
  a genuine receiver↔occluder gap and leak light into the umbra (a lit
  hole under a floating occluder). One-texel scale fixes the
  quantization acne identically while staying far too small to bridge a
  real occluder gap. `kernel_slack` therefore reads as "texels of
  quantization to forgive" (default 2).
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
  [bind_groups.wgsl](../packages/crates/renderer/src/render_passes/shared/shared_wgsl/shadow/bind_groups.wgsl).
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
| Cascade fit (stable-fit, splits)   | [`shadows/cascade.rs`](../packages/crates/renderer/src/shadows/cascade.rs)                                                                          |
| Atlas clear-once / cube clear      | [`shadows/render_pass.rs`](../packages/crates/renderer/src/shadows/render_pass.rs)                                                                  |
| Cube Y-flip + CW winding           | [`shadows/mod.rs`](../packages/crates/renderer/src/shadows/mod.rs) (search for `y_flip`)                                                            |
| EVSM moment write + blur shaders   | [`shadows/evsm.rs`](../packages/crates/renderer/src/shadows/evsm.rs)                                                                                |
| Cascade-blend, PCF/PCSS taps, baked Vogel disc (`VOGEL_BASE`), `shadow_samples`, `kernel_slack` | [`shared_wgsl/shadow/bind_groups.wgsl`](../packages/crates/renderer/src/render_passes/shared/shared_wgsl/shadow/bind_groups.wgsl)                   |
| Per-light shadow descriptor packing (`extra_params`) | [`shadows/helpers.rs`](../packages/crates/renderer/src/shadows/helpers.rs) + [`shadows/consts.rs`](../packages/crates/renderer/src/shadows/consts.rs) |
| Shadow denoise blur (visibility)   | [`material_prep/shader/shadow_blur_wgsl/`](../packages/crates/renderer/src/render_passes/material_prep/shader/shadow_blur_wgsl) + [`render_blur`](../packages/crates/renderer/src/render_passes/material_prep/render_pass.rs) |
| Equal-resolution cascades          | [`shadows/cascade.rs::cascade_resolution`](../packages/crates/renderer/src/shadows/cascade.rs)                                                      |
| Slope-scale + constant depth bias  | [`build_shadow_pipeline`](../packages/crates/renderer/src/shadows/mod.rs) (`with_depth_bias(1).with_depth_bias_slope_scale(1.5)`)                  |
| 16.B bind-group consolidation      | [`material_transparent/bind_group.rs`](../packages/crates/renderer/src/render_passes/material_transparent/bind_group.rs)                            |

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
