# Renderer performance — permanent reference

This document is the durable guide to `awsm-renderer`'s
performance model: what costs what, how the per-frame pipeline
is structured, which knobs to turn, and where to look when a
profile shows regression. It supersedes the now-archived
`docs/plans/optimizations.md`, which tracked one-off work items
during a series of optimization sessions.

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
│ Material classify    │  Per-tile shader_id bucketing; writes
│ (compute)            │  indirect-dispatch args + tile lists.
└──────────┬───────────┘
           │
┌──────────▼───────────┐
│ Material opaque      │  One dispatchIndirect per shader_id.
│ (compute)            │  Reads visibility + meta + materials,
│                      │  writes opaque_tex.
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
`performance.getEntriesByType('measure')`.

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
when `AwsmRendererLogging.render_timings = true` (always on in
debug builds via `cfg!(debug_assertions)`).

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

### Oversized-mesh light-bucket knobs (`light_buckets::buckets`)

| Knob | Default | Notes |
|---|---|---|
| `OVERSIZED_LIST_COUNT_THRESHOLD` | 16 | Bucket-depth at which the mesh is split out. |
| `OVERSIZED_AABB_DIAGONAL_METERS` | 50.0 | Mesh-size threshold. Floor planes / ocean planes / terrain chunks need this. |

These defaults are validated against the existing tuning
scenes; re-tune only if a real production scene shows the
oversized-classification missing terrain-class meshes.

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
  time the cursor wraps back. `spawn_local` + a shared `Cell<bool>`
  flag in the slot promotes `Pending → Ready` on the main thread.
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
now routes through `MappedUploader` — the original migration table
is fully closed:

- already-`Dynamic` sites: `transforms`, `materials`, `instances`
  ×2, `meshes.meta` ×2, `skins` ×2, `morphs` ×2, `textures.transforms`,
  the three mesh pool buffers.
- raw-writeBuffer promotions (Phase 2.1 second pass): `camera`
  (64 B uniform), `shadows` (globals + descriptors + views),
  `lights` (punctual + info), `mesh_light_indices`, `occlusion`
  (params + instance pack), `lines` (per-line uniform + per-line
  segment).

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

## 5c. Worker-mode gltf parse — measured (faster than inline)

`GltfParseJob` (Phase 4.3b) runs the full fetch + parse pipeline on a
pool worker **AND decodes every embedded image into an `ImageBitmap`
inside the worker** via the `DedicatedWorkerGlobalScope::createImageBitmap`
shim. The decoded handles are *transferred* (not structured-cloned)
across the `postMessage` boundary using the trait hooks
[`WorkerJob::into_response_message`][wj_into] / [`WorkerJob::from_response_message`][wj_from]:
the worker attaches the handles to the response object's `bitmaps`
array AND pushes them into the `post_message_with_transfer` transfer
list. Main thread receives them in O(1) and
[`GltfParseOutput::into_loader`][gp_il] skips its decode step
entirely. Reachable end-to-end via the dev-only `?gltf-worker=on`
URL knob in the scene-editor.

[wj_into]: ../crates/renderer/src/workers/pool.rs
[wj_from]: ../crates/renderer/src/workers/pool.rs
[gp_il]: ../crates/renderer-gltf/src/worker_job.rs

A/B measurement on Corset.glb (12.8 MB, the heaviest single-asset
glb in the shipped samples), n=3 iterations on M2 MacBook /
Chrome:

| Path | Mean load (ms) | Speedup |
|---|---|---|
| Inline `GltfLoader::load` | 196 ms | 1.0× (baseline) |
| Worker `GltfParseJob` → `into_loader()` | **91 ms** | **2.15×** |

The worker path is now **2.15× faster** end-to-end, down from
2× slower in the previous "encoded-bytes round-trip" revision.
The flip comes from moving image decode to the worker — what
used to be a main-thread `createImageBitmap` bottleneck (~150 ms
on Corset) is now zero main-thread time because the bitmaps
arrive pre-decoded via the transfer list.

For smaller assets (DamagedHelmet, ~4 MB, 5 textures): the
break-even point is around 5 MB. Below that, inline and worker
land within noise of each other — the worker spawn + dispatch
overhead matches the savings.

### Worker mode stays opt-in for now

The default `asset_cache::load_and_populate` path stays on
inline — flipping the default would require:

1. A pre-warmed worker pool at editor startup (the current
   on-demand pool builds add ~50 ms to the first asset load
   that would dwarf the 5 MB break-even win).
2. A graceful fallback when the worker bootstrap fails (e.g.
   blob-URL CSP, no `import.meta.url` resolution) — without
   the fallback a CSP misconfigured project would silently
   stop loading assets.

Production consumers wiring worker mode build their own pool
at startup, register the job with
`pool.register::<GltfParseJob>()`, and route through
`GltfParseOutput::into_loader()`. Worker mode is the right
pick when:

- Assets are large (≥ 5 MB) and the latency-vs-throughput
  trade favours throughput.
- The main thread runs game logic / audio / network code that
  can't tolerate the parse-blocking latency.

