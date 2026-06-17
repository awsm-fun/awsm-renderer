# Plan B — shared attribute-prep pass (shrink the per-material resolve floor)

**Status:** proposed (not started). Prereq done: shadow-sampling gate (commit `de6cd249`) took the
leanest no-MSAA Custom shader from 110,730 → 60,335 B. This plan attacks the *next* ~half.

## Problem

awsm shades opaque per **material bucket**: each unique material compiles its own `cs_opaque` compute
kernel. Today every one of those kernels re-does the same material-agnostic prep **inline** before it
shades:

- `material_mesh_metas[]` lookup (offsets)
- barycentric unpack (from `barycentric_tex`)
- triangle index fetch (from `visibility_data`)
- world-position reconstruction (`get_standard_coordinates`)
- TBN unpack (from `normal_tangent_tex`)
- UV / vertex-color interpolation

This prep is identical across materials but is duplicated ×N because **WGSL can't link shared function
definitions across pipeline modules** — each module must contain everything it uses. The classify pass
(`material_classify/compute.wgsl`) already visits every pixel but does **only** classification (bucket
bitmask + per-bucket tile lists); it does none of the prep.

(Already materialized by the geometry pass and *not* recomputed: `normal_tangent_tex`, `barycentric_tex`.
So the duplicated work is the mesh-meta lookup, world-pos reconstruct, triangle fetch, and UV/attr
interpolation — plus all the helper includes those pull in: `standard`, `positions`, most of `math`,
`camera`, `mesh_meta`.)

## Idea

Do the prep **once** (fused into classify, or a dedicated attribute-resolve compute pass right after
it), write a **thin G-buffer** of resolved per-pixel attributes + `material_offset`, then make each
per-material kernel **read the G-buffer + sample its own textures + shade** — nothing else. This makes
the per-material module shrink toward three's per-material size, and cuts precompile (cost ≈
pipeline-count × module-size).

This is the classic **visibility-buffer → deferred-material / "deferred texturing"** move; the split
is legal because it happens at a **pass boundary** (buffer hand-off), not function linking.

### What moves vs stays

| work | destination |
|------|-------------|
| mesh_meta lookup, barycentric unpack, triangle fetch, world-pos reconstruct, TBN unpack, UV/attr interpolation | **shared prep pass** (once) |
| material data load (`*_get_material`), the shading math (BRDF / custom body) | per-material (stays) |
| **texture sampling** (material-specific bindings + UV transforms) | per-material (stays) — but reads the shared interpolated UV |

### Thin G-buffer contents (tune by measurement)

- Reuse existing `normal_tangent_tex` + `barycentric_tex` (already written).
- Add only what's actually recomputed today: interpolated UV set(s), and either store world-pos or keep
  reconstructing it from depth (cheap — prefer reconstruct to save bandwidth).
- `material_offset` per pixel (so the per-material kernel can load its material data without the
  mesh-meta walk).

## Trade-offs (the whole point of measuring)

- **Win:** per-material module + precompile shrink a lot (drop `standard`/`positions`/`mesh_meta`/most
  of `math`/`camera` + the inline interp from every material). Plausibly ~60 KB → ~25–35 KB; confirm.
- **Cost:** a fatter G-buffer = more memory bandwidth, and awsm already loses to forward at 4K on
  bandwidth (see report.md). Net runtime could go either way — **must** measure at high res.
- **Granularity:** the shared pass computes the *union* of attributes any material needs (can't
  per-material-gate UV like `inc.textures` does today), but it's one shader, paid once.
- **ABI:** new G-buffer textures + bind-group layout changes; the per-material bind groups change shape.

## Measurement checklist (gate the merge on this)

Render + measure both **before/after**, at N = 256 and 1024, AA off, at **1280×720 AND 3840×2160**:

1. **Per-material module size** (bytes) — expect a large drop. (`reports/awsm-dumps/` dump harness.)
2. **Precompile time** (eager pipeline build) — expect a large drop (pipeline-count × smaller module).
3. **Runtime FPS** at 720p *and* 4K — the risk metric (bandwidth). Must not regress meaningfully at 4K.
4. **Correctness** — naga validation (existing `wgsl_validation` tests) + visual parity in model-tests
   (PBR/IBL/transmission dish, alpha) + clean console.
5. Memory: G-buffer VRAM delta at 4K.

Land behind a flag (visibility-buffer recompute vs shared-prep) so the two can be A/B'd, then pick the
default per the numbers (possibly res-dependent).

## Stage: deferred shadow-sampling pass (decided with David — supersedes per-material gating)

**Decision:** do NOT gate shadow sampling per-material by technique. Instead, move ALL shadow sampling
into exactly **one** pass. Shadow *visibility* is material-independent (it depends only on
world-pos/normal + lights + the shadow maps the `shadow_masked` generation pass already produced), so
it factors out of per-material shading entirely. Per-material shaders then read a precomputed term and
`mix` — a read, not the ~50 KB of PCSS/EVSM/cascade/cube/SSCS math. The whole technique-gating problem
dissolves: all the technique code lives once, in this pass; the runtime `sample_shadow_descriptor`
switch stays exactly as is.

This is strictly better than the per-material `apply_lighting` gate already shipped (`de6cd249`): that
only helped materials that don't light; this removes shadow sampling from **every** material, including
first-party PBR (~222 KB today, ~50 KB of it shadow).

Design:
- New pass after classification/prep, before per-material shading. Reads world-pos/normal (from the
  prep G-buffer) + `mesh_meta.receive_shadows` (already exists — skip pixels that don't receive) + the
  shadow maps + light/descriptor data. Picks technique per shadow-caster at runtime (unchanged switch).
- Writes per-pixel shadow **visibility** to a buffer. NOTE it is per-(pixel, shadow-caster), not a single
  scalar (a pixel can be lit by several shadowed lights). Storage options:
  - **First cut:** sun/directional-only term, one R8/R16 per pixel — the dominant real case; trivial
    buffer; point/cube casters can stay inline initially.
  - **General:** per-(pixel, shadow-caster-slot) buffer bounded by max active casters (more bandwidth —
    measure at 4K, awsm's weak axis).
- Per-material shading: walks its froxel light list and, for each shadowed light, reads the precomputed
  term indexed by the light's `shadow_index` (already in `LightPacked.row4.z`), multiplies into its own
  BRDF result. The lighting math stays per-material; only the shadow *sampling* leaves.

Correction recorded: shadow **technique** is light-driven (point→cube, directional→cascade; EVSM/PCSS
are quality settings), NOT a material property — so there is no per-material "shadow sample type". The
only per-material shadow input is `receive_shadows`, which already exists in `mesh_meta`.

Verify before building:
- The froxel light-list → `shadow_index` read path (so the per-material lighting loop can fetch the
  precomputed term per light) — the one place this can go subtly wrong.
- Transparent pass also lights → needs the same buffer or its own handling.
- 4K bandwidth of the visibility buffer.

This shares the prep-pass infrastructure (needs world-pos/normal), so it's a **stage of this plan**, not
a separate effort: prep pass → shadow-sampling pass → lean per-material shading.
