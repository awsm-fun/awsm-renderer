# Plan B (spec) — shared prep + deferred-shadow pass; slim per-material shading

**Status:** implementation-ready spec. Prereqs shipped: `MsaaSampleTextures` gate (`13e91e3e`),
whole-block shadow-sampling gate behind `apply_lighting` (`de6cd249`).

**Principle:** every per-pixel computation that is the *same regardless of material* runs ONCE in a
shared pass and is written to a buffer; per-material kernels read those buffers and do only genuinely
material-specific work (its texture sampling, normal-map application, BRDF/custom shading, light
accumulation). This shrinks the per-material module (compile + size) and is the foundation a future
single-branching-shading pass ("uber-shader", explicitly out of scope here) would build on.

## Locked decisions

1. **Shadow storage = K visibility layers**, where K = max shadow casters that can overlap a *single
   pixel* (NOT total scene casters). K is **configurable at `AwsmRenderer` build time**. Buffer is
   `texture_2d_array<r8unorm>` (or storage buffer) of K layers; slot `j` = the j-th shadowed light in
   the pixel's froxel, in froxel-list order. Overflow (>K shadowed lights in a froxel) is **clamped +
   logged** (no silent cap). The per-material lighting loop walks the same froxel list in the same
   order, so its j-th shadowed light reads layer `j` — no per-pixel search needed.
2. **World-position is materialized fp32** via the existing perspective-correct vertex interpolation
   (`positions.wgsl::get_standard_coordinates` — NOT depth unprojection). UV sets stay **variable**
   (the `uv_set_index` bitfield + `mesh_meta.uv_set_count`), stored up to the existing practical max.
3. **Edges = Option B (compact edge buffer).** The prep pass additionally emits a small
   per-edge-sample attribute+shadow buffer (edge pixels are a tiny fraction), and `cs_edge` *reads* it
   instead of reconstructing — so BOTH the primary and edge paths slim, and `cs_edge` collapses toward
   "read + shade", reducing the edge complexity.
4. **Transparent stays forward** (back-to-front fragment pass at its own pixels) — keeps inline
   shadow+lighting. Out of scope.
5. **Uber-shader is the north star, not now.** Per-material shading kernels are retained; this refactor
   makes them thin and produces exactly the buffers an uber-shader would later consume.

## Pass architecture (opaque path)

```
geometry / visibility (+ masked variant = alpha test)       [exists, unchanged]
light culling (froxel)                                       [exists]
shadow-map generation (shadow_masked)                        [exists]
classify → per-bucket tile lists  (+ "covered tiles" list)   [exists; minor add]
► RESOLVE-PREP pass (NEW, compute, per pixel over covered tiles):
    - read vis-buffer + mesh_meta
    - interpolate world_pos (fp32), UV sets, vertex colors    → attr G-buffer (full-res, sample 0)
    - walk froxel light list; for each shadowed light sample
      its shadow map (technique switch unchanged)            → shadow visibility buffer (K layers)
    - for EDGE pixels: also emit per-sample attrs + shadow    → compact edge buffer (Option B)
► per-material shading (SLIMMED):
    - cs_opaque: read attr G-buffer + shadow buffer (sample 0), sample own textures,
      apply normal map, run BRDF/custom, accumulate lights using precomputed shadow terms
    - cs_edge:   read COMPACT EDGE buffer per sample, shade, accumulate (Option B)
final_blend / skybox_edge_resolve (MSAA)                     [exists]
```

All shadow-sampling code now lives in: the **prep pass** (samples) + the **transparent** forward pass
(inline). It is **removed from every opaque per-material kernel** (incl. first-party PBR).

## Buffers (formats + the bandwidth budget — the measured risk)

Per-pixel, full-res (sample 0). Sizes shown @720p / @4K:

| buffer | format | bytes/px | @720p | @4K | notes |
|--------|--------|---------:|------:|----:|-------|
| world_pos | fp32 ×3 (storage buf, packed) | 12 | 11 MB | 100 MB | biggest item; the prime bandwidth variable |
| UV sets | RG16F × `uv_set_count` (cap) | 4·S | 4 MB·S | 33 MB·S | S = sets actually used |
| vertex color | RGBA8 × color sets (cap) | 4·C | 4 MB·C | 33 MB·C | usually C≤1 |
| shadow visibility | R8 × K layers | K | ~1 MB·K | 8 MB·K | K = per-pixel caster cap |
| normal/tangent | (reuse `normal_tangent_tex`) | — | — | — | already materialized |
| UV gradients | recompute from `barycentric_derivatives_tex` | — | — | — | don't materialize |

- **world_pos is the bandwidth swing.** Default: materialize it (principle-consistent, precise). A
  build-time flag `prep_reconstruct_world_pos` falls back to in-shader reconstruction (keeps
  `positions.wgsl` in materials, saves ~100 MB @4K). Decide the default from the 4K measurement.
- Each pixel is read by exactly one material pass, so materializing does NOT multiply reads by material
  count — total read traffic ≈ one full-res read regardless of N.

