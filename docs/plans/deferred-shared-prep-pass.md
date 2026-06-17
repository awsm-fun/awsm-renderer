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
2. **CORRECTED (verified in code, approved by David):** world position is NOT materialized. `cs_opaque`
   reconstructs it from **depth** via `standard.wgsl::get_standard_coordinates` (`depth_tex` → NDC →
   `inv_proj` → `inv_view`), which is cheap (depth + 2 matrix muls; only `depth_tex` + `camera` bound)
   — `positions.wgsl`'s vertex-interpolation is unused by the opaque kernel. So the slim shader KEEPS
   computing world-pos from depth (parity-exact with `cs_opaque`), and the prep pass instead
   materializes the **geometry-pool-fetch-heavy attributes**: interpolated **UV sets** (variable —
   `uv_set_index` bitfield + `mesh_meta.uv_set_count`, up to the existing max) + **vertex color**. This
   drops the ~100 MB fp32 world-pos buffer (the main 4K bandwidth risk) and the `reconstruct_world_pos`
   tunable becomes obsolete (always reconstruct in-shader; the builder field can be removed in cleanup).
   Net: smaller size win than first pitched (world-pos was never the expensive part), but the
   deferred-shadow win + bandwidth-safety keep it worthwhile.
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
| ~~world_pos~~ | — | — | — | — | **DROPPED** — reconstructed in-shader from depth (cheap), never materialized |
| UV sets | RG16F × `uv_set_count` (cap) | 4·S | 4 MB·S | 33 MB·S | S = sets actually used |
| vertex color | RGBA8 × color sets (cap) | 4·C | 4 MB·C | 33 MB·C | usually C≤1 |
| shadow visibility | R8 × K layers | K | ~1 MB·K | 8 MB·K | K = per-pixel caster cap |
| normal/tangent | (reuse `normal_tangent_tex`) | — | — | — | already materialized |
| UV gradients | recompute from `barycentric_derivatives_tex` | — | — | — | don't materialize |

