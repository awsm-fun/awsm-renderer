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

`RendererFeatures` (`crates/renderer/src/features.rs`) gates two
clusters of always-on infrastructure. **Both default to
`false`**, so library consumers pay zero overhead for features
they don't use. Game-side and editor builds opt in explicitly.

```rust
RendererFeatures {
    gpu_culling: bool, // HZB + occlusion cull + compaction +
                       // drawIndirect geometry path
    decals: bool,      // projection-decal classify, compute,
                       // composite + ~33 MB GPU at 4K
}
```

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
draws). **Set `gpu_culling = false` for scenes that stay under
~500 meshes.**

`insert_decal()` returns `AwsmDecalError::FeatureNotEnabled`
when `features.decals = false`. Misuse fails loud rather than
silently dropping decals.

In debug builds the scene-editor honors `?features=off` as a
URL switch for A/B measurement. Release builds skip the URL
parse entirely.

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
    .with_features(RendererFeatures { gpu_culling, decals })
    .with_shadows_config(ShadowsConfig { ... })
    .with_anti_aliasing(AntiAliasing { msaa_sample_count, mipmap })
    .build()
    .await?;
```

| Knob | Where | Default | Effect |
|---|---|---|---|
| `RendererFeatures::gpu_culling` | features.rs | `false` | Enables HZB + occlusion cull + drawIndirect. Worth it ≥ 500-mesh scenes; small net loss below that. |
| `RendererFeatures::decals` | features.rs | `false` | Allocates ~33 MB at 4K. Required for `insert_decal`. |
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
    cheap_material_key: Option<MaterialKey>,  // distance LOD swap
    cheap_material_pixel_threshold: Option<u32>, // None → renderer default
    skin_update_period: u8,                   // 1=every frame, 2=half, etc.
    billboard_mode: BillboardMode,            // camera-facing override
    // ...
}
```

`AwsmRenderer::set_mesh_skin_update_period_by_distance` lets the
caller distance-LOD skinning frequency at a stroke. Coverage-zero
meshes already skip skinning entirely (consumer ⇄ producer wired).

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

## 6. Hot-path catalogue

When optimizing or reviewing a PR, these are the files that
move the needle:

| File | What lives here | Watch for |
|---|---|---|
| `render.rs::AwsmRenderer::render` | The per-frame entry point. Wraps every other pass. | New work added here regresses every frame. Be sure new GPU writes are gated on a dirty flag. |
| `renderable.rs::collect_renderables` | Builds the per-frame opaque/transparent/HUD lists. Runs every frame. | Per-mesh allocations or material-key recomputation. The BVH query + per-mesh `effective_material_key` are the only intended work. |
| `meshes/mesh.rs::push_geometry_pass_commands` | Per-mesh draw recording. | Vertex/index buffer rebinds. Instanced meshes still use the legacy uniform-with-dynamic-offset path; non-instanced use storage-array meta + drawIndirect. |
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

1. Check `occlusion_instances[i].mesh_meta_offset` is populated
   correctly. Pre-§16.7/§16.8 rewire it was a `0u32`
   placeholder; the compaction shader divides by 256 to derive
   the slot index, so a wrong offset is silently a no-op.
2. Confirm the geometry meta + material meta slot indices for
   the same `MeshKey` align (they should because both
   `SecondaryMap`s are inserted/removed in lockstep). The
   coverage producer uses *material* slot indices because
   visibility_data carries `material_mesh_meta_offset`.
3. Instanced meshes are intentionally on the legacy
   `draw_indexed_with_instance_count` path — `instance_index`
   ranges would otherwise collide between meshes in the shared
   storage-array meta lookup.

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

Per-frame render-pass timings come from
`performance.getEntriesByType('measure')` — `tracing-web`'s
`performance_layer` routes every renderer span through the
browser's Performance API. No custom JSON harness required.

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

## 10. What *not* to do (preserves correctness)

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

## 11. References

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

## 12. Updating this doc

This file is the durable reference. When you land a change that
moves performance numbers measurably, or adds/removes a tuning
knob, update the relevant section. The git history is the
audit trail — don't track in-flight work here (that goes in a
PR description or a transient `docs/plans/` file you delete
once shipped).

A good rule: if your PR adds a knob the user can turn, this doc
should mention it in §5 and §7 should describe when to turn it.