## Shadow visibility — slot model

- Prep pass, per pixel: `j = 0`; walk the pixel's froxel light list; for each light with
  `shadow_index != NONE`: `if j < K { visibility[pixel][j] = sample_shadow_descriptor(light.shadow_index, world_pos, normal); j++ } else { overflow++ }`. Log per-frame overflow count once.
- Per-material lighting loop: walk the SAME froxel list in the SAME order; maintain its own `j`; for
  the j-th shadowed light, `vis = visibility[pixel][j]` (then apply `mesh_meta.receive_shadows`).
- `receive_shadows` is per-mesh (read from `mesh_meta` — available in both passes); applied at *use*
  time so the slot indexing stays material-independent in prep.
- K default e.g. 4; builder setter `with_max_shadow_casters_per_pixel(k)`. Feeds the shader cache key
  (pipelines vary with K) and the buffer allocation.

## apply_lighting — dual shadow source

`apply_lighting.wgsl` gets a template flag `shadow_from_buffer`:
- **opaque** (`true`): read `visibility[pixel][j]` — NO `sample_shadow_*` functions compiled in.
- **transparent** (`false`): inline `sample_shadow_directional(...)` as today (forward, own pixels).

So the shadow-sampling functions are included by the **prep pass** and the **transparent** pass only.

## What stays per-material (unchanged)

Material/texture param load, **texture sampling** (which textures, transforms, mip), **normal-map
application**, **BRDF / toon / unlit / custom WGSL**, and the **light-accumulation loop** (reads the
shadow buffer). The custom-material `OpaqueShadingInput` contract is unchanged — the kernel just
populates it from the prep buffers instead of reconstructing, so existing custom shaders are unaffected.

## Build-time config (AwsmRenderer builder)

- `with_max_shadow_casters_per_pixel(k: u32)` (default e.g. 4) — K above.
- `with_prep_pass(enabled: bool)` — A/B flag: off = current recompute-in-shader path; on = shared prep.
  Lets us land incrementally and measure both. (Remove once on-by-default is proven.)
- `prep_reconstruct_world_pos: bool` — world-pos materialize vs reconstruct (the bandwidth tunable).

## Implementation stages (one per commit; each independently testable + green)

Each stage: `cargo test -p awsm-renderer -p awsm-materials --lib` green (naga validation +
size_regression + completeness), and the renderer still renders model-tests correctly (PBR/IBL dish,
alpha, shadows) with a clean console. Stages 0–2 add the buffer + slim the no-MSAA primary path; 3–4
add deferred shadows; 5 handles edges (Option B); 6 finalizes.

0. **Config + buffers scaffolding.** Builder flags; allocate the attr G-buffer + shadow buffer +
   bind-group layouts. No behavior change (nothing reads them yet).
   - [x] **0a — config type + module.** `render_passes::material_prep::PrepPassConfig` (enabled /
     `max_shadow_casters_per_pixel` K=4 default, ceiling 16 / `reconstruct_world_pos`) + `material_prep`
     module scaffold. Compiles green, inert. (split from 0 for a safe first increment)
   - [x] **0b — builder wiring.** `with_prep_pass` / `with_max_shadow_casters_per_pixel` /
     `with_prep_reconstruct_world_pos` on `AwsmRendererBuilder`; `PrepPassConfig` flows builder→build()→
     `AwsmRenderer.prep_config`. Inert, compiles green, 254 tests pass.
   - [ ] **0c — buffers + bind-group layouts.** Allocate attr G-buffer (world_pos fp32, UVs, vcolor) +
     K-layer shadow buffer + compact edge buffer + their bind-group layouts. Inert.
1. **Prep pass — attributes.** New compute pass after classify: interpolate world_pos + UVs + vertex
   colors into the G-buffer (dispatched over covered tiles). Validate output vs in-shader values (a
   debug compare). No material reads yet.
2. **Slim `cs_opaque` (no-MSAA).** Behind `with_prep_pass`, `cs_opaque` reads the attr G-buffer instead
   of reconstructing; drop `positions`/`standard`/UV-interp includes from the no-MSAA module. Measure
   size drop; visual parity; tighten ceilings.
3. **Prep pass — shadow sampling.** Add the K-layer shadow visibility buffer + the froxel-order slot
   model + overflow logging. Prep includes `shadow/bind_groups.wgsl` (sampling).
4. **Lighting reads shadow buffer.** `apply_lighting` `shadow_from_buffer=true` for opaque; remove
   `sample_shadow_*` from opaque modules (first-party PBR drops ~50 KB). Transparent unchanged
   (`shadow_from_buffer=false`). Visual parity on the shadowed dish; measure PBR size.
5. **Edges (Option B).** Prep emits the compact per-edge-sample attr+shadow buffer; `cs_edge` reads it;
   drop reconstruction from the MSAA module. naga + visual MSAA-on parity; measure MSAA module size.
6. **Finalize.** Pick `prep_reconstruct_world_pos` default from 4K numbers; consider making
   `with_prep_pass` default-on / removing the A/B flag; re-dump `reports/awsm-dumps/`; update
   `report.md`; tighten all ceilings.