- **No world-pos buffer** → the main 4K bandwidth swing is gone. The slim shader keeps the
  depth-unprojection `get_standard_coordinates` (already in `standard.wgsl`, parity-exact). Remaining
  prep buffers (UV/vcolor/shadow) are far smaller.
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
   - [x] **0c-froxel — single source of truth.** Extracted `froxel_base_for_pixel` + constants + new
     `froxel_light_count()` into `shared_wgsl/lighting/froxel_walk.wgsl` (with the canonical
     shadow-caster enumeration-order contract: directional prefix then per-froxel punctual), included by
     `apply_lighting`. Behavior-identical refactor; naga-green; the prep pass will include the same file
     so shadow slots align.
   - [x] **0c-shader-scaffold — prep shader registration.** `material_prep/shader/{cache_key,template,
     material_prep_wgsl/{bind_groups,compute}}` + `ShaderCacheKeyRenderPass::MaterialPrep` +
     `ShaderTemplateRenderPass::MaterialPrep` arms + askama dir. Minimal valid `cs_prep` (reads
     visibility sample 0, writes a sentinel to an `rgba32float` world_pos storage texture). naga-test
     `material_prep_shader_validates` (MSAA on+off); 255 tests green.
   - [ ] **0c-buffers — FOLDED into the pipeline-wiring sub-stage.** Inert GPU allocation behind an
     off-by-default flag is untestable + churns; instead allocate the **UV + vcolor** attr buffers (NO
     world-pos buffer — decision #2 corrected) + K-layer R8 shadow array + compact edge buffer +
     bind-group layouts *with* the pipeline/dispatch wiring, conditionally on `enabled`, where used.
   - [x] **1a — real attribute body (UV0 + vcolor0).** `cs_prep` now reads mesh-meta + triangle
     indices + barycentric and interpolates UV0 + vertex-color-0 from the geometry pool, writing
     `uv_out` (rg32float) + `vcolor_out` (rgba32float). Bindings: visibility/barycentric textures +
     `visibility_data` pool + `material_mesh_metas`. World-pos NOT written. naga-green (renamed `meta`→
     `mesh_meta`, a WGSL reserved word); 255 tests pass. NOTE: only UV set 0 / color set 0 for now —
     multi-UV-set materialization is a follow-up; attr-fetch helpers inlined (mirror
     `_texture_uv_per_vertex`/`_vertex_color_per_vertex`), TODO to share for guaranteed parity.
   - [x] **1b-textures — gated output allocation.** `render_textures.rs`: `prep_uv` (Rg32float) +
     `prep_vcolor` (Rgba32float) storage textures, gated on a new `prep_enabled` flag threaded
     RenderTextures::new → views() → RenderTexturesInner::new (mirrors `decal_color`); added to
     RenderTextureViews + destroy(); call site passes `prep_config.enabled`. Inert (unread), compiles
     green, 255 tests pass.
   - [x] **1b-pipeline — bind group + pipeline + dispatch.** material_prep/{bind_group.rs, render_pass.rs,
     pipeline.rs} mirroring material_classify; wire into RenderPasses + build() + bind-groups recreate;
     dispatch between classify and opaque. GPU-verify flag-on renders identically to off + clean console.
1. **Prep pass — attributes.** New compute pass after classify: interpolate **UVs + vertex colors**
   into the attr buffers (world-pos is NOT materialized — kept as depth-unprojection in the slim
   shader). Dispatched over covered tiles. Validate output vs in-shader values. No material reads yet.
2. **Slim `cs_opaque` (no-MSAA).** Behind `with_prep_pass`, `cs_opaque` reads the attr G-buffer instead
   of reconstructing the UV/vertex-color attributes. **NOTE (decision #2 reshapes the original drop
   list):** world-pos is depth-reconstructed in the slim shader, so `positions.wgsl`/`standard.wgsl`
   STAY — Stage 2's only drop is the UV-interpolation + vertex-color recompute (`texture_uvs.wgsl`'s
   `_texture_uv_per_vertex` + `vertex_color_attrib.wgsl`'s `_vertex_color_per_vertex`). Measure size
   drop; visual parity; tighten ceilings. Split into:
   - [x] **2a — multi-set prep array outputs.** `texture_uv()`/`vertex_color()` are called ~25× with a
     *per-texture* `uv_set_index` (dynamic, from material data) — the slim shader can't know which sets a
     material samples at compile time, so prep must materialize **all present sets**, not just set 0 (as
     1a/1b's single-layer `Rg32float`/`Rgba32float` did). Rework prep outputs to **array textures**:
     `prep_uv` → `texture_2d_array<rg32float>` with `MAX_PREP_UV_SETS` layers, `prep_vcolor` →
     `texture_2d_array<rgba32float>` with `MAX_PREP_COLOR_SETS` layers. `cs_prep` loops `0..uv_set_count`
     / `0..color_set_count` (from mesh-meta) writing each layer (`textureStore` with array layer).
     Sets `>= cap` clamp to the last layer + log (mirrors the shadow-caster `K` overflow policy — caps
     chosen generously: UV 4, color 2, covering virtually all glTF). render_textures alloc arrays;
     bind_group storage-texture-array write; naga + tests green; inert (opaque still recomputes);
     GPU-verify prep still runs clean.
   - [x] **2b — opaque reads arrays + drops recompute.** Thread `prep_enabled` through the opaque shader
     cache key + template (distinct prep-on/off pipelines); add `prep_uv`/`prep_vcolor` array textures as
     gated opaque bind-group inputs (sampled `texture_2d_array<f32>`); rewrite `texture_uv()`/
     `vertex_color()` so prep-on reads `textureLoad(prep_*, coords, set_index, 0)` (parity-exact:
     prep used the same barycentric + fp32 interp + sample 0) and the geometry-pool recompute helpers are
     no longer emitted (prep-off keeps them verbatim). Measure size drop; GPU-verify parity (Iridescence
     + an alpha model, MSAA on AND off, flag on vs off); tighten ceilings.
3. **Prep pass — shadow sampling.** Add the K-layer shadow visibility buffer + the froxel-order slot
   model + overflow logging. Prep includes `shadow/bind_groups.wgsl` (sampling). **Locked decisions
   (verified against code):**
   - **Output format = `Rgba8unorm` array, PACKED 4 slots/texel (NOT `R8unorm`).** `R8unorm` storage
     needs the optional `r8unorm-storage` WebGPU feature (not core-guaranteed); `Rgba8unorm` is core +
     already the renderer's storage idiom. Packing 4 shadow factors per texel also keeps memory at
     4 bytes/px for K=4 (33 MB @4K) — an `R32float` K-array would be 133 MB @4K, reintroducing the very
     4K-bandwidth risk decision #2 eliminated. Layout: `texture_2d_array<rgba8unorm, write>`,
     `ceil(clamped_k()/4)` layers; **slot j → layer `j/4`, channel `j%4`**. `cs_prep` accumulates a vec4
     per layer and `textureStore`s once per 4 slots. (Add `PrepPassConfig::shadow_visibility_layers()`
     = `clamped_k().div_ceil(4)`.)
   - **Prep writes its OWN shadow loop — does NOT include `apply_lighting.wgsl`** (that computes BRDF).
     Prep includes `froxel_walk.wgsl` (SSOT) + `light_access(_types).wgsl` + `shadow/bind_groups.wgsl`
     (`needs_shadow_sampling=true`) + `standard.wgsl` (`get_standard_coordinates` for world-pos from
     depth) + `math.wgsl` (`unpack_normal_tangent`). It walks the CANONICAL order (directional prefix via
     `get_n_directional`/`get_directional_light_index`, then per-froxel punctual via
     `froxel_base_for_pixel`/`froxel_light_count` + `lights_storage`), and for each light with
     `shadow_index != SHADOW_INDEX_NONE` computes `visibility = sample_shadow_directional(...)` —
     **matching `apply_lighting_per_froxel` exactly**, incl. the directional `* apply_sscs(...)` leg
     (lines 186–189) and NO sscs for punctual — writing it to slot j (j = j-th shadowed light, clamp to
     `clamped_k()`).
   - **`receive_shadows` is NOT applied in prep** (slot model is material-independent): prep stores the
     raw sampled visibility (as if `receive_shadows=1`); stage 4's lighting loop applies the per-mesh
     `receive_shadows`/`shadow_receiver_gate` at READ time (chooses slot j vs 1.0).
   - **Parity-critical:** the slot `j` advances per shadowed light in canonical order **independent of**
     the lighting loop's range-reject / `kind==1` `continue`s — so in stage 4 the lighting loop must also
     advance j for EVERY shadowed light (applying range-reject only to the contribution, not the slot).
   - **Bind groups:** prep group(0) gains `depth_tex` (`texture_depth_2d`), `normal_tangent_tex`
     (`texture_2d<f32>`), `camera_raw`, `shadow_visibility_out`; new group(1) = lights (lights_info,
     lights, lights_storage, cull_params — mirror opaque's group(1)); new group(2) = shadows
     (`shadow_group_index=2`, the 10 entries from `shadow_bind_group_layout_entries(true)` /
     `build_shadow_bind_group_entries(ctx.shadows)`). No-MSAA only (sample 0) for stage 3; MSAA edge is
     stage 5. Split into **3a** (allocate the gated `shadow_visibility` array texture in render_textures
     + RenderTextureViews + destroy + the layers helper — inert, mirrors 1b/2a) then **3b** (the bindings
     + includes + `cs_prep` shadow loop). Inert overall (lighting still samples inline until stage 4).
4. **[DONE]** **Lighting reads shadow buffer.** `apply_lighting` `shadow_from_buffer=true` for opaque
   (`= inc.apply_lighting && prep_read`); `needs_shadow_sampling = inc.apply_lighting && !prep_read` drops
   `sample_shadow_*`/`apply_sscs` from the opaque module — **first-party PBR −54,515 B (~53 KB)** measured
   (224,515 → 170,000 B, no-MSAA). Lighting walks froxel_walk SAME order, advances a shadow slot per
   shadowed light (kind==1 continue first, then shadow predicate + slot++, THEN range-reject — so the
   slot stays aligned with prep), reads `textureLoad(prep_shadow_visibility, coords, j/4, 0)[j%4]` gated
   by `receive_shadows`, slot≥K → 1.0. Bound prep_shadow_visibility at opaque group(0) binding 26 (gated
   prep_read). group(3) shadow maps kept bound-but-unsampled (reuses the existing needs_shadow_sampling=
   false path; the size win is the dropped functions). Transparent unchanged (`shadow_from_buffer=false`).
   **GPU-verified byte-identical** prep on vs off (MSAA off) on MetalRoughSpheres (visibility=1) AND
   SheenChair (real self-shadow, visibility<1) — slot alignment correct, no shadows-on-wrong-lights;
   8-bit shadow quantization is below the output LSB. 257+34 tests green.
5. **Edges (Option B) — DESIGNED, deferred behind 7 + the reconstruct-cleanup (see "Stage ordering" below).**
   Prep emits the compact per-edge-sample attr+shadow buffer; `cs_edge` reads it; drop reconstruction from
   the MSAA module. naga + visual MSAA-on parity; measure MSAA module size. **Design (investigated):**
   - The MSAA module is the renderer's most fragile area (the "1024-fix" unified `cs_opaque`+`cs_edge`
     module + the packed `edge_buffers.rs` data_buffer: edge_to_xy / edge_slot_map / per-bucket sample
     lists / accumulator). `cs_edge` runs one thread per packed `(edge_pixel_id, sample_mask)` entry and
     calls `shade_sample(coords, s)` per set sample, which reads vis/bary/normal **per-sample** but
     world-pos at **sample 0** (deliberate — see the in-code NOTE: per-sample depth caused silhouette
     wireframe artifacts). So only UV/vcolor + (optionally) shadow need per-edge-sample materialization;
     world-pos stays sample-0 depth-reconstructed (consistent with decision #2).
   - **COUPLING (the hard part):** under MSAA the SAME `texture_uv`/`vertex_color` helpers serve BOTH
     `cs_opaque` (primary, sample 0) AND `cs_edge` (sample s). To slim the MSAA module, BOTH must stop
     recomputing: `cs_opaque` reads the prep sample-0 arrays (extend the 2b prep-read to the MSAA primary),
     and `cs_edge` reads the compact per-edge-sample buffer. They need DIFFERENT prep sources from the same
     shared fn → differentiate by a per-kernel `var<private>` mode set at each entry point (cs_opaque →
     read prep array at g_prep_coords sample 0; cs_edge → read compact edge buffer at the current
     edge-sample id). This is why 2b/4 deliberately scoped prep-read to `msaa.is_none()`.
   - **Compact buffer:** add a per-edge-sample attr+shadow region keyed by the existing edge-sample id
     (edge_pixel_id × samples), populated by an extension of the prep pass (which must run after classify's
     edge emission + bind the edge buffers + walk the edge-sample list, computing UV/vcolor/shadow at each
     edge sample). Sized off the existing per-bucket edge budget. Likely its OWN storage buffer (own it in
     `material_prep/` like `material_opaque/edge_buffers.rs`).
   - **Sub-increment split:** 5a = prep extension writes compact per-edge-sample UV/vcolor + cs_edge reads
     them (drop UV/vcolor recompute from the MSAA module; keep shadow inline); 5b = compact edge SHADOW +
     extend cs_opaque MSAA-primary to prep-read sample-0 arrays/shadow (drop shadow sampling from the MSAA
     module). MSAA-on GPU parity is the gate for each.
   - **Still relevant (corrected per David 2026-06-17):** uber-shader does NOT collapse shading into one
     branching pass — it's *selective* grouping (some materials share a switch, not all), so the
     per-material `cs_edge` MSAA path persists and this slimming is NOT obviated. Deferral reason is now
     ONLY the fragility/risk of this area — worth doing, but check approach/timing with David before
     sinking effort (he's deep in the adjacent MSAA/grouping area via uber-shader).
6. **Finalize.** Drop the obsolete `reconstruct_world_pos` field; consider making `with_prep_pass`
   default-on / removing the A/B flag; re-dump `reports/awsm-dumps/`; update `report.md`; tighten ceilings.
7. **Custom materials use froxel-culled lights (David-requested).** Today `light_access.wgsl` (the
   custom-material lighting surface) only exposes `get_light(i)` over ALL `n_lights` + `get_n_directional()`
   — so a custom material that lights itself iterates EVERY light, missing the deferred froxel cull that
   built-ins get via `apply_lighting_per_froxel`. Fix: expose the `froxel_walk.wgsl` SSOT to custom
   kernels (a Tier-A helper that returns `froxel_base_for_pixel`/`froxel_light_count` + the per-froxel
   light indices), and bind `cull_params` + `lights_storage` for Custom opaque kernels (they aren't today).
   Document the recipe (editor `NEW_MATERIAL_WGSL` + MCP). Verify a custom lighting material iterates only
   the froxel's lights. Independent of the prep pass but same lighting domain — uses the SSOT already in.

## Stage ordering (updated 2026-06-17, mid-loop)

The no-MSAA headline wins are landed + GPU-verified (2b UV materialization, 4 shadow-buffer = PBR
−53 KB). Remaining stages are reordered by value/risk:
1. **Stage 7 next** (custom-material froxel lights) — independent of the prep pass, David-requested,
   lower-risk; uses the froxel_walk SSOT already in.
2. **reconstruct_world_pos cleanup** (part of 6) — obsolete field (decision #2), pure mechanical removal.
3. **Stage 5** (MSAA edge slim) — DESIGNED above but **deferred + flagged to David**: it's the most
   fragile area (NOT obviated by uber-shader — that's selective grouping, the cs_edge path persists).
   Check approach/timing with David before tackling.
4. **Stage 6 default-on decision** — recommend to David, don't force (flipping the default needs broad
   GPU-verify + interacts with whether Stage 5 lands).

## After this loop completes: HAND OFF to David for `uber-shader.md`

**(Updated per David 2026-06-17 — supersedes the old "auto-continue" note.)** Do **NOT** auto-start
`uber-shader.md` — David is editing it live in another session and will kick off its implementation
himself. When Plan B's remaining stages are done (or blocked on the Stage 5 / default-on decisions
above), STOP the loop, post the concise before/after summary, and tell David Plan B is complete and
uber-shader awaits his go-ahead. (See memory `uber-shader-needs-kickoff`.)

## Implementation recipe (grounded in the code — built from `material_classify` as template)

The prep pass is a compute pass shaped exactly like `material_classify`; only inputs (vis-buffer + froxel
light list) and outputs (world_pos, UVs/vcolor, shadow visibility, compact edge) differ.

**Module skeleton** (mirror `render_passes/material_classify/`): `material_prep/{render_pass.rs,
pipeline.rs, bind_group.rs, buffers.rs}` + `shader/{cache_key.rs, template.rs, material_prep_wgsl/
{bind_groups.wgsl, compute.wgsl}}`. `mod.rs` already holds `PrepPassConfig`.

**Outputs / allocation** (`render_textures.rs` `RenderTexturesInner::new` + the `views()` resize path at
the size-changed branch — add new fields there so they re-alloc on resize):
- ~~`world_pos`~~ — NOT materialized (decision #2 corrected): the slim shader keeps depth-unprojection
  `get_standard_coordinates`. Prep still computes world-pos locally (from depth) for shadow sampling,
  but does not write it.
- UV sets — store interpolated UVs (variable count, capped). **Resolved (Stage 2a):** `texture_2d_array`,
  `MAX_PREP_UV_SETS` layers; vcolor `texture_2d_array`, `MAX_PREP_COLOR_SETS` layers. **Format = `Rg32float`
  / `Rgba32float` (full fp32), NOT the originally-guessed `Rg16float`/`Rgba8unorm`** — the slim shader must
  read back a value bit-identical to the in-shader fp32 barycentric interpolation it replaces, so half/unorm
  would drift visual parity. (These are the geometry-pool-fetch-heavy attrs worth materializing.) 1a/1b
  shipped a single-layer MVP (set 0 only); 2a reworks to the array form so the recompute can be fully
  dropped.
- `shadow_visibility` — **`R8unorm` `texture_2d_array`, K layers** (K = `PrepPassConfig::clamped_k()`),
  layer j = j-th shadowed froxel light. (Resolved ambiguity: R8unorm, not uint — it's a 0..1 factor.)
- compact per-edge-sample buffer — storage buffer in `material_prep/buffers.rs` (own it here, like
  `material_opaque/edge_buffers.rs`), allocated in `build()`; only when MSAA.

**Bind groups** (`material_prep/bind_group.rs`, dual MSAA/non-MSAA layout like classify): inputs =
visibility_data, barycentric(+derivatives), normal_tangent, camera, mesh-meta/material storage,
`cull_params` + `lights_storage`, shadow maps (`shared_wgsl/shadow/bind_groups.wgsl` — prep is a shadow
*sampler*); outputs = shadow_visibility + UV/vcolor + edge buffer (no world_pos output). Rebuild on
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
