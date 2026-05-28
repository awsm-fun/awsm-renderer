# GPU light culling — design plan

**Branch**: `light-culling`. **Status**: design only; no implementation yet.

This is a tailored proposal — it builds on the renderer's existing
visibility-buffer pipeline, the per-mesh CPU bucket system in
[`crates/renderer/src/light_buckets/`](../../crates/renderer/src/light_buckets/),
and the empty pass scaffold already wired up at
[`crates/renderer/src/render_passes/light_culling/`](../../crates/renderer/src/render_passes/light_culling/).
It is **not** a generic clustered-forward whitepaper.

A few open questions at the bottom need user input before implementation
starts.

---

## What we already have

- **Per-mesh CPU light buckets**
  ([`light_buckets/buckets.rs`](../../crates/renderer/src/light_buckets/buckets.rs)).
  For each active punctual light, `SceneSpatial::query_envelope` returns the
  meshes whose world AABB overlaps the light's influence sphere; the transpose
  gives each mesh a short `Vec<u32>` of light indices. The per-mesh slice is
  patched into the mesh's `MaterialMeshMeta` (offset + count), so the opaque
  shader reads it for free as part of the meta load already on the hot path.
- **Directional-light global prefix** — directional lights bypass the per-mesh
  slice entirely. Every opaque pixel loops over the small prefix (~4 typical)
  unconditionally.
- **Oversized-mesh flagging** — meshes whose AABB diagonal exceeds 50 m AND
  who land in a bucket with more than 16 lights get flagged in
  `LightMeshBuckets::oversized_meshes`. Nothing currently consumes the flag;
  the design here is what consumes it.