### Unsupported formats fall back

`createImageBitmap` rejects unsupported formats (KTX2, Basis,
etc.). The worker tags those entries with `bitmap: None` and
keeps the encoded bytes — `into_loader` re-decodes them on the
main thread via the pure-Rust `image` crate path. Same
behaviour as the inline loader, no observable difference at
the populate step.

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

Per-frame cost on tuning-10k-meshes: the new "Shadow Receiver
Gate" span shows **0.048 ms mean / 0.1 ms p95** — well under the
~0.1 ms it saves the geometry pass.

### PCSS variable tap count by distance

Both PCSS branches (cube `sample_shadow_cube`, directional
`sample_shadow_cascade_array`, 2D spot `sample_shadow_descriptor`)
now taper their tap count linearly from `shadow_globals.flags.z`
(close, "max taps", default **16**) to `shadow_globals.flags.w`
(far, "min taps", default **4**). The interpolation parameter is
the receiver's normalised distance to the light:

- Cube: `dist / range` (receiver-to-light distance / light range)
- Directional / 2D: `ndc.z` (receiver's NDC.z within the cascade)

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

### The "first-visible-frame stutter" we observed

After the "finish-every-optimisation" commit, the WGSL source
for every material pipeline changed (added
`shadow_receiver_gate: u32` to `MaterialMeshMeta`, changed the
four `apply_lighting*` callsites). Every user's first page load
post-deploy hit a cache miss and recompiled ~12 pipelines
serially in the render loop. The first Insert Model was a
multi-hundred-ms stall; the second was instant.

A production game should not ship that behaviour. The fix is to
explicitly drive a draw of every routinely-used pipeline during
the load screen, where the user is already looking at a
progress bar.

### The recipe (not yet wired as a renderer API)

The minimum-viable shader-warmup pass walks the
`Materials::enabled_materials()` table, picks one mesh per
`MaterialShaderId` × `is_transparency_pass` × MSAA mode, and
ensures it draws at least once during init. For first-party
materials that's PBR / Unlit / Toon × {opaque, transparent} ×
{MSAA off, MSAA on} × {mipmaps off, mipmaps on} = up to **24
pipelines** to warm before the user can touch the viewport.

In the editor's case, the simplest concrete step is to unhide a
gizmo mesh from each material shader for one rAF tick, then
re-hide. That forces `collect_renderables` to route the
gizmo through every active pipeline, the geometry / opaque /
transparent / shadow passes to draw something, and WebGPU to
compile.

### Interaction with the planned dynamic-materials sprint

This warmup story gets **more important** once the
[`docs/plans/dynamic-materials.md`](plans/dynamic-materials.md)
work lands. That plan adds runtime registration of custom
material shaders — `MaterialDefinition` data + a `shader.wgsl`
fragment, both registered at startup (or mid-frame, with a
recompile). Two new wrinkles to handle in the warmup:

1. **Custom shader_ids aren't known until the consumer
   registers them.** First-party materials are enumerable at
   compile time (`enabled_materials()`); dynamic ones come from
   `MaterialRegistry::register()`. The warmup pass needs to run
   *after* every dynamic registration the consumer cares about
   — usually right after game-init finishes loading material
   defs, before the first gameplay frame.
2. **Mid-frame registration forces a recompile.** The dynamic-
   materials plan calls this out explicitly: registering a new
   material mid-frame busts the cached opaque-compute /
   transparent-fragment pipelines (the dispatch chain text
   changes). A game that streams in custom materials during play
   would see exactly the same stutter pattern this section
   describes, but mid-gameplay instead of at boot. The fix is
   the same — pre-warm by drawing one mesh per
   newly-registered shader_id before the next user-interactive
   frame.

So whoever picks up the dynamic-materials sprint should land a
**`AwsmRenderer::prewarm_pipelines()` API** alongside the
registry work: walks every active (shader_id × variant) combo
the renderer knows about, fakes a one-pixel draw to compile,
then drops the fake renderables. Callers invoke it at end of
game-init AND after every burst of dynamic registrations
they're about to surface to the player. Until that API exists,
shipping consumers should follow the load-screen trick in §8a
step 5.

The cost of the warmup pass *is* the compile tax — it doesn't
make compilation faster, it just relocates it to a frame the
user expects to be slow (the splash) instead of one they expect
to be instant (the first interactive draw). On Chrome with a
warm GPU disk cache, the warmup pass itself takes <5 ms; on a
cold cache (first-ever visit, post-redeploy reload), it takes
50–500 ms depending on how many pipelines and how heavy the
shader. That cost is unavoidable — what changes is *when* the
user pays it.

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

Re-captured after the "finish-every-optimisation" sprint
(coverage-driven skin-skip with grace + BVH override, cheap-material
LOD routing live, shadow-receiver gate, PCSS variable taps,
worker-mode gltf with in-worker image decode). The mean frame
budget is identical to the pre-sprint baseline — the new
per-mesh `Shadow Receiver Gate` walk costs 0.048 ms and is offset
by the PCSS-tapered shadow generation cost (-0.02 ms).

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
   browser's Performance tab) to find the dominant span.
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
| `AwsmRendererLogging::render_timings` | `false` | The per-pass `tracing::span!` `performance.measure()` calls only fire when this is on. Off by default → zero overhead for shipped games. |
| Mapped-buffer ring (Phase 2.1) | Always on | Every per-frame `writeBuffer` site is routed through `MappedUploader`. 99.9999% of bytes go through the mapped fast path on 10k meshes. |
| Coverage-driven skin-skip (§5d) | Always on | Off-screen skins stop animating after a 2-frame grace; in-frustum skins resume that same frame via the BVH override. |
| Shadow-receiver gate (§5f) | Always on | Meshes no caster reaches skip the entire shadow-sample chain. 0.048 ms / frame to maintain on 10k meshes. |
| PCSS variable taps (§5f) | 16 → 4 by distance | Far receivers cost 4× less; near receivers get full quality. |
| Adaptive optimization policy | On with `Auto` cooldown | `RendererOptimizationPolicy` flips `indirect_first_instance`, `occlusion`, `coverage_lod` etc. based on per-frame signals. Manual override only for A/B testing. |
| Scene-spatial BVH | `rebuild_dirty_threshold: 200`, `rebuild_period_frames: 600` | Right for 1K–5K dynamic meshes. Bump for 10K+ static-heavy scenes. |
| `default_cheap_material_pixel_threshold` | 64 px | Below this, the cheap variant takes over on any mesh that has one authored. Per-mesh override always wins. |

### What a game must do at startup

#### 1. Pre-warm the worker pool (for mid-gameplay asset loads)

The `asset_cache::load_and_populate` path defaults to inline
because the on-demand worker spawn cost dwarfs the win on small
assets. For shipped games loading additional content during play
(level streaming, animation rigs, audio-paired assets, …), build
a pool at startup so the dispatch cost is amortised:

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
worker 91 ms = **2.15× speedup** with the in-worker
`createImageBitmap` + transfer path.)

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
invalidates, and how this will interact with the upcoming
dynamic-materials sprint.