## Implementation recipe (grounded in the code — built from `material_classify` as template)

The prep pass is a compute pass shaped exactly like `material_classify`; only inputs (vis-buffer + froxel
light list) and outputs (world_pos, UVs/vcolor, shadow visibility, compact edge) differ.

**Module skeleton** (mirror `render_passes/material_classify/`): `material_prep/{render_pass.rs,
pipeline.rs, bind_group.rs, buffers.rs}` + `shader/{cache_key.rs, template.rs, material_prep_wgsl/
{bind_groups.wgsl, compute.wgsl}}`. `mod.rs` already holds `PrepPassConfig`.

**Outputs / allocation** (`render_textures.rs` `RenderTexturesInner::new` + the `views()` resize path at
the size-changed branch — add new fields there so they re-alloc on resize):
- `world_pos` — `Rgba32float` storage texture (materialized; per locked decision — NOT depth). Skipped
  when `reconstruct_world_pos`.
- UV sets — store interpolated UVs (variable count, capped) — format `Rg16float` ×N (or packed); vcolor
  `Rgba8unorm`.
- `shadow_visibility` — **`R8unorm` `texture_2d_array`, K layers** (K = `PrepPassConfig::clamped_k()`),
  layer j = j-th shadowed froxel light. (Resolved ambiguity: R8unorm, not uint — it's a 0..1 factor.)
- compact per-edge-sample buffer — storage buffer in `material_prep/buffers.rs` (own it here, like
  `material_opaque/edge_buffers.rs`), allocated in `build()`; only when MSAA.

**Bind groups** (`material_prep/bind_group.rs`, dual MSAA/non-MSAA layout like classify): inputs =
visibility_data, barycentric(+derivatives), normal_tangent, camera, mesh-meta/material storage,
`cull_params` + `lights_storage`, shadow maps (`shared_wgsl/shadow/bind_groups.wgsl` — prep is a shadow
*sampler*); outputs = world_pos storage view + shadow_visibility + UV/vcolor + edge buffer. Rebuild on
`render_texture_views` recreate OR light-culling-buffer recreate.

**Dispatch** (`render.rs`, between `material_classify.render()` and `material_opaque.render()`): gate on
`prep_config.enabled`; `dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1)` (one wg/8×8 tile,
same grain as classify).

**FROXEL SINGLE SOURCE OF TRUTH (do this first, stage 3 prereq):** extract `froxel_base_for_pixel` + the
per-froxel light-index walk out of `shared_wgsl/lighting/apply_lighting.wgsl` (the
`apply_lighting_per_froxel` loop) into a new `shared_wgsl/lighting/froxel_walk.wgsl`, included by BOTH
`apply_lighting` and the prep shader. Prep writes `shadow_visibility[j]` for the j-th shadowed light in
that walk; the per-material lighting loop reads `shadow_visibility[j]` for its j-th shadowed light. Same
include = same order = aligned slots. This is the spec's CRITICAL invariant — must be one file.

**Pipeline/cache:** `ShaderCacheKeyMaterialPrep { msaa_sample_count, /* gated on enabled at build */ }`;
`MaterialPrepPipelines::build_descriptors` returns empty when `!enabled` so it's zero-cost off.

**Revised sub-stage order** (replaces the coarse 0c): 0c-froxel (extract `froxel_walk.wgsl`,
naga-green, behavior-identical) → 0c-buffers (allocate outputs + bind-group layouts, inert) → then
stage 1+. Each its own commit.

## Measurement gates (record before/after at N=256 and 1024, AA off, 1280×720 AND 3840×2160)

1. Per-material module size (bytes) — expect large drop on the no-MSAA path; PBR drops shadow.
2. Precompile time — expect large drop (pipeline-count × smaller module).
3. Runtime FPS at 720p AND 4K — the risk metric (bandwidth). Must not meaningfully regress at 4K; if it
   does, prefer `prep_reconstruct_world_pos`/keep the A/B flag and document the res-dependent default.
4. Correctness — naga; model-tests visual parity (PBR/IBL/transmission dish + shadows, alpha,
   MSAA on/off); clean console.
5. VRAM delta at 4K.

## Risks

- **Bandwidth at 4K** (world_pos buffer) — the main risk; the world-pos tunable + A/B flag exist for it.
- **MSAA edge correctness** — Option B's per-edge-sample emission must match the old per-sample shading
  exactly; verify visually (it can't be naga-checked) and keep the old path behind the A/B flag until
  proven.
- **Froxel slot alignment** — prep and lighting MUST walk the froxel list identically; a mismatch =
  wrong shadows. Single source of truth for the walk order.
- **Transparent divergence** — it keeps inline shadows; ensure the dual `shadow_from_buffer` flag keeps
  both paths compiling (naga covers both).

## Out of scope (separate future work)

- Transparent path slimming.
- The uber-shader (single branching shading dispatch) — this spec builds toward it but does not do it.