- **Transparent path is flat** — `material_transparent` does NOT use per-mesh
  slices; every transparent fragment loops over **all** punctual lights
  (`MAX_PUNCTUAL_LIGHTS = 1024`, but the live count is what's in the buffer).
- **Visibility buffer + per-sample depth** — `vis_data` (Rgba16Uint) gives
  per-sample mesh identity; depth is multisampled when MSAA is on.
- **Empty pass scaffold** at
  [`render_passes/light_culling/`](../../crates/renderer/src/render_passes/light_culling/)
  — bind_group, pipeline, render_pass, shader/template all exist as stubs.
  `render.rs` already calls `self.render_passes.light_culling.render(&ctx)?`
  between shadow generation and material classify.

---

## What this design adds and why

The renderer is **CPU-coarse / GPU-zero today** for light culling. That's
fine on the opaque path for scenes where every mesh is small relative to
each light's influence volume — the per-mesh slice keeps the per-pixel
loop short. It breaks down in three places:

1. **Oversized meshes** — a single floor mesh spanning the room enters every
   point-light bucket. Every pixel of that mesh then loops over every
   point light, regardless of whether the pixel's actual world-space
   position is inside that light's range.
2. **Transparency** — flat per-pixel loop over all 1024 slots is wasteful
   when a screen tile only sees 2–4 lights.
3. **Z-cull** — a point light behind the camera, or in front of the
   near depth, still passes the CPU AABB overlap test against any mesh
   sticking through it. No frustum / depth gate is applied.

We want a **per-tile light list on the GPU** that catches all three cases.
The CPU per-mesh path stays — it's the right answer for small meshes —
but the GPU per-tile list becomes the authoritative cull for:

- Every transparent fragment.
- Every fragment of a mesh flagged `oversized`.
- Optionally every opaque fragment (open question A).

The new GPU pass does **2D screen-tile culling with conservative
view-space Z extents per tile** (Forward+ shape), with an opt-in path
toward 3D froxels later (see "Future: 3D froxel clusters" below).

---

## Where in the frame the cull runs

The existing scaffold places `light_culling.render(...)` between shadow
generation and material classify
([`render.rs:584`](../../crates/renderer/src/render.rs#L584)). I'm keeping
that slot. Rationale:

- **After geometry pass** — depth and `normal_tangent` are written, so the
  tile can compute its true min/max view-space depth from the depth
  attachment instead of using the camera near/far. Per-tile depth-bounds
  culling cuts roughly half of the candidate lights in typical interiors.
- **After shadow gen** — shadow gen reads the per-mesh shadow-receiver
  flag (already populated CPU-side), independent of per-tile culling. No
  ordering hazard.
- **Before material classify** — classify is per-tile too (8×8
  workgroups, [`material_classify/render_pass.rs`](../../crates/renderer/src/render_passes/material_classify/render_pass.rs)).
  The opaque pass that consumes classify's per-bucket tile lists also
  reads the per-tile light list — both bindings land on the
  opaque-main bind group. Same tile coordinate system means zero
  conversion at shading time.
- **Before transparent pass** — the transparent pass binds the same
  per-tile light list and uses it as a direct replacement for its
  current flat-all-lights loop.

This means the cull pass runs **once per frame** at a single point in the
graph; its output is consumed by classify (optionally), opaque (optionally
— see open question A), and transparent (definitely).

**Why not after material classify?** Because the per-tile light list
benefits classify's edge-detection (a tile crossing a sharp light cutoff
is automatically a per-pixel light edge — though we don't need that for
correctness today, it's worth noting for future fine-grain culling).

**Why not after opaque shading?** Because opaque shading is the biggest
consumer; running the cull after it forces transparent to keep its
flat loop.

**Why not run two culls — pre-opaque and post-opaque?** Tempting (the
post-opaque cull would have the final depth buffer including all opaque
surfaces, giving tighter Z bounds for transparency), but the cost is one
extra compute dispatch that would only matter for transmissive materials
that bend the depth budget. Defer this; the geometry-pass depth is a
good-enough upper bound for transparent.

---

## Tile size

**16×16 pixels per tile**, not 8×8. Reasoning:

- Classify uses 8×8 because its work per tile is tiny (a single visibility
  scan + bucket-append). Light culling has much heavier per-tile work
  (per-light frustum / depth-bound test, atomic appends to a per-tile list)
  and benefits from amortizing that work over a larger tile.
- 16×16 = 256 threads per workgroup, which lines up with the typical
  WebGPU `maxComputeWorkgroupSizeX × Y` = 256 we already use elsewhere.
- The light list itself is per-tile, so the size of the output buffer
  scales as `(viewport_w / 16) × (viewport_h / 16)` — at 1920×1080 that's
  120×68 = 8160 tiles. With a per-tile budget of 32 light indices, the
  output buffer is ~1 MiB. At 4K that's ~4 MiB. Both fit comfortably.
- 16×16 is also the standard Forward+ pick — most of the literature
  reference data is at this size, easier to compare results to.

The 8×8 classify tiles still exist; light culling's 16×16 tiles are
strict multiples (one cull tile covers a 2×2 block of classify tiles).
The shading shader can map either way without conversion math.

---

## Algorithm sketch

Per cull tile (16×16 px), one workgroup:

1. **Phase 1 — depth bounds.** Each thread loads its pixel's
   depth from the depth attachment (sample 0 if MSAA is on; sample-min
   would be tighter but matches the existing standard of "sample 0 is
   the canonical depth" — see `helpers/standard.wgsl`). Workgroup-shared
   reductions produce `(z_min, z_max)` in view space.
2. **Phase 2 — frustum + sphere test.** Each thread iterates a chunk of
   the active punctual-light list. For each light:
   - Reconstruct the light's world-space bounding sphere (already in
     the light's row 1 as `pos_range`).
   - Convert the tile + (z_min, z_max) bounds into a view-space frustum
     (4 side planes from the tile's screen-space pixel rect, 2 near/far
     planes from the depth reduction).
   - Sphere-vs-frustum test. (Cheap closed-form: distance from sphere
     center to each plane, reject if any > radius.)
   - For spot lights, additionally test the cone direction against the
     tile's view direction. Conservative — false-positives are fine;
     missing a light isn't.
3. **Phase 3 — append.** Lights that pass the test are atomic-appended
   to that tile's light-index list. Capacity per tile is fixed (see
   "Memory budget" below). Overflow tags the tile as "saturated"; the
   shading shader falls back to looping over the **whole** punctual
   buffer for that tile (degraded perf, correct shading — same fallback
   as the existing edge-budget overflow handling in Stage 3).
4. **Phase 4 — directional prefix.** Directional lights aren't culled
   (they have no bounded volume). The existing global-prefix path stays
   — every shaded pixel walks the small directional list unconditionally.

Workgroup size: 16×16 = 256 threads, one per pixel of the tile. The
per-light test loop is over `ceil(active_punctual_count / 256)` lights
per thread.

---

## Output buffer shape

Two storage buffers:

- `tile_light_offsets: array<vec2<u32>>` — one (offset, count) per tile.
  Indexed by `tile_y * tiles_per_row + tile_x`. Count is 0 for tiles
  with no lights; offset is the start index into the indices array.
  Equivalently: a single `array<u32>` where `[2*i]` is offset and
  `[2*i+1]` is count, packed tightly.
- `tile_light_indices: array<u32>` — flat array of `light_index` (the
  same index the per-mesh path uses, i.e. position in
  `Lights::iter()`). Each tile's slice is `[offsets[t].offset ..
  offsets[t].offset + offsets[t].count]`.

Per-tile capacity: 32 light indices (= 128 bytes). At 1920×1080: 8160 ×
128 = ~1 MiB worst-case. Allocated once at viewport size; reallocated
(via the standard `ensure_capacity → mark_create` pattern) when the
viewport grows.

A **saturation bit** lives in the count: count's high bit (1 << 31) is
set when the tile overflowed. The shading shader checks the bit and
either trusts the truncated list (clear) or falls back to looping the
full punctual buffer (set). The bit cost is one bit of headroom in the
24-bit-ish realistic light counts.

---

## How the shading shaders consume it

### Opaque (per-pixel, per-shader_id)

Currently each opaque pixel reads its mesh's `MaterialMeshMeta` and
loops over the per-mesh light slice. Under this design:

- **Default (small-mesh) path** — unchanged. The per-mesh slice IS the
  short list; per-tile culling is bypassed. We hit this whenever the
  mesh's `OVERSIZED` flag (already maintained CPU-side) is false.
- **Oversized-mesh path** — the per-mesh slice is replaced by the
  per-tile slice (per-tile cull gives a per-pixel list scoped to where
  the mesh actually is). Detection: `MaterialMeshMeta.mesh_light_slice.count`
  encodes a sentinel value (e.g. `count = 0xFFFFFFFF`) meaning "use
  per-tile list instead." Set by `MeshLightIndicesGpu::write_gpu` when
  the mesh is in `LightMeshBuckets::oversized_meshes`.

The shader branch becomes:

```wgsl
let slice = material_mesh_meta.light_slice;
if (slice.count == 0xFFFFFFFFu) {
    // Oversized → tile path
    let tile = pixel_to_cull_tile(input.coords);
    let entry = tile_light_offsets[tile];
    let count = entry.count & 0x7FFFFFFFu;
    let saturated = (entry.count & 0x80000000u) != 0u;
    if (saturated) {
        // Full-list fallback (degraded; rare).
        for (var i = 0u; i < info.light_count; i++) { … }
    } else {
        for (var i = 0u; i < count; i++) {
            let li = tile_light_indices[entry.offset + i];
            …
        }
    }
} else {
    // Normal mesh-bucket path (unchanged).
    for (var i = 0u; i < slice.count; i++) {
        let li = mesh_light_indices[slice.offset + i];
        …
    }
}
```

The `pixel_to_cull_tile` helper is `(coords.xy / 16u)`. The branch is
uniform across each 16×16 tile (a tile contains only one mesh for the
oversized case the branch fires on — actually that's not true; a tile
can contain both small and oversized meshes. But the branch is
per-pixel and the predicate is uniform per-mesh: every fragment of the
oversized mesh takes the tile path, every fragment of small meshes
takes the mesh path. No wave divergence.)

### Transparent (per-pixel, per-shader_id)

Currently flat-loops over all punctual lights. Under this design:
**always** use the per-tile list. Same branch as the oversized-opaque
path; no per-mesh sentinel needed (every transparent fragment uses the
tile path unconditionally). This is the biggest win — transparent
fragments today are unbounded by per-mesh culling, and 32–1024 lights
per pixel is meaningful waste.

### Why not put every opaque fragment on the tile path?

Considered. Two reasons against making it default:

1. **The per-mesh slice is cheaper for small meshes.** A typical mesh
   touches 1–4 lights; the per-tile list might have 8–16. The per-mesh
   loop is shorter for the common case.
2. **The per-mesh slice already passes a stronger AABB test** — it knows
   which meshes a light overlaps, which the tile-test doesn't. The
   tile-test sees "a light's sphere intersects this tile's frustum" but
   that doesn't mean the mesh inside the tile is in the light's range.

Open question A asks whether to flip this default. Today's design says
keep per-mesh as default for small meshes; tile is the override for
oversized + transparent.

---

## Memory budget

At 1920×1080:

- `tile_light_offsets`: 8160 × 8 bytes = 64 KiB. Persistent.
- `tile_light_indices`: 8160 × 32 × 4 bytes = ~1 MiB. Persistent.
- Per-frame staging via the existing `MappedUploader` pattern: ~16 KiB
  of camera-derived frustum bounds + light bounds metadata (uploaded
  once per frame).

At 4K (3840×2160): 4× the above — 256 KiB + 4 MiB. Still trivial.

Per-tile capacity = 32 is a guess. From the CPU bucket stats we already
have (`LightMeshBuckets::last_max_bucket`), realistic interior scenes
have at most ~10 lights per tile. 32 leaves headroom; saturation
fallback covers the rare pathological case.

---

## Pipeline + bind-group plan

The empty scaffold already imports `ShaderTemplateLightCulling` and a
shader-cache-key struct. Filling them out:

### Bind group layout (one bind group, slot 0)

| Binding | Resource | Notes |
|---|---|---|
| 0 | `camera_raw: uniform` | View/proj matrices for frustum reconstruction. Already on `ctx.camera.gpu_buffer`. |
| 1 | `depth_tex: texture_depth_multisampled_2d` (msaa-on) / `texture_depth_2d` (msaa-off) | Sample 0 reads only. |
| 2 | `lights_info: uniform` | Total `light_count` for per-thread chunk size. |
| 3 | `lights_punctual: uniform` | The packed `LightPacked` array (already at `ctx.lights.gpu_punctual_buffer`). |
| 4 | `tile_light_offsets: storage<read_write>` | Output. |
| 5 | `tile_light_indices: storage<read_write>` | Output. Atomics on the per-tile append counter inside `offsets`. |

5 storage/uniform bindings + 1 texture = 6 bindings on one group. Well
inside the per-stage cap.

A **second bind group** is needed for the MSAA cache key variant
because `texture_depth_multisampled_2d` vs `texture_depth_2d` are
different binding types and the template needs a cache-key flag. The
cache-key shape is:

```rust
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyLightCulling {
    pub msaa: bool,
}
```

Two pipelines compile up-front (or lazily — see "Lazy or eager" below).

### Pipeline-readiness integration

Per the [pipeline-readiness scheduler](../../crates/renderer/src/pipeline_scheduler/),
this is a new `PassDef::LightCulling { samples: u8 }`. Two variants
(samples=1, samples=4) submit to the scheduler and transition Pending →
Ready independently. The render-frame preamble's `warn_pipeline_not_compiled`
already covers "skip if Pending"; if the cull pipeline isn't ready yet,
the shading shaders fall back to the per-mesh / flat path (the existing
behavior).

### Lazy or eager?

**Lazy on first scene with at least one punctual light.** A zero-scene
shouldn't pay the compile cost. The trigger is the first call to
`insert_light` with `Light::Point` or `Light::Spot` — mirrors the
shadow / EVSM lazy-compile pattern from PR #99 Block B. Until that
trigger fires, the existing scaffold's `render(&ctx)` is a no-op and
the shading shaders take the existing per-mesh path.

---

## Bind-group integration on the consuming side

The opaque + transparent main bind groups gain two new entries
(`tile_light_offsets`, `tile_light_indices`). That requires:

- Two new `BindGroupCreate` events: `LightCullingTilesResize` (when the
  output buffers grow due to viewport resize) → rebinds opaque main,
  transparent main, light_culling itself.
- Layout-key bump on opaque + transparent main BGLs. This is a
  pipeline-cache-key change — all opaque + transparent pipelines
  recompile once, then stay.

That's a one-time recompile cost; subsequent runs hit the cache.

---

## Phasing

Suggested order so each phase is shippable on its own and the next
phase has something to A/B against:

### Phase 1 — pass scaffold + transparent-only consumption

- Fill in the bind group, pipeline, cache-key, shader template.
- Write the WGSL: per-tile depth reduce + sphere test + atomic append.
- Wire transparent shaders to read `tile_light_offsets` /
  `tile_light_indices` instead of flat-looping.
- Opaque path stays on per-mesh slice (unchanged).

**Acceptance**: a scene with 32+ point lights and a transparent material
in front of a wall renders correctly; per-frame profile shows
transparent shader cost dropping (the loop is now bounded). No opaque
regression.

### Phase 2 — oversized-mesh routing for opaque

- `MeshLightIndicesGpu::write_gpu` writes the `count = 0xFFFFFFFF`
  sentinel into oversized meshes' `MaterialMeshMeta` slot.
- Opaque shader gains the per-tile branch behind the sentinel.

**Acceptance**: a scene with a single floor mesh spanning a room with
32 point lights renders correctly; per-pixel light count drops from
"all 32" to "only the lights overlapping that pixel's tile."

### Phase 3 — saturation fallback hardening

- Stress-test with 64+ lights in a single tile.
- Confirm the saturation bit triggers the full-list fallback and
  output is visually identical to a non-saturated frame (perf is the
  only thing that differs).

### Phase 4 — opt-in "tile-first" for all opaque (open question A)

If user wants this: flip the default branch in the opaque shader, gate
the per-mesh path behind a debug toggle for A/B comparison.

### Phase 5 — Z-tighter cull (open question B)

If user wants higher cull rate: switch the per-tile depth reduce from
sample-0 to sample-min/max (multisampled reads), or run a coarse
HZB-style mip reduction over the depth buffer before the cull pass.

### Phase 6 — 3D froxels (open question C)

If the 2D-tile cull is leaving too many lights in tall tiles (e.g.
high-rise interiors where the tile's Z range spans 50 m), upgrade to
froxels (16×16 screen tiles × 32 view-space depth slices, exponential
spacing). The shader change is a depth-to-slice mapping plus one extra
index into the `tile_light_offsets` lookup.

---

## What this design deliberately does NOT do

- **Transparent meshes' depth contribution.** Transparent surfaces
  don't write depth before the cull runs; the per-tile depth bounds
  come from the opaque depth buffer only. Lights in front of opaque
  geometry but behind transparent geometry are still in the tile's
  light list — correct, conservative.
- **Per-pixel light-list reconstruction.** Some renderers do a
  "subgroup-shuffle" per-pixel light list. Not needed here — the
  per-tile list is already short enough (≤32) that per-pixel iteration
  is cheap.
- **Light-list compaction between frames.** Each frame writes its
  own list from scratch. Temporal reuse would buy nothing while the
  camera moves; static-camera scenes don't need the optimization either
  (the cull pass is already cheap).
- **Mesh-shader / mesh-shader-like culling at the light level.** Not
  supported on WebGPU.
- **Replacing the directional-light global prefix.** Directional lights
  don't benefit from spatial culling (they affect every pixel by
  definition). They stay in the small global-prefix loop.

---

## Where it touches existing code

Read-only references (no edits expected):

- `crates/renderer/src/light_buckets/buckets.rs` — `oversized_meshes()`
  consumed by `MeshLightIndicesGpu::write_gpu`.
- `crates/renderer/src/lights.rs` — `Lights::iter()` order is the
  light-index space the per-tile list uses.

Files that grow (estimated diff size):

- `crates/renderer/src/render_passes/light_culling/` — ~600 lines of
  Rust + WGSL across the 4 files now empty.
- `crates/renderer/src/bind_groups.rs` — +1 `BindGroupCreate` variant
  (`LightCullingTilesResize`) + the routing match-arm. ~20 lines.
- `crates/renderer/src/render_passes/material_opaque/bind_group.rs`
  and `material_transparent/bind_group.rs` — +2 bindings each on the
  main bind group; recreate paths gain two new buffer entries. ~30
  lines total.
- `crates/renderer/templates/material_opaque_wgsl/...` and
  `material_transparent_wgsl/...` — the per-pixel light-list branch.
  ~50 lines of WGSL.
- `crates/renderer/src/light_buckets/gpu.rs` — sentinel write for
  oversized meshes in `write_gpu`. ~10 lines.
- `crates/renderer/src/pipeline_scheduler/` — register the new
  `PassDef::LightCulling` variant + its compile path. ~40 lines.

Total: ~750 lines, mostly mechanical wiring.

---

## Acceptance criteria (end-to-end)

The plan is "done" when:

1. A model-tests scene with `MAX_PUNCTUAL_LIGHTS / 4 ≈ 256` point lights
   distributed across the room, plus a soft-glass transparent material,
   renders at 60 fps at 1080p on the dev machine. Today this scene
   (if it existed) would be transparent-shader-bound on the flat loop.
2. The cull-pass profile shows ≤200 µs per frame at 1080p with 256
   lights and 8160 tiles.
3. The oversized-floor-mesh test scene shades correctly with a
   per-pixel light count matching ground truth (verified by debug
   visualization: each pixel colored by `count`).
4. MSAA on/off both work — the cull pass uses depth sample 0 only,
   matching the existing `get_standard_coordinates` convention.
5. No regression on the small-mesh scenes that currently use the
   per-mesh slice — same frame time, same visual output (byte-identical
   pixel reads via `getImageData` per the existing MSAA debugging
   methodology in `docs/DEBUGGING-PREVIEW.md`).

---

## Open questions for user

### A. Default for opaque shading: per-mesh slice or per-tile list?

The proposal above says **per-mesh is default, per-tile is the
oversized-mesh override.** That keeps perf identical for small-mesh
scenes (the common case) and only changes behavior where the per-mesh
slice is already a bad fit.

Alternative: flip the default and make every opaque fragment use the
per-tile list. Pros: one code path, simpler shader; possibly tighter
culling on average. Cons: regress small-mesh scenes that already work
well; the per-tile list is *more conservative* than the per-mesh AABB
test for individual pixels of small meshes.

**Question**: keep per-mesh as the opaque default (recommended), or
flip to per-tile-first?

### B. Depth reduce: sample-0 or sample-min/max?

The geometry pass writes per-sample depth (4 samples in MSAA mode).
Sample 0 is the canonical "representative" depth used everywhere else
in the renderer (`get_standard_coordinates`). The cull's depth bounds
could:

- Use sample 0 only — simple, matches existing convention. May
  slightly over-cull lights at silhouette edges (the tile's "true" Z
  range is tighter than the sample-0 range there).
- Reduce over all 4 samples — tighter bounds, ~4× more depth fetches
  per cull-thread.

**Question**: sample 0 (recommended for simplicity + correctness with
the existing convention), or sample-min/max?

### C. 3D froxels in Phase 6?

The 2D-tile cull is correct but loose in tall tiles (a tile that spans
near-camera + far-wall depth ranges sees the union of both light
sets). 3D froxels (e.g. 16×16×32) split the tile's depth range into
exponential slices and cull per-slice. Real Forward+ shape.

This is real work — maybe 1–2 days, mostly the depth-to-slice mapping
+ the per-slice output buffer (~4× the 2D buffer size). Pays off only
when scenes have wide depth ranges with many lights at different
depths.

**Question**: ship Phase 6 froxels as part of this work (answer "yes —
go straight to 3D"), keep it as a follow-up (answer "later"), or skip
it entirely (answer "2D tiles are enough")?

### D. Lazy-compile threshold

Phase 1 above says "lazy on first punctual-light insert." That means a
scene with only directional lights never pays the compile cost. Is that
right, or should the cull pass eagerly compile alongside the cold-boot
eager set so the first punctual-light insert doesn't pay the compile
latency?

The PR #99 Block B precedent says "lazy" — but the operations during
gameplay (e.g. spawning a fireball with a point light) want zero compile
latency at trigger time.

**Question**: lazy (recommended — matches Block B pattern, can
`wait_for_pipelines_ready` if needed), or eager?

### E. Saturation fallback shape

The proposal says "tiles that overflow the per-tile capacity flag
saturated; the shader loops the full punctual buffer for that tile."
That's degraded performance in those rare cases but correct rendering.

Alternative: a tail-spill region (mirroring the C.2 stage-3 edge
overflow design) where overflow lights atomic-add into a shared region;
each tile's saturated entry points at a slice of the tail.

**Question**: simple full-list fallback (recommended; the rare pathological
case isn't worth the implementation complexity), or tail-spill?

### F. Tile size — 16×16 or smaller?

16×16 was chosen for amortized per-tile work + matches Forward+
literature. 8×8 would match `material_classify`'s tile size exactly,
avoiding the 2×2 cull-tile-to-classify-tile mapping. 32×32 would
reduce the output buffer to a quarter at the cost of looser culling per
pixel.

**Question**: 16×16 (recommended), 8×8 (matches classify), or 32×32
(smaller buffer)?

---

## Cross-references

- Existing per-mesh CPU culling:
  [`light_buckets/buckets.rs`](../../crates/renderer/src/light_buckets/buckets.rs),
  [`light_buckets/gpu.rs`](../../crates/renderer/src/light_buckets/gpu.rs).
- Existing pass scaffold (empty):
  [`render_passes/light_culling/`](../../crates/renderer/src/render_passes/light_culling/).
- Visibility buffer + depth attachment shape:
  [`render_textures.rs`](../../crates/renderer/src/render_textures.rs),
  [`render_passes/geometry/`](../../crates/renderer/src/render_passes/geometry/).
- Pipeline-readiness scheduler (for lazy-compile integration):
  [`pipeline_scheduler/mod.rs`](../../crates/renderer/src/pipeline_scheduler/mod.rs).
- Bind-group recreate event pattern:
  [`bind_groups.rs`](../../crates/renderer/src/bind_groups.rs).
- Standard depth-coordinate convention used elsewhere:
  [`templates/helpers/standard.wgsl`](../../crates/renderer/templates/helpers/standard.wgsl).
- The Stage-3 edge-resolve overflow pattern referenced under question E:
  [`docs/plans/remaining.md`](remaining.md) → "MAX_EDGE_BUDGET overflow
  atomic-add fallback".