The short version for game-shipping: unhide one mesh per
`MaterialShaderId` (PBR / Unlit / Toon) — plus the MSAA-on
variants if your game lets the player toggle MSAA — briefly
during the load screen so WebGPU compiles their pipelines
while the user is still looking at the splash, not at a frozen
viewport on first gameplay frame.

### Tuning knobs by play style

| Game style | What to bump | Why |
|---|---|---|
| **Twin-stick action / racing** | `ShadowsConfig::pcss_min_taps: 8` | Camera moves fast; smoother far shadows hide cascade snap. |
| **First-person exploration** | `ShadowsConfig::pcss_max_taps: 24` (clamped to 16 → stays at 16) | Camera is stationary; near shadows benefit from quality. |
| **Crowd / RTS** | `default_cheap_material_pixel_threshold: 128`, `SkinLodLevel { 50.0, period: 8 }` | Most meshes are far. Aggressive material LOD + slow skinning. |
| **Mobile / low-end desktop** | `ShadowsConfig::point_shadow_resolution: 512`, `pcss_min_taps: 4` | Cut VRAM (4× drop) + cheaper shadows. |
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
| `measure_gltf_load_ab(url, iterations)` | JSON string | Phase-4.3b A/B: `{ inline_ms[], worker_ms[], inline_mean, worker_mean, speedup }`. Drives the flip-to-default decision on `asset_cache::load_and_populate`. Result on Corset.glb (12.8 MB): inline 184 ms / worker 24209 ms → **inline stays default**. The bottleneck is `serde_wasm_bindgen` encoding the multi-MB `Vec<u8>` payloads (buffers + image bytes) into JS-native nested Objects before structured-clone across postMessage. See [§Worker-mode gltf parse](#worker-mode-gltf-parse) below for what's needed to make worker mode viable. |

Per-frame render-pass timings come from
`performance.getEntriesByType('measure')` — `tracing-web`'s
`performance_layer` routes every renderer span through the
browser's Performance API. `read_render_pass_timings(...)` is the
one-shot summariser if you don't want to walk the entries
manually.

URL switch `?features=off` (debug only) flips
`RendererFeatures::default()` for A/B comparison without
rebuilding the renderer.

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

Nothing in this section right now — the previously-parked items
(coverage-driven skin-skip, cheap-material LOD routing, shadow-
receiver gate, PCSS variable taps) all landed in the
"finish-every-optimisation" sprint commit; their behaviour and
tuning knobs are documented in their respective sections of this
file (§4 / §5 / §6 / §8). What remains in "Won't do" (§11) below
is genuinely intentional non-work, not deferred work.

If a parked item lands here in the future, document the *hazard*
(why it's parked, not just "TODO") so the next picker has
something concrete to design against — the prior round's
"freezes self-occluded submeshes in their last-skinned pose"
note is what unblocked the grace-period + BVH-override design
when the work resumed.

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
