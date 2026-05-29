# Renderer performance — permanent reference

This document is the durable guide to `awsm-renderer`'s
performance model: what costs what, how the per-frame pipeline
is structured, which knobs to turn, and where to look when a
profile shows regression.

If you're new here, start with §1 and §2; for in-flight tuning,
jump to §5 (tuning knobs) or §7 (diagnostic recipes).

---

## 1. Architecture at a glance

The renderer is a **visibility-buffer** pipeline (Burns & Hunt
2013; Schied & Dachsbacher 2015), not classical forward or
G-buffer-deferred. The geometry pass is the only pass that
runs the vertex shader and writes per-fragment data; shading is
a separate compute pass that reads the visibility buffer and
material data per pixel.

Frame structure (per `crates/renderer/src/render.rs::render`):

```
┌──────────────────────┐
│ GPU writes (uniforms │  transforms, materials, instances,
│ + per-frame buffers) │  skin/morph data, mesh_light_indices,
└──────────┬───────────┘  decals (opt-in), meta, camera, shadows
           │
┌──────────▼───────────┐
│ Geometry pass        │  visibility_data + barycentric +
│ (vertex + fragment   │  normal/tangent + barycentric_derivatives.
│  rasterizer)         │  Each opaque mesh is one drawIndirect (if
│                      │  features.gpu_culling) or a CPU-recorded
│                      │  draw_indexed_with_first_instance.
└──────────┬───────────┘
           │
┌──────────▼───────────┐
│ Coverage tally       │  Per-pixel atomicAdd → mesh_pixel_counts.
│ (compute)            │  Feeds MeshCoverage (one frame lag).
│                      │  Drives cheap-material LOD; the skin-skip
│                      │  consumer is currently parked (needs the
│                      │  grace-period mitigation — see §10).
└──────────┬───────────┘
           │
┌──────────▼───────────┐
│ Shadow generation    │  Per-shadow-view depth render; cube
│                      │  faces throttled by CubeFaceUpdateRate;
│                      │  EVSM moments + 2× blur for cascades.
└──────────┬───────────┘
           │
┌──────────▼───────────┐
│ Light culling        │  Clustered light grid build.
│ (compute)            │
└──────────┬───────────┘
           │
┌──────────▼───────────┐
│ Material classify    │  Per-tile shader_id bucketing AND per-pixel
│ (compute)            │  MSAA-edge detection (coverage / mat_meta /
│                      │  depth / normal). Writes per-bucket
│                      │  indirect-dispatch args + tile lists + the
│                      │  edge sample-list buffers.
└──────────┬───────────┘
           │
┌──────────▼───────────┐
│ Material opaque      │  One dispatchIndirect per shader_id. Shades
│ (compute)            │  sample 0 only — cheap fast path for the
│                      │  ~90 % of pixels classify marks as non-edge.
│                      │  Reads visibility + meta + materials,
│                      │  writes opaque_tex.
└──────────┬───────────┘
           │
   (MSAA-on, edge pixels only)
┌──────────▼───────────┐
│ Per-shader-id        │  One indirect dispatch per material shader_id.
│   edge_resolve       │  Shades the per-sample bits classify flagged
│ (compute)            │  for that bucket; sums into the per-edge
│                      │  accumulator slot. The per-pipeline SPIR-V
│                      │  knows about exactly one shader_id — no
│                      │  cross-material switch inlined.
└──────────┬───────────┘
           │
┌──────────▼───────────┐
│ Skybox edge_resolve  │  Same shape, for samples that hit no
│ (compute)            │  geometry (skybox). Material-agnostic.
└──────────┬───────────┘
           │
┌──────────▼───────────┐
│ Final blend          │  One thread per edge-pixel; weighted average
│ (compute)            │  of up to 4 accumulator slots; writes the
│                      │  resolved colour back to opaque_tex.
│                      │  Material-agnostic.
└──────────┬───────────┘
           │
   (optional)
┌──────────▼───────────┐
│ Opaque mipgen        │  Only when any visible transparent uses
│ (compute, on demand) │  transmission. Skipped otherwise.
└──────────┬───────────┘
           │
┌──────────▼───────────┐
│ Opaque → transparent │  Blit primes the transparent target
│ blit                 │  with opaque shading result.
└──────────┬───────────┘
           │
   (opt-in)
┌──────────▼───────────┐
│ Material decal       │  Per-tile classify + alpha-blend
│ (compute)            │  composite. Gated by features.decals.
└──────────┬───────────┘
           │
   (opt-in)
┌──────────▼───────────┐
│ HZB build            │  r32float mip chain, seeded from depth
│ (compute)            │  + max-reduced per mip.
└──────────┬───────────┘
           │
   (opt-in)
┌──────────▼───────────┐
│ Occlusion cull +     │  Per-instance frustum + HZB; compaction
│ compaction (compute) │  populates IndirectDrawArgs.instance_count
│                      │  for the next frame's geometry pass.
└──────────┬───────────┘
           │
┌──────────▼───────────┐
│ Lines / transparent /│  Forward passes.
│ HUD / blit / effects/│
│ display              │
└──────────────────────┘
```

Source of truth for the order: `render.rs`. Each pass has a
named `tracing` span whose timings surface via
`tracing-web::performance_layer` into
`performance.getEntriesByType('measure')` — provided the
renderer is running at the `SubFrame` tier. See
[perf-tracing.md](perf-tracing.md) for tiers and runtime knobs.

### 1a. MSAA as a separate dispatch chain, decoupled from materials

The five-pass MSAA chain in the diagram above — **classify → primary
opaque → per-shader edge_resolve → skybox edge_resolve → final_blend**
— is the renderer's most important architectural choice for both
**mobile reachability** and **long-term material extensibility**. It's
worth understanding before you reach for the tuning knobs in §5.

The naive way to MSAA a visibility-buffer renderer is to inline the
per-sample resolve into the primary opaque compute kernel: detect
edges, branch, and shade 4 samples on the edge path. That's how the
pre-Stage-3 implementation worked, and it's what every classical
forward MSAA renderer does conceptually. It produces correct output
but pays for it with **shader bloat**: every material's shading body
gets duplicated 4× inside one giant kernel, and every material has to
hand-roll its MSAA-aware control flow.

The Stage 3 architecture splits these concerns:

- **Edge detection is its own pass** (`material_classify`). Looks at
  per-sample `vis_data` / depth / `normal_tangent` from the geometry
  pass and decides "is this pixel an edge". Material-agnostic — the
  same pass works for PBR, Unlit, Toon, FlipBook, custom dynamic
  materials, and any future material that lands.
- **Primary opaque shades sample 0 only** — every material pipeline
  becomes a single-sample compute kernel. ~90 % of pixels go through
  this fast path on a typical scene.
- **Per-shader-id `edge_resolve`** is dispatched only over edge pixels
  the material owns. The per-pipeline SPIR-V knows about exactly one
  material's shading body; there's no cross-material switch table
  inlined.
- **`final_blend`** averages the per-sample contributions for each
  edge pixel and writes the resolved colour. Material-agnostic, one
  tiny kernel for the whole scene.

The wins from this split, in priority order:

#### Mobile / Android-Vulkan reachability

The non-split design produced a PBR primary-opaque compute pipeline
whose SPIR-V was large enough that Android Chrome's Dawn/Vulkan
backend rejected it at init with `VK_ERROR_INITIALIZATION_FAILED`.
**The renderer would not start on Android at all.** Splitting the
4-sample resolve into a separate per-shader-id pipeline shrinks each
primary-opaque pipeline's SPIR-V by ~80 %, which is what unblocks
Android-Vulkan init. The full PBR resolve still happens — it just
lives in a pipeline whose SPIR-V budget is small enough on its own.

This is the headline reason Stage 3 exists. The fact that it also
gives the architectural wins below is bonus.

#### Material authors don't write MSAA code

A material author — first-party or runtime-registered dynamic — writes
**a single-sample shading function**. They don't see `msaa_sample_count`,
they don't query sample state, they don't write edge-detection logic.
The shading function runs in both contexts (primary opaque shades
sample 0; edge_resolve calls the same function once per set sample bit
on edge pixels). The framework drives the per-sample loop.

The dynamic-material `contract-opaque.md` codifies this **dual-context
invariant**: the same authored WGSL fragment runs in both call sites,
and the framework guarantees both produce a correct result without the
author knowing which one is calling. **Dynamic materials registered at
runtime get MSAA for free.**

#### Adding new materials doesn't recompile existing ones

Each material's shading body lives in its own per-shader-id pipeline.
Adding a new dynamic material is *one new small pipeline* (primary
opaque + edge_resolve variant), not a recompile of a single giant
multi-material kernel. This matters most when:

- The editor hot-reloads a custom material — only the affected
  shader's pipelines recompile.
- A game registers 10+ runtime materials — the per-pipeline cost stays
  bounded; first-party PBR / Unlit / Toon are unaffected.
- The pipeline cache hit rate stays high across scenes that use
  different material mixes.

#### MSAA is independently extensible

The edge-detection signals live in one file
(`material_classify_wgsl/compute.wgsl`). The current gate is
`seen_count >= 2u || any_mesh_differs || depth_edge || neighbor_edge`.
Adding a 5th signal (e.g. velocity-based detection for temporal AA)
is a one-line gate addition + however much texture-load wiring the
signal needs. Material shaders stay oblivious.

The four trustable signals at the gate today, with thresholds taken
from main's pre-Stage-3 `msaa_resolve_samples` reference:

| Signal | Catches |
|---|---|
| `seen_count >= 2u` | Multi-shader-id silhouette (e.g. PBR pixel adjacent to UNLIT pixel within one tile) |
| `any_mesh_differs` | Mesh-vs-mesh silhouette where two distinct meshes touch in screen-space |
| `depth_edge` | Same-mesh in-pixel silhouettes — e.g. the platform's top/front-face seam in MorphStressTest |
| `neighbor_edge` | Coverage + normal + depth check on the 4 cardinal neighbours — catches silhouettes that run between pixels and tile-facet boundaries with rotated normals |

#### Runtime cost is at parity with the inlined design

The per-pixel total GPU work is essentially unchanged. Non-edge pixels
do less work (no inline branch + only sample-0 shading); edge pixels
do the same work split across more dispatches. The dispatch-count
overhead (`classify → opaque → per-shader edge_resolve × N → skybox
edge_resolve → final_blend`) is ~5 extra GPU work-graph nodes per
frame — tens of microseconds at most, well inside the existing
per-frame budget.

For numbers, see [`edge-resolve-baseline.md`](edge-resolve-baseline.md)
which captures the Fox / 1080p / MSAA-4 baseline at mean 16.68 ms /
p99 17.6 ms (vsync-aligned).

#### What the architecture costs

- One extra MSAA-only compute pass (`classify` does double duty for
  bucketing AND edge detection; this is unconditional). On a non-MSAA
  configuration the edge-detection legs are template-gated out, so
  the cost is 0.
- One extra storage texture bind in classify (`normal_tangent_tex`,
  group 0 binding 9) — read-only re-use of the multisampled
  normal/tangent the geometry pass writes anyway, no new allocation.
- Several extra GPU dispatches per frame, dominated by indirect-args
  setup overhead. Negligible at typical scene complexity.

---

## 2. Opt-in features

`RendererFeatures` (`crates/renderer/src/features.rs`) gates always-on
infrastructure. **Boolean fields default to `false`**, so library
consumers pay zero overhead for features they don't use. Game-side
and editor builds opt in explicitly. Capability-tied features use
`FeatureToggle::Auto` (capability-detect at device creation) by
default — see [§2.1](#21-feature-toggles-vs-bool-fields) below.

```rust
RendererFeatures {
    gpu_culling: bool,                       // HZB + occlusion cull +
                                             // compaction + drawIndirect
    decals: bool,                            // projection-decal classify,
                                             // compute, composite +
                                             // ~33 MB GPU at 4K
    coverage_lod: bool,                      // per-mesh pixel-coverage
                                             // producer (consumers parked)
    indirect_first_instance: FeatureToggle,  // see §2.1
}
```

### 2.1 Feature toggles vs bool fields

A `bool` field means "the consumer decides whether to allocate this
subsystem at all." A `FeatureToggle` (`Auto` / `On` / `Off`) wraps a
*capability* that may or may not be available on the target device:

- **`Auto`** (default): probe the adapter at device-creation time.
  Resolves to `true` when the underlying WebGPU feature is exposed,
  `false` otherwise.
- **`On`**: assume the feature is present; bypass the probe. Useful
  for testing the optimized path on a device where Auto's probe is
  misbehaving, or for forcing engagement in a benchmark harness.
- **`Off`**: assume the feature is absent. Forces the portable
  fallback path even on a device that supports the optimized one —
  useful for testing the fallback path on your dev machine, or for
  side-stepping a driver bug.

The renderer carries **both** code paths for any toggle-gated
feature; neither is a "degraded" mode. Both are independently
optimized for their respective device class.

#### `indirect_first_instance`

Controls how the non-instanced geometry pass passes the per-mesh
"which slot in the meta buffer am I?" identity to the vertex shader:

- **Toggle resolves to true** (`indirect-first-instance` available):
  one shared storage-array binding services every non-instanced
  draw. The compaction shader writes the per-mesh slot index into
  `IndirectDrawArgs.first_instance`; the vertex shader reads
  `geometry_mesh_metas[@builtin(instance_index)]`. No per-draw
  `setBindGroup` cost. Requires the WebGPU `indirect-first-instance`
  feature on the device.
- **Toggle resolves to false** (portable fallback): the non-instanced
  path uses the same uniform-with-dynamic-offset binding the
  instanced path uses. CPU calls
  `setBindGroup(2, meta_group, &[meta_offset])` before each
  `drawIndexedIndirect`. The args buffer's `first_instance` stays at
  0; compaction templates out the slot-index write. GPU culling
  itself is preserved (compaction still writes `instance_count`).

Browser support for `indirect-first-instance` is narrow as of
mid-2026 — Firefox: none; Chrome desktop: Linux-Intel only — so most
player devices in shipped games will hit the portable path. Both
paths must stay first-class and benchmarked.

Measured overhead when both are on (Claude Preview MCP,
120-frame mean):

| Scene | both-off `Render` | both-on `Render` | Δ |
|---|---|---|---|
| empty editor | 0.395 ms | 0.524 ms | +0.129 ms |
| `tuning-1k-meshes` | 1.637 ms | 1.645 ms | +0.008 ms (drawIndirect recovered the per-mesh `set_bind_group`) |
| `tuning-64-lights` | 0.663 ms | 0.790 ms | +0.127 ms |
| `tuning-10k-meshes` | 1.807 ms | 1.912 ms | +0.105 ms |

When off:

- No `HZB` / `Occlusion Cull` / `Occlusion Compaction` /
  `Material Decal` tracing spans fire at all.
- `decal_color` (16 MB at 4K) and `decal_classify_buffers`
  (~17 MB at 4K) are not allocated.
- Lazy-grow HZB / occlusion / compaction buffers stay at 1×1
  initial capacity.

When on, the GPU-driven culling pipeline becomes a 30–50%
frame-time win at the 10K-mesh tier once the visible set is
small. Below ~500 meshes it nets to a small loss (the always-on
cull dispatch + per-frame CPU upload outweigh the few saved
draws). The adaptive policy below handles this automatically —
keep `gpu_culling = true` at the capability layer and let the
policy disengage on small scenes.

`insert_decal()` returns `AwsmDecalError::FeatureNotEnabled`
when `features.decals = false`. Misuse fails loud rather than
silently dropping decals.

In debug builds the scene-editor honors `?features=off` as a
URL switch for A/B measurement. Release builds skip the URL
parse entirely.

### Adaptive policy (`RendererOptimizationPolicy`)

`RendererFeatures` decides whether the GPU-driven *resources* exist;
`RendererOptimizationPolicy` (`optimization_policy.rs`) decides whether
to *engage them this frame*. The two layers exist because the
always-on cull + compaction + drawIndirect path costs more than it
saves on small scenes, but reallocating the buffers on every
threshold flip would be worse than the win it buys.

```rust
RendererOptimizationPolicy {
    gpu_culling: OptimizationMode::Auto,    // Off / Auto / Force
    gpu_culling_enable_threshold: 800,      // engage at >= this opaque count
    gpu_culling_disable_threshold: 500,     // disengage below this
    gpu_culling_cooldown_frames: 30,        // min frames per mode before another flip
}
```

`Auto` mode uses hysteresis (separate enable / disable thresholds)
and a cooldown to keep the mode stable: a scene oscillating around
600 visible meshes won't ping-pong the path on every frame. `Force`
keeps the GPU path engaged regardless of scene size — editor builds
use this so authoring exercises the pipeline. `Off` parks it for the
session, but HZB still rebuilds when decals are active (`decals` use
the same texture).

Per-frame the policy lowers to a `FrameOptimizations { gpu_occlusion,
indirect_geometry, hzb, decal_hzb_gate }` struct on `RenderContext`.
Call sites consult `ctx.frame_optimizations.get()` rather than the raw
features for runtime branching.

**Args-ready poisoning.** When `gpu_occlusion` flips from `true` to
`false` the renderer clears `compaction_buffers.args_ready`, so a
later re-enable warms up through one frame of CPU-recorded geometry
before drawIndirect resumes — no stale-args window.

`compute_frame_optimizations(policy, stats, prev, frames_in_mode)` is
pure and has 10 unit tests in `optimization_policy.rs`. To retune
thresholds for a specific deployment: set them on the builder via
`with_optimization_policy`, or at runtime via
`AwsmRenderer::set_optimization_policy` (mode flips reset the cooldown
so a Force → Auto / Auto → Off transition takes effect immediately).

---

## 3. The visibility-buffer model — why it matters for perf

Two practical implications shape everything else in this doc:

1. **The geometry pass is intentionally cheap.** It only writes
   visibility data + barycentrics + normal/tangent. No material
   evaluation, no lighting. Adding work here regresses every
   frame; do it only when the data is needed by *all* downstream
   passes.

2. **Material classify + per-tile dispatch keeps shading focal.**
   The opaque compute pass runs N dispatches (one per active
   `MaterialShaderId`), each indirect-dispatched over the
   `material_classify` pass's per-tile bucket lists. A pixel
   that doesn't belong to that tile's shader never runs that
   shader. Adding a 4th opaque shader variant means adding a
   classify bucket; the classify shader's bitmask is hard-coded
   for 3 today (PBR, Unlit, Toon).

3. **Skybox ownership rule.** The PBR pipeline handles skybox
   pixels (`triangle_index == U32_MAX`). Non-PBR pipelines
   early-return on skybox so a mixed-material tile shaded by
   Unlit + skybox doesn't double-write the skybox pixels. A new
   opaque variant either keeps this rule or declares its own
   dedicated skybox slot.

---

## 4. Per-frame budget

Span names appear in `performance.getEntriesByType('measure')`
when the renderer is running at the `SubFrame` tier — the default
in debug builds, opt-in with `?trace=sub-frame` in release. The
shipping default (`Frame` tier) emits only the outer `Render`
span. See [perf-tracing.md](perf-tracing.md).

Typical 4K viewport, scene-editor with both features on, no
decals authored, modest mesh count:

| Span | Typical |
|---|---|
| `Render` (wraps everything) | 0.4–2.0 ms |
| `Geometry RenderPass` | 0.04–0.15 ms |
| `Material Classify RenderPass` | ~0.01 ms |
| `Material Opaque RenderPass` | 0.005–0.015 ms |
| `Coverage RenderPass` | < 0.01 ms (compute + copyBuffer) |
| `HZB RenderPass` | ~0.02 ms |
| `Occlusion Cull RenderPass` | 0.01–0.03 ms |
| `Occlusion Compaction` | 0.005–0.015 ms |
| `Material Decal RenderPass` | ~0.003 ms (empty decal set) |
| `Shadow Generation` | 0.6–1.1 ms (10-20 shadow casters) |
| `Light Culling RenderPass` | < 0.01 ms |
| `Display RenderPass` | < 0.02 ms |

Numbers are headless-Chrome timings via the Claude Preview MCP;
real macOS / Linux desktops with discrete GPUs typically beat
these by 2–5×, mobile WebGPU by a similar factor slower.

`Shadow Generation` dominates when many lights cast shadows.
Cube faces are 6× the cost of a 2D shadow, so a point light is
much more expensive than a spot or directional. See §5's
"shadow knobs" for caps.

---

## 5. Tuning knobs

### Renderer construction (set once)

```rust
AwsmRendererBuilder::new(gpu)
    .with_features(RendererFeatures {
        gpu_culling: true,
        decals: true,
        coverage_lod: false,
        indirect_first_instance: FeatureToggle::Auto,
    })
    .with_shadows_config(ShadowsConfig { ... })
    .with_anti_aliasing(AntiAliasing { msaa_sample_count, mipmap })
    .build()
    .await?;
```

| Knob | Where | Default | Effect |
|---|---|---|---|
| `RendererFeatures::gpu_culling` | features.rs | `false` | Enables HZB + occlusion cull + drawIndirect. Worth it ≥ 500-mesh scenes; small net loss below that. |
| `RendererFeatures::decals` | features.rs | `false` | Allocates ~33 MB at 4K. Required for `insert_decal`. |
| `RendererFeatures::coverage_lod` | features.rs | `false` | Allocates the per-mesh-pixel-coverage producer + readback buffer. Consumers (skin-skip, cheap-material LOD) are currently parked. |
| `RendererFeatures::indirect_first_instance` | features.rs | `FeatureToggle::Auto` | Resolves at device creation. `On` requires the WebGPU feature; `Off` forces the portable uniform-with-dynamic-offset path. See §2.1. |
| `ShadowsConfig::atlas_size` | shadows/config.rs | 4096 | 2D shadow atlas. Memory = `size² × 4 bytes`. Per-light shadow resolutions max out at this. |
| `ShadowsConfig::cascade_resolution` | shadows/config.rs | 2048 | Directional-cascade layer dimensions. `cascade_count × resolution² × 4 bytes × max_layers` for the cascade array texture. |
| `ShadowsConfig::cascade_array_max_layers` | shadows/config.rs | 16 | Maximum directional-cascade layers across all directional lights × cascades. |
| `ShadowsConfig::max_point_shadows` | shadows/config.rs | 8 | Cube-array slots available for point lights. Excess point lights silently skip shadow casting. |
| `ShadowsConfig::point_shadow_resolution` | shadows/config.rs | 1024 | Cube-face dimensions. Cube pool memory = `6 × max_point_shadows × resolution² × 4 bytes`. |
| `ShadowsConfig::evsm_atlas_size` | shadows/config.rs | 2048 | EVSM moment-write atlas (directional cascades only). |
| `AntiAliasing::msaa_sample_count` | anti_alias.rs | `Some(4)` | 4× MSAA on visibility-buffer + transparent target. `None` skips MSAA entirely. |
| `AntiAliasing::mipmap` | anti_alias.rs | varies | Mipmap-derivatives mode for the visibility decode. |
| `default_cheap_material_pixel_threshold` | lib.rs | 64 | Global default for `Mesh::cheap_material_pixel_threshold`. Override per-frame from your quality system if you want tier-tied behaviour. |

### Per-light shadow knobs (`shadows::LightShadowParams`)

```rust
LightShadowParams {
    cast: bool,                                    // master toggle
    resolution: u32,                               // 256–2048 typical
    hardness: LightShadowHardness::Hard | Soft | Pcss,
    pcss_penumbra_scale: f32,                      // PCSS only
    cascade_count: u8,                             // 1..=4, directional only
    cascade_split_lambda: f32,                     // 0=uniform, 1=log
    evsm_cutoff: EvsmCutoff,                       // which cascades use EVSM
    far_cascade_update_rate: FarCascadeUpdateRate, // throttle far cascade
    cube_face_update_rate: CubeFaceUpdateRate,     // throttle cube faces
    // ...
}
```

`ShadowQualityTier::{Low, Medium, High, Ultra, Custom}` (via
`apply_to_light_params`) packages these into preset combinations.

`AwsmRenderer::refresh_light_importance_budgets()` is the
importance-tier auto-assigner — score = `intensity / (1 + dist²)`,
cutoffs `> 10.0` → Ultra, `> 1.0` → High, `> 0.05` → Medium,
else Low. Directionals pin to High. Call on a slow tick (every
10–30 frames) — coarse signal, churning the shadow allocator
every frame is wasted work.

### Per-mesh knobs (`meshes::Mesh`)

```rust
Mesh {
    cast_shadows: bool,                       // appears in shadow gen
    receive_shadows: bool,                    // samples shadow maps
    receive_decals: bool,                     // decal compute affects
    cheap_material_key: Option<MaterialKey>,  // distance LOD swap (live; see §5e)
    cheap_material_pixel_threshold: Option<u32>, // None → renderer default
    skin_update_period: u8,                   // 1=every frame, 2=half, etc.
    billboard_mode: BillboardMode,            // camera-facing override
    // ...
}
```

`AwsmRenderer::set_mesh_skin_update_period_by_distance` lets the
caller distance-LOD skinning frequency at a stroke.

**Coverage-driven skin-skip** is fully wired. The
`Meshes::update_world` path now layers two gates on top of the
cadence one (`skin_update_period`):

1. *Grace period* — a skin's per-frame zero-coverage counter
   resets to 0 the moment any consumer mesh shows coverage > 0,
   and increments otherwise. Only when the counter clears
   `SKIN_COVERAGE_GRACE_FRAMES` (default 2) does the skin
   become eligible for skipping. The grace dodges the
   "rest-pose pop-in" hazard on multi-primitive characters
   (e.g. BrainStem's 59 primitives sharing one skeleton)
   where a submesh briefly self-occludes.
2. *BVH-visible override* — if any consumer mesh's
   `world_aabb` is inside the camera frustum, the skin keeps
   animating regardless of coverage. The frustum check uses
   the BVH-built `Frustum::intersects_aabb`, so a skin
   re-entering the frustum resumes animation that same frame.

The two gates *compose* with `skin_update_period`: a
`period = 4` skin that's also fully out-of-frustum runs
*never*. A `period = 1` skin in-frustum with zero coverage
keeps running. The skip only fires on the intersection of:
"period allows" ∧ "coverage = 0 for > grace frames" ∧ "no
consumer mesh in frustum".

### Scene spatial config (`scene_spatial::SceneSpatialConfig`)

| Knob | Default | When to bump |
|---|---|---|
| `rebuild_dirty_threshold` | 200 | Per-frame mesh insert/remove churn pushes rebuild cadence; at 10K+ static meshes bump to ~2000. |
| `rebuild_period_frames` | 600 | Time-based rebuild cap. At 10K+ static meshes bump to ~1800. |

Surface a builder method (`with_scene_spatial_config`) only if
the target scene exceeds ~1K dynamic meshes; the defaults handle
the 1K–5K range fine.

### Oversized-mesh light-bucket knobs (`light_buckets::buckets`) — deprecated for shading

| Knob | Default | Notes |
|---|---|---|
| `OVERSIZED_LIST_COUNT_THRESHOLD` | 16 | (vestigial) bucket-depth gate. |
| `OVERSIZED_AABB_DIAGONAL_METERS` | 50.0 | (vestigial) mesh-size gate. |

> **No longer used for shading-path routing.** All surfaces now use the
> froxel path unconditionally (see §5h history note), so the oversized
> classification no longer decides anything for lighting. These constants
> + the classification are pending removal once the per-mesh light-list
> build is untangled from the shadow-receiver rebuild.

---

## 5b. Per-frame upload path — `MappedStagingRing` + `MappedUploader`

Renderer-owned per-frame uploads (transforms, materials, instances,
meshes meta, skins, morphs, texture-transforms, the three mesh pool
buffers) flow through a **mapped staging ring** instead of
`queue.writeBuffer`.

- Each migrated call site owns a [`MappedUploader`][mu] companion
  alongside its existing CPU-side `DynamicStorageBuffer` /
  `DynamicUniformBuffer` and destination `GpuBuffer`.
- On `write_gpu`: the uploader acquires the next slot of its internal
  [`MappedStagingRing`][msr] (default depth 3,
  `MAP_WRITE | COPY_SRC`, `mappedAtCreation: true`), `memcpy`s the
  dirty ranges into the mapped `ArrayBuffer`, `unmap()`s, records
  `copyBufferToBuffer(slot → dest)` for each range into a
  per-upload command encoder, and submits.
- Once a slot has been submitted the uploader auto-kicks `mapAsync`
  on the oldest still-`Submitted` slot so its bytes are ready by the
  time the cursor wraps back. `spawn_local` + a shared
  `Arc<AtomicBool>` flag in the slot promotes `Pending → Ready` on
  the main thread (renderer-wide convention: shared interior
  mutability uses `Arc` + atomics / `Mutex` so the same types
  compile unchanged the day a subsystem moves across threads).
- On dest-buffer growth: the ring rebuilds at the new size in one
  shot (live `Mapped` slots are explicitly `unmap`ped to keep
  validation quiet; in-flight slots ride their `GpuBuffer`
  destructor). The first post-resize frame falls back to
  `queue.writeBuffer` since the dest contents are uninitialised
  anyway.
- On ring exhaustion (debug build): `debug_assert!` so depth bugs
  surface in development. Release: silently falls back to
  `queue.writeBuffer` and bumps `fallback_count`.

`queue.writeBuffer` stays as the canonical path for foreign-bytes
ingestion — glTF parse output, raster bitmap decode results,
worker-job payloads. The mapped path doesn't help there because the
source bytes already live in a JS `ArrayBuffer` / Rust `Vec` and the
memcpy is the same either way. Each `MappedUploader` exposes an
`ingest_foreign(..)` entrypoint for this so call sites use a
documented method instead of reaching for raw `gpu.write_buffer`;
those bytes count against `bytes_uploaded_via_writebuffer`
(separate from the ring's `bytes_uploaded_via_fallback`).

Telemetry is surfaced via the
[`read_upload_ring_stats()`](#9-measurement-harness) wasm export.
Expected steady-state on `tuning-10k-meshes`:
`_total.fallback_count` settles at 1-2 after the cold-start frame,
`peak_ring_depth_used == 3` (full ring rotation), `resize_count == 0`
after the initial scene fill-in. See §5d for the captured numbers.

**Every** per-frame `queue.writeBuffer` site in the renderer crate
routes through `MappedUploader`. Migrated sites:

- `DynamicStorageBuffer` / `DynamicUniformBuffer` clients:
  `transforms`, `materials`, `instances` ×2, `meshes.meta` ×2,
  `skins` ×2, `morphs` ×2, `textures.transforms`, the three mesh
  pool buffers.
- Direct `writeBuffer` clients: `camera` (64 B uniform), `shadows`
  (globals + descriptors + views), `lights` (punctual + info),
  `mesh_light_indices`, `occlusion` (params + instance pack),
  `lines` (per-line uniform + per-line segment).

The only `queue.writeBuffer` calls left are the explicit
foreign-bytes ingestion path (`MappedUploader::ingest_foreign`,
used by glTF buffer + texture upload) and the per-frame reset
writes (`coverage_buffers.reset_counts`,
`material_classify_buffers.reset_header`,
`decal_classify_buffers.reset_counts`) — the latter are full-replace
of small fixed-content payloads (zeros / static headers) where the
ring's mapped-write win doesn't apply.

[mu]: ../crates/renderer/src/buffer/mapped_uploader.rs
[msr]: ../crates/renderer/src/buffer/mapped_staging_ring.rs

---

## 5c. Worker-mode gltf parse — default in the editor

`GltfParseJob` runs the full fetch + parse pipeline on a pool worker
**AND decodes every embedded image into an `ImageBitmap` inside the
worker** via the `DedicatedWorkerGlobalScope::createImageBitmap`
shim. Every cross-thread payload is transferred (not structured-
cloned) across the `postMessage` boundary using the trait hooks
[`WorkerJob::into_response_message`][wj_into] / [`WorkerJob::from_response_message`][wj_from]:

- **`ImageBitmap` handles** — attached to the response object's
  `bitmaps` array and pushed into the
  `post_message_with_transfer` transfer list. Main thread
  receives them in O(1) and [`GltfParseOutput::into_loader`][gp_il]
  skips its decode step entirely.
- **`doc_bytes` + `buffer_bytes`** (zero-copy byte transfer) —
  re-emitted glTF JSON and the
  per-buffer-view binary payloads are moved into freshly-allocated
  JS-heap `Uint8Array`s on the worker side and their underlying
  `ArrayBuffer`s are added to the same transfer list. The previous
  `#[serde(with = "serde_bytes")]` path went through serde-
  produced `Uint8Array`s that then paid a structured-clone copy on
  the postMessage hop; the explicit transfer detaches the buffers
  worker-side and re-attaches them main-side without copying. One
  memcpy per direction (Rust `Vec<u8>` → JS heap on the worker,
  `Uint8Array::to_vec` back into wasm linear memory on the main
  thread); the cross-thread hop itself is free.

[wj_into]: ../crates/renderer/src/workers/pool.rs
[wj_from]: ../crates/renderer/src/workers/pool.rs
[gp_il]: ../crates/renderer-gltf/src/worker_job.rs

A/B measurement on Corset.glb (12.8 MB, the heaviest single-asset
glb in the shipped samples), n=3 iterations on M2 MacBook /
Chrome (pre-zero-copy baseline):

| Path | Mean load (ms) | Speedup |
|---|---|---|
| Inline `GltfLoader::load` | 196 ms | 1.0× (baseline) |
| Worker `GltfParseJob` → `into_loader()` | **91 ms** | **2.15×** |

The worker path is **2.15× faster** end-to-end against inline. The
flip comes from moving image decode to the worker — what used to be
a main-thread `createImageBitmap` bottleneck (~150 ms on Corset) is
now zero main-thread time because the bitmaps arrive pre-decoded via
the transfer list. Zero-copy byte transfer adds a smaller increment
on top: the worker no longer pays the ~12 MB structured-clone hop
for `doc_bytes` + `buffer_bytes`. On Corset that's a low single-
digit-ms win; on a 50 MB asset it scales to ~50 ms (linear in
payload size). The 12.8 MB Corset is the largest asset shipped in
this repo's stress dir, so the bigger-asset numbers stay a
hypothesis until a real consumer drives one through.

Re-measured on the Claude Preview MCP (headless Chrome), n=5:
inline 74.7 ms / worker 69.9 ms = **1.07×**. The
absolute speedup compresses because the headless environment's
main-thread `createImageBitmap` is already much faster than M2 real
Chrome — the bottleneck the worker path eliminates is smaller, so
the *relative* gap is smaller. Both runs improved (75 ms vs the M2
baseline's 196 ms for inline, 70 ms vs 91 ms for worker) reflecting
the headless decoder's raw speed; the worker-mode win is bounded by
how much main-thread image decode there was to remove. Real-Chrome
M2 numbers stay the canonical baseline because they're closer to
what a shipping browser user pays.

For smaller assets (DamagedHelmet, ~4 MB, 5 textures): the
break-even point is around 5 MB. Below that, inline and worker
land within noise of each other — the worker spawn + dispatch
overhead matches the savings. The scene-editor's pre-warmed pool
(see below) makes the *first* small-asset load break even too —
the on-demand spawn cost no longer falls on its critical path.

### Editor uses worker mode by default

The scene-editor's `asset_cache::load_and_populate` dispatches
through the worker path. Two structural pieces make the default
viable:

1. **Pre-warmed pool at editor init.** `context.rs::maybe_build_worker_pool`
   constructs `WorkerPool::new(WorkerPoolBootstrap::Auto, 2)` during
   `create_context` — the same await sequence that builds the
   renderer. Workers come up in parallel with shader compile; by the
   time the user can issue Insert Model the pool is ready and the
   first dispatch is a direct `pool.dispatch::<GltfParseJob>(input)`
   call (no ~50 ms on-demand build).
2. **Sticky graceful fallback.** `WorkerPoolHandle =
   Arc<Option<WorkerPool>>`: if the bootstrap fails (CSP that
   blocks blob URLs, ad-blockers nuking the worker shim, no
   resolvable `import.meta.url`) the field stays `None` for the
   session and `asset_cache::load_and_populate` routes through
   the canonical inline `GltfLoader::load` path. The failure is
   surfaced once at boot via `tracing::warn!` so a CSP
   misconfiguration shows up immediately in the dev console;
   pool construction never retries in-session.

Dev-only `?gltf-worker=off` URL knob forces the inline path
(measurement harness's A/B baseline, smoke-testing the fallback
without misconfiguring CSP). `?gltf-worker=on` is accepted as a
no-op for backward compatibility.

Pool size defaults to 2 workers. Rationale: the editor's common
case is one asset load at a time (drag-drop one glb at a time,
project-open serialises assets), so 1 would technically be enough;
2 keeps a spare for the occasional parallel dispatch (multi-asset
import, the measurement harness) without burning RAM on workers
that never see load. `WorkerPool::with_workers(None)` would clamp
to `min(hardware_concurrency, 4)`, which on a 16-core dev box
parks 4 workers permanently.

### Shipping a game — still build your own pool

A library consumer (a shipping game) doesn't inherit the editor's
pool — the editor's `context.rs::create_context` is what builds
it. Production consumers should mirror the editor's shape: kick
`WorkerPool::with_workers(Some(N))` during their splash, register
`GltfParseJob`, and route asset loads through it. See
[§8a step 1](#1-pre-warm-the-worker-pool-for-mid-gameplay-asset-loads)
for the snippet + sizing guidance.

### Unsupported formats fail fast

`createImageBitmap` rejects unsupported formats (KTX2, Basis,
etc.). The worker treats rejection as fatal and propagates the
error up out of the dispatch (`anyhow::Context` annotates the
mime type + URI so the caller knows which entry broke). The
earlier "carry encoded bytes, main thread re-decodes" fallback
was theatre: `GltfParseOutput::into_loader`'s main-thread
fallback used the exact same `createImageBitmap` shim, so a
format the worker browser rejected would fail identically on
the main thread after a bytes round-trip — pure overhead.
Decision rationale and the design note for a future Rust-side
decoder (e.g. `image` crate basis support behind a feature
flag) live in [`crates/renderer-gltf/src/worker_job.rs`][gp]'s
`import_image_data` doc.

[gp]: ../crates/renderer-gltf/src/worker_job.rs

[mu]: ../crates/renderer/src/buffer/mapped_uploader.rs

---

## 5e. Cheap-material LOD routing — live

`Mesh::effective_material_key(mesh_key, coverage, default_threshold)`
resolves the cheap variant on every mesh that has
`cheap_material_key.is_some()` AND last-frame coverage below
`cheap_material_pixel_threshold` (per-mesh override; falls back to
the renderer's `default_cheap_material_pixel_threshold`, default 64
px). [`Meshes::refresh_cheap_material_routing`][cm_refresh] is
called once per frame from `AwsmRenderer::render` right after
`coverage.ingest` and before `meshes.meta.write_gpu`; it walks
every mesh with a cheap variant, compares the effective key against
a `SecondaryMap<MeshKey, MaterialKey>` cache of the last-frame
value, and patches `MaterialMeshMeta.material_offset` only on the
meshes that actually crossed the threshold. Steady-state writes
are O(0) when nothing changed — the cache short-circuits.

[cm_refresh]: ../crates/renderer/src/meshes.rs

### Compatibility constraint — enforced at set time

`AwsmRenderer::set_mesh_cheap_material(mesh_key, Some(cheap_key), …)`
rejects with `AwsmMeshError::IncompatibleCheapMaterial` when the
cheap material differs from the authored material in either:

- `MaterialShaderId` (Pbr / Unlit / Toon) — different shader_id
  means a different opaque-compute pipeline; the per-frame swap
  doesn't migrate pipeline keys.
- `is_transparency_pass()` classification — cross-pass cheap
  variants would land in the wrong renderable pool.

A mismatched pair is a programmer error rather than a silent
"my cheap variant doesn't kick in." Same-shader-id +
same-transparency cheap variants (e.g. PBR-with-textures →
PBR-flat-colour, or PBR-opaque → PBR-opaque-no-normal-map) cost
exactly one 4-byte GPU write per threshold transition.

### When to author a cheap variant

For meshes that render at small pixel coverage (props in the
distance, particles, ambient debris). The pixel-coverage gate is
GPU-measured — set the threshold to match the screen-space size
below which the cheap variant becomes visually equivalent. Typical
values: 64 px for hero props, 16 px for ambient props, 256 px for
characters (the threshold reflects how much pixel detail your
cheap shader can preserve, not raw screen-space size).

---

## 5f. Shadow optimisations — coverage gate + PCSS variable taps

Two extensions to the shadow path, both live in [`render_passes/shared/shared_wgsl/shadow/bind_groups.wgsl`][bgw] and the receiver-side WGSL in [`lights.wgsl`][lwgsl]:

[bgw]: ../crates/renderer/src/render_passes/shared/shared_wgsl/shadow/bind_groups.wgsl
[lwgsl]: ../crates/renderer/src/render_passes/shared/shared_wgsl/lighting/lights.wgsl

### Coverage-driven shadow-receiver gate

`MaterialMeshMeta` carries a per-mesh `shadow_receiver_gate: u32`
that's bitwise-ANDed with the authored `receive_shadows` flag at
every `apply_lighting*` call site. The gate is patched once per
frame in `AwsmRenderer::update_transforms` after
`LightMeshBuckets::mark_shadow_receivers` from
`light_buckets.is_shadow_receiver(mesh_key)`. A `SecondaryMap<MeshKey, u32>`
cache inside `MeshMeta` short-circuits unchanged writes — on a
steady-state 10k-mesh scene, the dirty-range set stays sparse and
the mapped-buffer ring uploads only actual transitions.

A mesh that no shadow-caster reaches this frame skips the entire
`sample_shadow_*` invocation chain — that's the shadow atlas
sample plus the PCSS blocker search plus the variable-kernel PCF.
On a scene with one directional shadow caster and 10k meshes
mostly outside its frustum, the gate cuts the shading-time
shadow-sample work to near-zero for the unreachable cohort.

Per-frame cost on tuning-10k-meshes: the "Shadow Receiver Gate"
span shows **0.048 ms mean / 0.1 ms p95** — well under the
~0.1 ms it saves the geometry pass.

### PCSS / Soft kernels — all 16-tap fixed

The three PCSS branches (cube `sample_shadow_cube`, directional
`sample_shadow_cascade_array`, 2D spot
`sample_shadow_descriptor`) and the Soft (hardness < 1.5)
branches all run at a fixed 16-tap rotated Poisson kernel.

Why not vary the tap count by receiver distance? A distance-keyed
taper looks like the obvious optimisation — far receivers get
narrower penumbras and shouldn't need 16 taps — but the directional
taper key available cheaply at shading time (`ndc.z`) is
**uncorrelated with penumbra width**. Wide kernels keyed by
distance got too few samples, and the rotated-Poisson disc
rendered as visible ribbons rather than smooth penumbra. Any
future tap-budgeting work has to derive its key from the blocker
search's actual penumbra estimate, not from receiver distance.

Implementation pattern (kept identical across the four call sites
so the safety reasoning carries):

```wgsl
let tap_count = pcss_tap_count(dist_ratio);  // u32 in [1, 16]
var sum = 0.0;
for (var i = 0u; i < 16u; i = i + 1u) {  // static loop — Poisson table size
    if i >= tap_count { break; }         // dynamic early-exit
    ...
    sum += textureSampleCompareLevel(...);
}
return sum / f32(tap_count);
```

The static-bounded `for` plus dynamic `break` preserves WGSL
backend stability — drivers happily unroll the static loop and
hoist the bounds check. The runtime tap count drives both the
blocker search AND the variable PCF, so the "all blockers" /
"no blockers" early-exits stay aligned with the smoothing kernel.

**Per-light tunables**: configured globally via
`ShadowsConfig::pcss_max_taps` / `pcss_min_taps`. Values above 16
silently clamp (the Poisson table holds 16 samples); values below
1 clamp to 1 to avoid divide-by-zero. The defaults give a 4× cost
reduction on far receivers vs the previous fixed-16 path; bump to
8/16 for "everything sharp" or drop to 4/16 for "even cheaper far
shadows" on mobile.

---

## 5h. Lighting & light culling

This is the durable reference for how the renderer decides *which lights
touch which surfaces*. The guiding principle: **never loop all lights on
a fragment**. Punctual lights use a single **clustered (froxel)** path
for every surface; directional lights use a small global prefix. (This
unifies what used to be a per-mesh-CPU-bucket path + an "oversized" froxel
fallback gated on hard-to-tune `AABB>50m AND bucket>16` heuristics — see
the history note below.)

### Light types and the global path

- **Directional lights** have infinite extent, so they're never culled.
  The CPU packs the (≤ 8) directional lights' indices into a small list in
  the `lights_info` uniform (`data.w` = count, `directional[]` = indices);
  every shaded fragment walks just that list via `get_n_directional()` /
  `get_directional_light_index()`. This avoids scanning all `n_lights` per
  pixel to find the few suns (catastrophic with hundreds of punctuals).
- **Punctual lights** (point + spot) have a finite range sphere and are
  the subject of culling. Hard cap: `MAX_PUNCTUAL_LIGHTS = 1024`.

### The 1024 cap is deliberate (and load-bearing)

`lights` is a **uniform** buffer: `array<LightPacked, 1024>`, and
`1024 × 64 B = 64 KB`, which is exactly WebGPU's
`maxUniformBufferBindingSize`. Uniform (vs storage) is the right choice
because most fragments read a small, coherent set of light slots. Going
past 1024 would require moving `lights` to a **storage** buffer in all
three passes (cull, opaque, transparent) — which also adds a
storage-buffer binding to the opaque stage, already near the
`maxStorageBuffersPerShaderStage` ceiling (see §10). So 1024 is treated
as the design ceiling, not an accident.

### The single path — per-froxel GPU cull (all opaque + all transparent)

Every surface (opaque via `material_opaque` + the MSAA `edge_resolve`,
and all transparent) shades punctual lights from its **per-pixel froxel
light list**. There is no per-mesh light list and no mesh-size gate:
clustered (froxel) culling is generic and camera-correct for any mesh
size, so a 1 m prop and a 100 m floor use the same path.

> **History (why this is now one path).** The renderer used to have a
> per-mesh CPU bucket path (BVH-assigned light lists per mesh) for "small"
> meshes, and only routed "oversized" meshes (AABB diagonal > 50 m **and**
> bucket > 16 lights, via a `light_slice_count = 0xFFFFFFFF` sentinel) to
> the froxel path. That gate was a tuning liability — the thresholds
> depend on the game/camera/scene, and a big floor with a moderate light
> count would fall back to per-mesh and dump *every* overlapping light on
> every pixel (the cause of the "1024-lights floor runs at ~8 fps"
> regression: ~1000 lights/pixel). Unifying on always-on froxel removed
> the gate and fixed that (the floor drops to ~tens of lights/pixel after
> the cull → 60 fps with large headroom). The cull already runs whenever
> any light is live (transparent needs it), so the unification adds ~zero
> cull cost. **The CPU `LightMeshBuckets::rebuild` is retained, but now
> only feeds the shadow-receiver gate** (`is_shadow_receiver` →
> `MaterialMeshMeta.shadow_receiver_gate`); its per-mesh light-list /
> oversized-classification output is no longer consumed for shading and is
> a candidate for a future surgical removal (it shares a rebuild pass with
> the shadow-receiver flags, so it must be untangled carefully).

The froxel grid (`render_passes/light_culling/`): screen split into
16×16-px tiles × `DEFAULT_SLICE_COUNT = 32` exponential view-space Z
slices. The cull compute pass tests each light's bounding sphere against
each froxel's frustum and atomic-appends surviving indices into a
per-froxel slice of `storage_buffer` (tail region). Consumers
(`apply_lighting_per_froxel*`) walk only their froxel's slice — steady
state ≤ `max_per_froxel_capacity` lights/pixel.

**Two-level (clustered) cull.** The cull is split into two compute
dispatches over one shader module (distinct `@compute` entry points):
- `cs_tile` (Stage A): one workgroup per **2D tile** — runs only the four
  **side-plane** tests (which are Z-independent) and writes a per-tile
  candidate list (`tile_lights`).
- `cs_main` (Stage B): one workgroup per **froxel** — reads only its
  tile's candidates and applies the cheap **Z-slice** test, writing the
  per-froxel slices.

Because the side planes don't depend on Z, the tile candidate set is
exactly the Z-independent union of the column's froxels, so the output is
**identical** to a single-level cull — but the expensive side-plane test
runs once per tile instead of once per froxel (≈ `SLICE_COUNT`× less).
Verified pixel-identical (full-canvas diff) on the tuning scenes.

**Camera-type correctness.** The side-plane test (`cs_tile`) is built for
both projection types: perspective uses the four frustum side planes
through the camera origin; **orthographic** uses an axis-aligned
view-space box test instead (parallel rays — the pyramid-plane
construction is invalid for ortho), selected at runtime via
`proj[3].w`. Stage B's Z-slice test and the consumer froxel lookup are
already camera-agnostic. Verified via the `tuning-cull-debug` probe +
the light-count heatmap in both perspective and orthographic.

### Runtime-sized capacities (no recompiles, small buffers)

Both per-froxel budgets are **runtime** fields on the `cull_params`
uniform, so they grow without recompiling any shader:
- **`max_per_froxel_capacity`** — per-froxel index budget. **Phase 1D
  auto-grow**: a per-frame `mapAsync` readback of `overflow_counter`
  doubles it when the cull dropped indices (e.g. `32 → 64 → 128` on the
  1024-light fixture), then settles.
- **`tile_light_capacity`** — Stage-A per-2D-tile candidate budget. Grown
  by `ensure_tile_light_capacity` toward the **live punctual-light
  count** (a tile can't hold more candidates than there are punctual
  lights, so it never overflows — no fallback path). This keeps the
  `tile_lights` buffer proportional to the actual light count
  (e.g. ~1 MB at 1080p for 64 lights) instead of always sizing for 1024
  (~16.8 MB). Identical at the 1024-light ceiling.

### Where the cost is (and isn't)

- **Shading** scales well: per-pixel light count is bounded by the froxel
  budget, regardless of total lights — the froxel sweet spot is lights
  *spread across the scene*.
- **The cull pass itself is still `O(tiles × lights)`** in Stage A (every
  tile tests every light). The two-level split cut the *constant*, not
  the asymptotic; total cull cost still grows linearly with light count.
  ~12 M side-plane tests at 3 k lights (cheap), ~123 M at 30 k (bites).
- **Point-light shadows are a separate, tighter cap** (8 cube slots —
  see §5f / `docs/SHADOWS.md`); many *shadowed* point lights thrash that
  pool independently of culling.
- **Worst case = many lights in one froxel** (all overlapping one screen
  region): culling can't help (they genuinely reach those pixels),
  auto-grow raises the budget, and shading walks them all.

### Direction (parked — see §10)

Pushing to *thousands* of lights (e.g. particle-driven) is **not**
supported today and is parked, because it's a conditional win that taxes
the common case. It would need: (1) `lights` uniform → storage (+ raise
the cap, + resolve the opaque storage-binding budget), and (2) a
GPU-resident light grid/BVH to make Stage A test only *nearby* lights
(the only way to beat the `tiles × lights` linear scan). Both should be
**light-count-gated tiers**, not always-on, and need profiling on real
high-light scenes first. The CPU rstar tree can't be reused here — it's a
CPU structure and the froxel cull is a massively-parallel GPU pass.

---

## 5g. Shader cache warmup — what the browser caches, what we don't

The browser caches compiled WebGPU pipeline objects (PSOs — the
driver-compiled shader + render-state bundle returned by
`device.createComputePipeline()` / `createRenderPipeline()`) on
disk, keyed by `(driver version, GPU adapter, hash of WGSL source
+ pipeline descriptor)`. After the first compile, subsequent page
loads on the same wasm bundle restore the compiled pipeline in
microseconds instead of recompiling.

What that means for our renderer:

- **A pipeline compiles the first time it's drawn**, not when its
  material is registered. So `populate_gltf` finishing doesn't
  guarantee the PBR-opaque-compute pipeline is warm — the first
  visible draw of a PBR mesh does.
- **Hidden meshes don't drive compilation.** They're filtered out
  of `collect_renderables`. The scene-editor's `gizmo.glb` loads
  at init but stays hidden until selection; its PBR /
  HUD-geometry pipelines don't compile until a *user-visible*
  PBR mesh materialises. The first Insert Model therefore pays
  the full compile tax — often hundreds of ms across PBR-opaque
  / PBR-transparent / geometry / shadow-generation pipelines.
- **Any WGSL source change busts the disk cache** for the
  affected pipeline. The hash changes, the cached entry doesn't
  match, the next page load recompiles. This isn't ours to fix
  — it's a correctness property of the cache.

### Per-origin, persistent across reloads, not across origins

Chrome's GPU disk cache lives in the user's profile directory
(`~/Library/Application Support/Google/Chrome/Default/GPUCache`
on macOS) and persists until the user clears browser data,
Chrome itself is updated, or the GPU driver changes. The cache
is keyed per-origin to prevent fingerprinting: a shader cached
on site A doesn't unlock the same shader on site B. CDN-hosted
wasm running on a project-specific domain still pays the
per-origin first-compile tax for each origin independently.

Firefox / Safari have weaker / not-yet-shipped equivalents.
Cross-browser, the correctness model is "expect a cold first
compile every time you can't prove the cache is warm."

### WGSL source changes invalidate the cache

Any deploy that touches the WGSL emitted for a material pipeline
— adding a binding, changing a uniform layout, modifying a call
site shared across material variants — changes the source hash
and busts the cached PSO for every affected pipeline. The
**next** user load on each profile pays the cold-compile tax
once; subsequent loads hit the disk cache again.

A material-pipeline source change can invalidate a dozen+
pipelines simultaneously (opaque-compute × shader_id, MSAA
variants, shadow caster, transparent). Without pre-warm the cost
falls on the first interactive draw — typically the first
visible PBR mesh — as a multi-hundred-ms stall. **A production
game should not ship that behaviour.** Drive a draw of every
routinely-used pipeline during the load screen, where the user
already expects to wait.

See [§8a step 5](#5-pre-warm-the-shaderpso-cache) for the
practical recipe.

### How the renderer parallelises the cold-load compile

Every `createRenderPipelineAsync` /
`createComputePipelineAsync` call goes through
`RenderPipelines::ensure_keys` / `ComputePipelines::ensure_keys` —
these mirror `Shaders::ensure_keys`: build all descriptors
synchronously, fire every Promise back-to-back so Dawn's compile
pool starts on all of them in parallel, then `join_all` the
`JsFuture`s. Wall-clock for an N-pipeline batch drops from
`sum(t_i)` to `max(t_i)` bounded by the Dawn pool size
(≈ `num_cpus`).

#### Cross-renderer pool inside `AwsmRendererBuilder::build`

`build()` runs in **three stages**, each ending in one main await.
Stages 2 and 3 are the actual cross-renderer Dawn pool work; stage
1 is the concurrent setup that feeds them.

**Stage 1 — concurrent setup (one `try_join!`).** Texture prep
+ render-texture creation + cache key collection all run in
parallel against `&gpu`:

- Three default-cubemap `prepare_resources` futures (prefiltered
  IBL, irradiance IBL, skybox),
- BRDF LUT generation,
- opaque-mipgen pipeline construction,
- `RenderTextures::new`,
- `RenderPasses::describe_shaders` — phase 1 of the
  `describe_shaders → describe_pipelines → from_resolved` split.
  Returns bind groups + the union of every render pass's shader
  cache keys. No Dawn shader/pipeline compile in this stage.

**Stage 2 — cross-renderer shader pool.** One
`Shaders::ensure_keys` covering every shader the renderer
compiles: RenderPasses' own (opaque × 14, geometry × 18, hzb × 3,
classify × 2, decal × 2+2, coverage × 1–2, occlusion × 1,
compaction × 1), shadow caster (× 2), picker (× 2, gated by
`features.picking`), line, effects (× 5, AA + post-processing
dependent), and display (× 1). Joined via `futures::join` with
the EVSM inline-shader `validate_shader` futures —
`Shadows::build_descriptors` issues 3 `compile_shader` calls for
the EVSM moment-write + blur shaders (which bypass the shared
shader cache); their validate futures are joined here so EVSM
validation overlaps with the rest of the shader pool. The 3 EVSM
modules then enter the shader cache via
`Shaders::insert_uncached`.

**Stage 3 — cross-renderer pipeline pool.** One `try_join`'d
`ComputePipelines::ensure_keys` + `RenderPipelines::ensure_keys`
covering every compute + render pipeline across the entire
renderer (~36 compute + ~27 render on a fully-featured build).
Compute + render compile concurrently inside Dawn's worker pool
via a split-borrow on `Pipelines.compute` / `Pipelines.render`
(disjoint `&mut` fields).

After stage 3, sync fold-up assembles the typed
`RenderPasses` via `RenderPasses::from_resolved` and each tail
subsystem (Picker, LineRenderer, Shadows + EVSM, Effects' and
Display's typed `Pipelines` inside `RenderPasses`) via its own
`from_resolved` / `install_resolved` — no further awaits.

#### Architectural guarantees

`RenderPasses::new` is a thin 3-stage wrapper that the
orchestrator bypasses; `describe_shaders` is `async` only because
of bind-group constructor awaits, `describe_pipelines` is sync
apart from cache-hit `get_key`s, and `from_resolved` is fully sync.
A future contributor adding a new render pass can't accidentally
introduce a sequential `.await?` that bypasses the cross-renderer
pool: the type system forces the new cache keys through
`describe_pipelines`'s returned `Vec`s.

The dynamic-setter path (mid-session AA / post-processing flips)
is preserved: `set_anti_aliasing` and `set_post_processing` still
call `EffectsPipelines::set_render_pipeline_keys` and
`DisplayPipelines::set_render_pipeline_key`, which wrap the same
`build_descriptors` + `install_resolved` shape the orchestrator
drives at startup.

#### Other batched paths

- `finalize_gpu_textures` recompiles every transparent mesh's
  pipeline through the same batched API — the cycle that fires
  once per model load (texture pool grew, every transparent
  pipeline's bind-group layout is stale) compiles in parallel.
- `AwsmRenderer::prewarm_pipelines` walks `self.meshes` and runs
  one batched `ensure_keys` for every unique (buffer_info,
  material) combination. Useful immediately after a model load,
  before the first frame; idempotent and free on subsequent calls.
- `AwsmRendererBuilder::with_phase_handler` lets a consumer hook
  every `RendererLoadingPhase` transition during `build()` so the
  UI can show "Browser is compiling shaders…" rather than a frozen
  generic loading line.

#### Reference numbers

Reading off a fresh `--user-data-dir` Chrome trace against the
model-tests Fox scene on an M2 MacBook (AC power; on battery the
wall-clock degrades by ~50% as the SoC throttles to E-cores):

| Metric | Reference |
|---|---|
| `domComplete → first 'Render [1]: span-enter'` | ~600 ms |
| `domComplete → 'Prewarm Pipelines [1]: span-enter'` | ~485 ms |
| GPU-process total CPU | ~740 ms |
| Renderer-main-thread total CPU | ~1000 ms |

Note GPU-process CPU exceeds the wall-clock window: the three
processes (Renderer / GPU / Browser) run concurrently, so total
cross-process CPU work of ~1.9 s lands in the ~600 ms window.

**The cache hierarchy matters when interpreting these numbers.**
Below WebGPU's PSO disk cache there's the OS / GPU-driver level
pipeline cache (macOS Metal, the equivalent on Windows / Linux).
`--user-data-dir` only wipes Chrome's cache, not the driver's.
On a developer machine that has run this codebase before, a
"fresh Chrome profile" looks much like a warm load to the
renderer because the driver still has every pipeline cached.
The first-ever run of the app on a particular machine pays both
layers cold; the cost of that compile work is on the order of
tens of seconds, no JS-side parallelism eliminates it.

User-timing marks after `Prewarm Pipelines` should cascade
~1 ms apart on any healthy capture. A forest of ~500 ms gaps
between marks indicates Dawn is serialising compile work behind
itself, which means the pool got fragmented and someone added a
per-pass `.await?` outside the orchestrator — start by reading
the `describe_pipelines` / `from_resolved` call sites.

#### What's not addressed here

Pipeline-compile parallelization is at its theoretical JS-side
limit. Other axes of "page-load → first useful frame" live in
different problem spaces:

| Axis | Approx magnitude | Lever |
|---|---|---|
| WASM module instantiation + glue | 200–500 ms | bundle size, code splitting, LTO, cargo-bloat audit |
| Browser-process startup | ~450 ms | minimal lever — Chrome's machinery |
| JS ↔ Rust marshalling per `gpu.create_*` | dispersed | descriptor-build overhead reduction |
| Texture decode (ImageBitmap × 3 + BRDF) | 300–500 ms (already parallelised) | smaller default cubemaps, shipped BRDF LUT |
| First-model-load (gltf + textures + finalize) | 150–400 ms / model | separate path with its own pool |
| First-frame bind-group recreate | 10–30 ms | pre-create some bind groups in `build()` |
| Driver-level MSL lowering (Metal / Vulkan / D3D) | 5+ s truly cold | no JS hook — browser team |

Of these, **WASM size + instantiation** has the cleanest cost /
benefit story for a focused follow-up.

### Runtime-registered materials and prewarm

If the consumer registers custom material shaders at runtime
(`MaterialDefinition` + `shader.wgsl` fragment, registered after
`AwsmRendererBuilder::build` returns), the prewarm story extends
naturally. Two wrinkles to handle:

1. **Custom shader_ids aren't known until registration.**
   First-party materials are enumerable at compile time
   (`enabled_materials()`); custom ones come from registry
   calls. Drive
   [`AwsmRenderer::prewarm_pipelines`](../crates/renderer/src/lib.rs)
   *after* the consumer has finished registering, before the
   first gameplay frame.
2. **Mid-session registration triggers a recompile.** Registering
   a new shader_id mid-frame busts the cached opaque-compute /
   transparent-fragment pipelines (the dispatch chain text
   changes). A game that streams in custom materials during play
   sees the same stutter pattern WGSL-source-change deploys see,
   but mid-gameplay. Same fix: pre-warm by drawing one mesh per
   newly-registered shader_id before the next user-interactive
   frame.

`prewarm_pipelines` is the canonical "I'm done registering
materials, please compile everything" hook. It currently walks
`self.meshes` to warm transparents for the live scene; consumers
with runtime-registered shader_ids should call it after each
registration batch, and the method's batched `ensure_keys` path
will compile every newly-needed variant in one pool.

The cost of the warmup pass *is* the compile tax — it doesn't
make compilation faster, it just relocates it to a frame the
user expects to be slow (the splash) instead of one they expect
to be instant (the first interactive draw). On Chrome with a
warm GPU disk cache, the warmup pass itself takes <5 ms; on a
cold cache (first-ever visit, post-redeploy reload), it takes
50–500 ms depending on how many pipelines and how heavy the
shader. That cost is unavoidable — what changes is *when* the
user pays it.

### 5g-i. Cold-load measurement procedure

When changing anything that touches pipeline creation, capture
before/after traces on a **fresh** Chrome profile so the disk
PSO cache is empty. The recipe:

```sh
# Always use a unique --user-data-dir for cold capture; reuse
# the same one (without rm) for the warm follow-up.
PROFILE=/tmp/chrome-webgpu-cold-$(date +%s)
/Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome \
    --user-data-dir=$PROFILE \
    http://localhost:9080/  # or 9081 for scene-editor
```

In the resulting browser:
1. Open DevTools → Performance.
2. Click record, hit Reload, wait until the first frame draws
   (or until `Render [1]` user-timing marks start appearing in
   the timeline), then stop.
3. Right-click the timeline → "Save profile". Save with an
   informative name (`cold-baseline.json`, `cold-phase-3.json`).
4. For the warm follow-up: with the same profile dir still
   live, reload once more (DevTools open, recording again).

What to read off the saved trace:

- **`domComplete → first 'Render [1]: span-enter'`**: the
  headline metric — total wall-clock the user waits between
  the wasm bundle finishing and the first frame.
- **`domComplete → 'Prewarm Pipelines [1]: span-enter'`**: the
  cost of `AwsmRendererBuilder::build` (everything before the
  first prewarm call). Drops to milliseconds on warm.
- **GPU-process total CPU** (Bottom-Up by activity): same on
  cold and warm if the only difference is the PSO cache — the
  driver still does the work, the cache just remembers the
  result.
- **Renderer-main idle gaps**: scrolling the renderer-main
  thread row should show no ~500 ms idle staircases between
  user-timing marks; on a healthy capture, marks after
  `Prewarm Pipelines` cascade ~1 ms apart. Visible gaps mean
  Dawn is serialising compile work behind the renderer thread,
  which is a sign that someone added a per-pass `.await?` outside
  the orchestrator pool.

If a change claims to improve cold start, the trace numbers belong
in its PR description.

---

## 5d. Steady-state perf — `tuning-10k-meshes` reference numbers

Captured via `read_render_pass_timings(min_count=30)` on Chrome
through the Claude Preview MCP, after loading
[`assets/world/tuning-10k-meshes`](../assets/world/tuning-10k-meshes)
and letting 181 frames accumulate. Hardware: M2 MacBook. These
numbers are the bar a renderer change should clear before it lands.

| Pass | mean ms | p95 ms | max ms |
|---|---|---|---|
| **Render (whole frame)** | **2.74** | **3.7** | **4.5** |
| Geometry RenderPass | 0.51 | 0.6 | 0.9 |
| Shadow Generation | 0.73 | 1.6 | 1.9 |
| Collect renderables | 0.36 | 0.5 | 0.5 |
| SceneSpatial Rebuild | 0.14 | 0.1 | 4.0 (periodic) |
| Camera GPU write | 0.10 | 0.2 | 0.2 |
| Shadow Receiver Gate (§5f) | 0.048 | 0.1 | 0.2 |
| Punctual Lights GPU write | 0.02 | 0.1 | 0.1 |
| Occlusion Cull | 0.02 | 0.1 | 0.1 |
| HZB RenderPass | 0.02 | 0.1 | 0.2 |
| Material Classify | 0.01 | 0.1 | 0.1 |
| Display RenderPass | 0.02 | 0.1 | 0.1 |

Captured with every production optimisation engaged: coverage-
driven skin-skip with grace + BVH override, cheap-material LOD
routing, shadow-receiver gate, fixed 16-tap PCSS, and worker-mode
gltf with in-worker image decode. The per-mesh `Shadow Receiver
Gate` walk costs 0.048 ms and is offset by the corresponding
~0.1 ms savings in the geometry pass when meshes that no shadow
caster reaches skip the entire shadow-sample chain.

Frame budget at 60 fps is 16.67 ms; the renderer runs at ~6× that
headroom. p95 stays at 3.7 ms even on a 10k-mesh stress scene.

**Upload-ring telemetry from the same run** (`read_upload_ring_stats()._total`):

| Counter | Value |
|---|---|
| `bytes_uploaded_via_ring` | 242 MB / 181 frames = 1.34 MB/frame |
| `bytes_uploaded_via_fallback` | 512 B (single cold-start frame) |
| `bytes_uploaded_via_writebuffer` | 0 |
| `fallback_count` | 1 |
| `peak_ring_depth_used` | 3 (full ring rotation) |
| `resize_count` | 0 |

The ring is delivering: 99.9999% of bytes go through the mapped fast
path; the single fallback is the expected first-frame edge before
`mapAsync` cycles populate the cursor's next slot.

### What "regressed" looks like

If `Render` mean > 5 ms on `tuning-10k-meshes`, something landed
that scales linearly with mesh count instead of going through the
BVH. The diagnostic recipe in §7 walks through how to find it.

If `_total.fallback_count` grows beyond cold-start (i.e. > 2-3 on a
fresh load), some buffer's ring depth (default 3) isn't deep enough
for its frame cadence — bump it via
`MappedUploader::with_ring_depth(label, depth)` at construction.

If `_total.bytes_uploaded_via_writebuffer` grows, foreign-bytes
ingestion (`MappedUploader::ingest_foreign`) is being called more
than expected — usually a glTF load.

---

## 6. Hot-path catalogue

When optimizing or reviewing a PR, these are the files that
move the needle:

| File | What lives here | Watch for |
|---|---|---|
| `render.rs::AwsmRenderer::render` | The per-frame entry point. Wraps every other pass. | New work added here regresses every frame. Be sure new GPU writes are gated on a dirty flag. |
| `renderable.rs::collect_renderables` | Builds the per-frame opaque/transparent/HUD lists. Runs every frame. | Per-mesh allocations or material-key recomputation. The BVH query + per-mesh `effective_material_key` are the only intended work. |
| `meshes/mesh.rs::push_geometry_pass_commands` | Per-mesh draw recording. | Vertex/index buffer rebinds. Two non-instanced variants picked by `features.indirect_first_instance_enabled()`: storage-array meta (shared bind group, requires `indirect-first-instance`) or uniform-with-dynamic-offset (portable, one `setBindGroup` per draw). Instanced meshes always use uniform-with-dynamic-offset. |
| `shadows/state.rs::write_gpu` | Reconciles shadow descriptors + throttle state. | Per-light writes scale with shadow caster count × cascade/cube count. |
| `light_buckets/buckets.rs::rebuild` | Per-mesh × per-light AABB overlap. Runs every frame. | O(meshes × lights) cost — but BVH-driven for normal meshes, and oversized meshes skip the per-light walk. |
| `scene_spatial/*` | The BVH (rstar). Per-pass frustum culling descends through this instead of walking meshes. | Don't add full mesh-walk fallbacks — they re-introduce the cost the BVH eliminates. |
| `transforms.rs::update_inner_recursively` | World-transform propagation. | Adding work here scales with hierarchy depth. |

---

## 7. Diagnostic recipes

### "My scene drops frames at N+ meshes"

1. Read `performance.getEntriesByType('measure')` (or the
   browser's Performance tab) to find the dominant span. Make
   sure the page is loaded at the `SubFrame` tracing tier — debug
   builds default to it, release builds need `?trace=sub-frame`.
   See [perf-tracing.md](perf-tracing.md).
2. If `Geometry RenderPass` dominates: turn `gpu_culling` on
   if it isn't (saves per-mesh CPU recording + indirect-draws
   GPU-cull the invisible set).
3. If `Shadow Generation` dominates: lower per-light tiers via
   `refresh_light_importance_budgets()`, drop
   `point_shadow_resolution` to 512, set
   `CubeFaceUpdateRate::Every2Frames` for non-hero point lights.
4. If `Material Opaque RenderPass` dominates: check if you have
   3+ shader_ids active — each adds a dispatch. Consider
   collapsing rare material flavours.
5. If `Collect renderables` dominates: the per-frame BVH query
   is hitting a degenerate case (probably an unbounded scene
   with most meshes lacking world AABBs). Make sure meshes
   that *should* be in the index have `world_aabb` set.

### "Memory pressure grows over time"

1. `RendererFeatures::default()` (both off) for tools that don't
   need GPU culling or decals. Drops ~33 MB at 4K.
2. Check `meshes.len()` — orphaned meshes that were never
   removed via `AwsmRenderer::remove_mesh` accumulate.
3. Per-light shadow resolution can quietly bloat the cube pool;
   each cube slot is `6 × resolution² × 4 B`.

### "Per-frame CPU recording is high but GPU is idle"

1. Look at `Collect renderables` first — the BVH walk + per-mesh
   `effective_material_key` should be sub-millisecond at
   moderate mesh counts. If it's high, either many meshes lack
   world AABBs (forcing the conservative tail-walk) or the BVH
   needs a rebuild (`scene_spatial::rebuild_if_needed`).
2. Per-frame `gpu.write_buffer` calls for the
   `IndirectDrawArgs` static fields scale with `meshes.len()`
   not visible-count. At 10K meshes this is ~320 KB / frame.
   Not huge but lurks as background CPU work.

### "drawIndirect path renders garbage / misses meshes"

1. Check which `indirect_first_instance` variant is active. The
   storage-array path writes `first_instance = mesh_slot` from the
   compaction shader and requires the WebGPU
   `indirect-first-instance` feature. The portable path writes
   `first_instance = 0` and routes the slot identity through a
   uniform-with-dynamic-offset bind group set per draw. A
   mismatched variant (shader expects one, runtime feeds the other)
   silently produces no draws on the storage-array path — log
   `features.indirect_first_instance_enabled()` at render entry to
   confirm. The `?ifi=on/off` URL switch in the editor (debug
   builds) forces a specific variant for A/B testing.
2. Check `occlusion_instances[i].mesh_meta_offset` is populated
   correctly. An earlier rewire left it as a `0u32` placeholder;
   the compaction shader divides by 256 to derive the slot index,
   so a wrong offset is silently a no-op.
3. Confirm the geometry meta + material meta slot indices for
   the same `MeshKey` align (they should because both
   `SecondaryMap`s are inserted/removed in lockstep). The
   coverage producer uses *material* slot indices because
   visibility_data carries `material_mesh_meta_offset`.
4. Instanced meshes are always on the
   uniform-with-dynamic-offset path regardless of
   `indirect_first_instance` — their `instance_index` ranges would
   otherwise collide between meshes in the shared storage-array
   meta lookup.

---

## 8. Authoring guidance

Realistic content shapes the heuristics. The defaults are tuned
for the conditions below; far-outliers may need per-mesh /
per-light overrides.

**Light intensities.** Hero lights at distance ≤ 5 from camera
with intensity ≥ 10 climb to High/Ultra and cost real shadow
budget. Ambient fills 20+ units away at intensity ≤ 5 should
sit at Low. The `intensity / (1 + dist²)` heuristic does this
naturally; you only need to override when your content's
intensity scale doesn't fit (e.g. cinematic intensities of
thousands).

**Mesh sizes.** Anything with an AABB diagonal > 50 m gets
flagged as an "oversized mesh" and skips the per-light bucket
loop. This is correct for terrain / ocean / skydomes. If you
have a "large prop" that genuinely *should* be in light buckets
(e.g. a hero ship), bump `OVERSIZED_AABB_DIAGONAL_METERS` to
match your scale.

**Mesh churn.** `SceneSpatial`'s rebuild cadence is sized for
moderate churn (sidecar for dynamic meshes, BVH rebuild on a
threshold). Worst-case is a scene where every mesh is dynamic
*and* gets inserted/removed every frame — at that point the
sidecar linear-scan dominates. Mark static meshes as such by
keeping them inserted once.

**Decals.** Cap is `MAX_DECAL_COUNT = 128` simultaneously
active. Beyond that, `insert_decal` returns
`AwsmDecalError::TooManyDecals`. Decals with `alpha = 0` still
participate in classify (frustum-tested + per-tile-bucketed)
but contribute no pixels — use `remove_decal` if a decal is
truly inactive.

**Skinned characters.** `skin_update_period > 1` cuts skinning
work proportionally. At distance ≥ 20 m the visual difference
is sub-pixel; use the
`AwsmRenderer::set_mesh_skin_update_period_by_distance` helper.

**Insert Model auto-framing (scene-editor UX).** After a glTF
materialises on the GPU, the editor calls
`actions::camera::frame_on_meshes` with the inserted template's
mesh keys, unions their `world_aabb`s, and re-builds
`FreeCamera::new_aabb(...)` at margin 1.5 (matching
`model-tests`). Without this step, small models — e.g.
`DamagedHelmet` (~2 unit AABB), `Corset` (~0.05 unit AABB) —
appear as a speck against the editor's default 36-unit-away
camera and look "blank" even though base color / normal /
metallic-roughness textures are bound correctly. The user's
projection mode (Perspective / Orthographic) is preserved, so
no UI state flips. Programmatic inserts via
`measurement::insert_model_from_url` get the same treatment.

---

## 8a. Shipping a game — defaults audit + recipe

This is the one-stop reference for "I'm shipping a game on this
renderer; what do I need to do beyond the defaults?" Most knobs
default to game-friendly values; the items below are the
*explicit* setup a production consumer should do.

### Defaults that are already game-friendly (no action)

| Default | Value | Why it's right |
|---|---|---|
| `AwsmRendererLogging::render_timings` | `RenderTimings::Off` | Tiered enum (`Off` / `Frame` / `SubFrame`). The default is `Off` — zero overhead. Frontends opt up to `Frame` (single outer span, ~free) for production and `SubFrame` (every pass) for diagnosis; the `?trace=…` URL param overrides at runtime. See [perf-tracing.md](perf-tracing.md). |
| Mapped-buffer staging ring | Always on | Every per-frame `writeBuffer` site is routed through `MappedUploader`. 99.9999% of bytes go through the mapped fast path on 10k meshes. |
| Coverage-driven skin-skip (§5d) | Always on | Off-screen skins stop animating after a 2-frame grace; in-frustum skins resume that same frame via the BVH override. |
| Shadow-receiver gate (§5f) | Always on | Meshes no caster reaches skip the entire shadow-sample chain. 0.048 ms / frame to maintain on 10k meshes. |
| PCSS tap count | Fixed 16 (cube + directional + 2D spot + Soft) | Sized to the static Poisson-disc table. See §5f for why receiver-distance-keyed tap budgeting doesn't work. |
| Adaptive optimization policy | On with `Auto` cooldown | `RendererOptimizationPolicy` flips `indirect_first_instance`, `occlusion`, `coverage_lod` etc. based on per-frame signals. Manual override only for A/B testing. |
| Scene-spatial BVH | `rebuild_dirty_threshold: 200`, `rebuild_period_frames: 600` | Right for 1K–5K dynamic meshes. Bump for 10K+ static-heavy scenes. |
| `default_cheap_material_pixel_threshold` | 64 px | Below this, the cheap variant takes over on any mesh that has one authored. Per-mesh override always wins. |
| Worker-mode gltf parse (editor) | Pre-warmed at boot; default-on | Editor `asset_cache::load_and_populate` dispatches via a 2-worker pool built during `create_context`. Sticky inline fallback if the bootstrap fails. Library consumers don't inherit this — game-side wiring still needs the snippet in [§8a step 1](#1-pre-warm-the-worker-pool-for-mid-gameplay-asset-loads). |

### What a game must do at startup

#### 1. Pre-warm the worker pool (for mid-gameplay asset loads)

The scene-editor pre-warms a `WorkerPool` at boot and routes
`asset_cache::load_and_populate` through it by default (see
[§5c → "The editor flip"](#5c-worker-mode-gltf-parse--default-in-the-editor)).
A *library consumer* (a shipped game) doesn't inherit that pool —
the editor's wiring lives in its own `context.rs`. For shipped
games loading additional content during play (level streaming,
animation rigs, audio-paired assets, …), build a pool at startup
so the dispatch cost is amortised and worker mode actually engages:

```rust
use awsm_renderer::workers::{WorkerPool, WorkerPoolBootstrap};
use awsm_renderer_gltf::worker_job::GltfParseJob;

// Run during game-init splash, before the first frame.
let pool = WorkerPool::with_workers(Some(2)).await?;
pool.register::<GltfParseJob>();
awsm_renderer::workers::register_job::<GltfParseJob>();
```

Then route asset loads through the pool:

```rust
let out = pool.dispatch::<GltfParseJob>(GltfParseInput {
    url: asset_url.into(),
    file_type: None,
}).await?;
let loader = out.into_loader().await?;
renderer.populate_gltf(loader.into_data(None)?, None).await?;
```

Measured break-even: ~5 MB. Below that, inline is within noise of
worker; above, worker beats inline by 2× and the main thread
stays free for game logic. (Corset.glb 12.8 MB: inline 196 ms /
worker 91 ms = **2.15× speedup** on M2 Chrome with the in-worker
`createImageBitmap` + handle-transfer + zero-copy byte-transfer
path. Headless Chrome compresses the relative gap — see §5c for
why.)

#### 2. Author cheap-material variants on distant props

For meshes that render at small pixel coverage in the typical
play frustum (rocks, distant trees, ambient debris), author a
cheap material in the asset pipeline and bind it once at insert
time:

```rust
renderer.set_mesh_cheap_material(
    mesh_key,
    Some(cheap_material_key),   // same shader_id + same alpha mode
    Some(32),                   // per-mesh override; None → renderer default (64 px)
)?;
```

The cheap variant kicks in on every frame coverage < threshold;
re-pack of `material_offset` is a single 4-byte patch via the
mapped-buffer ring. Steady-state writes are O(0) when nothing
crossed the threshold.

#### 3. Distance-LOD skinning for character/crowd scenes

Crowds and distant NPCs don't need per-frame joint updates.
Drive the cadence from a per-second tick (call once every 10
frames is plenty):

```rust
use awsm_renderer::meshes::skin_lod::SkinLodLevel;

renderer.set_skin_update_periods_by_distance(camera_pos, &[
    SkinLodLevel { max_distance: 10.0, period: 1 },  // hero — every frame
    SkinLodLevel { max_distance: 30.0, period: 2 },  // mid — every other
    SkinLodLevel { max_distance: 80.0, period: 4 },  // far — quarter-rate
    // past the last threshold, the slowest tier sticks
]);
```

Pairs with the coverage-driven skip on §5d: out-of-frustum
crowds drop to zero work entirely; visible crowds run at the
period above.

#### 4. Per-mesh shadow opt-outs for HUD / sky / particles

```rust
mesh.cast_shadows = false;    // skip from shadow generation
mesh.receive_shadows = false; // skip from sample-side shadow lookup
mesh.receive_decals = false;  // skip per-decal volume test
```

`Mesh::hud` is the heavy-hammer "skip every cull / pass / shadow"
flag — set it on HUD overlays and screen-space effects.

#### 5. Pre-warm the shader/PSO cache

The first time a pipeline is actually *drawn*, WebGPU compiles
its shader; the browser then caches the compiled binary on
disk and subsequent page loads restore it in microseconds. See
[§5g](#5g-shader-cache-warmup--what-the-browser-caches-what-we-dont)
for the full mechanics — what the cache keys on, when it
invalidates, and how it interacts with runtime-registered
materials.

The short version for game-shipping: unhide one mesh per
`MaterialShaderId` (PBR / Unlit / Toon) — plus the MSAA-on
variants if your game lets the player toggle MSAA — briefly
during the load screen so WebGPU compiles their pipelines
while the user is still looking at the splash, not at a frozen
viewport on first gameplay frame.

### Tuning knobs by play style

| Game style | What to bump | Why |
|---|---|---|
| **Twin-stick action / racing** | `Hardness::Soft` on most lights | Camera moves fast; the fixed 16-tap Soft path is the cheapest smooth-shadow setting. PCSS is overkill when motion blur hides edge quality anyway. |
| **First-person exploration** | `Hardness::Pcss` on hero lights, `Soft` on rest | Camera is stationary; near contact-hardening matters and 16-tap PCSS resolves it cleanly. |
| **Crowd / RTS** | `default_cheap_material_pixel_threshold: 128`, `SkinLodLevel { 50.0, period: 8 }` | Most meshes are far. Aggressive material LOD + slow skinning. |
| **Mobile / low-end desktop** | `ShadowsConfig::point_shadow_resolution: 512`, `Hardness::Soft` everywhere | Cut VRAM (4× drop) + use 16-tap Soft instead of 32-tap PCSS. |
| **Cinematic / promo** | All defaults, `?features=on` | Quality wins; the editor's debug knobs are off. |


### What to monitor in production

The `read_render_pass_timings` + `read_upload_ring_stats` helpers
in `crates/frontend/scene-editor/src/actions/measurement.rs` are
debug-only, but the same data is on the renderer's tracing spans
— production consumers can subscribe to the `tracing` subscriber
and emit metrics to their own telemetry system.

The two metrics to watch:

- **Render mean ms** crossing the frame budget. On `tuning-10k-meshes`
  the reference is 2.74 ms (§5d); a steady-state cross past ~5 ms
  is a regression.
- **Upload-ring `bytes_uploaded_via_writebuffer` growing**. Means
  foreign-bytes ingestion fired more than expected — usually a
  gltf load. Look at the call site, not the upload.

---

## 9. Measurement harness

Driven by the Claude Preview MCP (or any equivalent). The
scene-editor exposes four `#[cfg(debug_assertions)]`
`#[wasm_bindgen]` helpers from
`crates/frontend/scene-editor/src/actions/measurement.rs`:

| Helper | Returns | Use |
|---|---|---|
| `load_scene_by_path("tuning-Xxx")` | Promise<()> | Loads `assets/world/<name>/project.json` via fetch. |
| `read_mesh_coverage_stats()` | JSON string | Verifies the GPU coverage producer reached the CPU table. |
| `read_importance_tier_histogram()` | JSON string | Shadow-caster-light tier histogram. |
| `read_oversized_mesh_stats()` | JSON string | `{ last_max_bucket, oversized_count }` from `LightMeshBuckets`. |
| `read_render_pass_timings(min_count)` | JSON string | Per-pass `count / mean / p50 / p95 / max / total` (ms). Strips the `[id]: span-measure` suffix `tracing-web` appends so call sites collapse into one bucket. Clears measures after sampling. Pass `min_count=0` to include rare init spans (GLTF parse, etc.). |
| `read_upload_ring_stats()` | JSON string | Phase-2.1 mapped-upload-ring telemetry, keyed by subsystem (`transforms`, `materials`, `instances.transforms`, `meshes.meta.*`, …) plus a `_total` rollup. Each entry includes `peak_ring_depth_used / fallback_count / map_async_wait_ms / bytes_uploaded_via_{ring,fallback,writebuffer} / resize_count`. Steady state on `tuning-10k-meshes` should see `_total.fallback_count == 0`; non-zero means a buffer's ring depth (default 3) is too shallow for its frame cadence. |
| `measure_gltf_load_ab(url, iterations)` | JSON string | A/B harness for `GltfParseJob`: returns `{ inline_ms[], worker_ms[], inline_mean, worker_mean, speedup }` so the inline `GltfLoader::load` path can be compared against the worker `pool.dispatch::<GltfParseJob>(..)` path. Canonical reference on M2 Chrome / Corset.glb (12.8 MB): inline **196 ms** / worker **91 ms** → **2.15×**. The editor defaults to worker mode (pre-warmed pool + sticky inline fallback); `?gltf-worker=off` is the dev-only opt-out for re-running the inline baseline. See [§5c](#5c-worker-mode-gltf-parse--default-in-the-editor). |

Per-frame render-pass timings come from
`performance.getEntriesByType('measure')` — `tracing-web`'s
`performance_layer` routes every renderer span through the
browser's Performance API when the renderer is running at the
`SubFrame` tier. `read_render_pass_timings(...)` is the one-shot
summariser if you don't want to walk the entries manually.

URL switches (all dev-friendly, can be combined):

* `?features=off` (debug only) flips `RendererFeatures::default()`
  for A/B comparison without rebuilding the renderer.
* `?trace=off|frame|sub-frame` picks the render-tracing tier at
  runtime — see [perf-tracing.md](perf-tracing.md).
* `?log=info|debug|trace|warn|error|off` overrides the subscriber
  log-level filter (default `INFO`).

Tuning scenes (regenerate with `cargo run --example
generate_tuning_scenes -p awsm-scene-schema`):

- `tuning-1k-meshes` — 1024 boxes + 20 point lights.
- `tuning-64-lights` — 64 mixed punctual lights + 10 spheres.
- `tuning-mixed-intensity` — 20 lights spanning 0.1× → 50×
  intensity at radius 12.
- `tuning-open-world` — 1 km terrain + ocean + props + sun.
- `tuning-coverage` — 100 receding props.
- `tuning-10k-meshes` — 100×100 box grid + sun.
- `tuning-importance-tiers` — 16 lights in a 4×4 (distance ×
  intensity) grid; drives the importance-tier cutoff
  validation.

---

## 10. Known limits / parked optimizations

Use this section to record optimisations that are deliberately
deferred — work the team has considered, found a hazard in, and
decided not to ship until the hazard is addressed.

For each parked item, document the **hazard**, not just a "TODO":
*why* it's parked, what would break if it shipped as drafted, and
what evidence (test scene, trace, repro) the next person needs to
re-attempt it from. The most useful entries are the ones where
the next picker can read "this approach failed because X" and
either fix X or pick a different approach.

Items that ship out of "parked" move to their relevant section
(§4 / §5 / §6 / §8) along with their tuning knobs. "Won't do"
items (§11 below) are intentional permanent non-work, not
parked work.

### Thousands-of-lights tier (storage-buffer lights + GPU light grid)

**What:** support > 1024 punctual lights (e.g. particle-driven point
lights) by (1) moving `lights` from a uniform to a storage buffer and
raising `MAX_PUNCTUAL_LIGHTS`, and (2) adding a GPU-resident light
grid/BVH so the cull's Stage A tests only *nearby* lights instead of all
of them.

**Hazard / why parked:** as *always-on* changes neither is win-or-neutral
for the common (low-light) case — exactly the wrong shape for a general
renderer:
- Uniform → storage adds a storage-buffer binding to the opaque stage,
  which is already near `maxStorageBuffersPerShaderStage` (this device
  reports 10; the spec minimum is 8, and opaque uses 9). It would need
  `lights` merged into an existing storage buffer, and risks excluding
  lower-limit devices. Possible small perf hit for divergent dynamic
  indexing on some mobile GPUs.
- A light grid has fixed per-frame build + indirection + memory cost that
  is a *net loss* when there are few lights to prune.
- The cull's `O(tiles × lights)` Stage A is the only thing that scales
  with light count; the grid is the fix, but the benefit is marginal
  below a few thousand lights and only meaningful at tens of thousands.

**Re-attempt as:** light-count-**gated tiers** (kick in above a
threshold, leave normal scenes untouched), and **profile on a real
high-light scene first** — the CPU rstar tree does *not* transfer (it's
CPU; the froxel cull is GPU-parallel). See §5h for the current design and
why the per-mesh BVH path already covers typical scenes.

**Evidence to start from:** `tuning-1024-lights` (the densest current
fixture) renders correctly and auto-grows to cap 128; it's heavy but
bounded. There is no >1024-light fixture yet — build one (a particle
emitter of unshadowed point lights) before attempting this.

## 11. What *not* to do (preserves correctness)

- **Don't bump `with_max_storage_buffers_per_shader_stage` past
  10.** Adapter caps at 10/10. The opaque main bind group peaks
  at 7/10 today (merged geometry pool); adding a binding past
  10 fails pipeline validation on devices that exactly meet the
  declared limit.
- **Don't introduce a backend trait abstraction.** The
  renderer is web-sys-only (not `wgpu`) by design. WGSL changes
  ship via Askama templates under
  `crates/renderer/src/render_passes/*/shader/`.
- **Don't bypass the visibility-buffer pipeline.** Adding a
  forward pass for "just this one effect" reintroduces the
  wasted-lane tax the visibility-buffer architecture avoids.
- **Don't add per-frame work without a dirty flag.** Everything
  in `render.rs::render` runs once per `requestAnimationFrame`;
  if your data only changes on user input, plumb the write
  through `mark_create` / a dirty bit on the relevant subsystem.
- **Don't iterate `meshes` linearly for per-pass culling.** The
  BVH (`scene_spatial`) is the canonical query path. Mesh-walk
  fallbacks exist only for meshes lacking world AABBs; don't
  generalize them.
- **Don't `set_pipeline` per mesh.** The geometry pass already
  groups by render_pipeline_key; opaque/material/shadow passes
  should do the same.

---

## 12. References

**Architecture:**
- Burns & Hunt, "The Visibility Buffer" (JCGT 2013).
- Schied & Dachsbacher, "Deferred Attribute Interpolation
  Shading" (HPG 2015).
- Wihlidal, "Optimizing the Graphics Pipeline with Compute"
  (GDC 2016) — material classify + indirect dispatch.

**GPU-driven rendering:**
- Haar & Aaltonen, "GPU-Driven Rendering Pipelines"
  (Siggraph 2015).
- Karis, "A Deep Dive into Nanite Virtualized Geometry"
  (Siggraph 2021).

**Decals:**
- de Carpentier & Ishiyama, "Decima Engine: Advances in
  Lighting and AA" (Siggraph 2017).

**Shadows:**
- ESM/EVSM: Annen et al.
- PCSS: Fernando, "Percentage-Closer Soft Shadows."

**Spatial structures:**
- [rstar — RTree](https://docs.rs/rstar/latest/rstar/) is the
  BVH backend for `scene_spatial`.

---

## 13. Updating this doc

This file is the durable reference. When you land a change that
moves performance numbers measurably, or adds/removes a tuning
knob, update the relevant section. The git history is the
audit trail — don't track in-flight work here (that goes in a
PR description or a transient `docs/plans/` file you delete
once shipped).

A good rule: if your PR adds a knob the user can turn, this doc
should mention it in §5 and §7 should describe when to turn it.
