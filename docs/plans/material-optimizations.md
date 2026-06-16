# Material shader compartmentalization + size optimizations

**Status:** planned
**Date:** 2026-06-16
**Owner:** (tbd)

## Problem

A benchmark (`experiments/compare-threejs-materials`) generates N unique
procedural materials (one compiled pipeline per cube) and found awsm's
per-material compiled WGSL is ~273 KB / 167 functions each, vs three.js's
~1–3 KB forward fragment. This drives a 4–10× longer precompile and a hard
device-loss failure at N = 1024 (per-pipeline GPU resource exhaustion).

The "skinny materials" design (per-material `ShaderIncludes` gating) was
supposed to make dynamic materials lean. It mostly doesn't. Measured by
rendering the real opaque-pass template for a Custom (dynamic) material:

| config | `ShaderIncludes::empty()` (leanest) | `ShaderIncludes::all()` (benchmark) |
|---|--:|--:|
| no-MSAA, no-mips | **161 KB** | 217 KB |
| no-MSAA, mips    | **173 KB** | 240 KB |
| MSAA4 + mips     | **196 KB** | **262 KB** ≈ report's 273 KB |

Toggling every include off only removes ~25%. The floor is ~160 KB per
pipeline. Section breakdown of the lean (no-MSAA, mips) 173 KB build:

```
  33 KB  ALL four first-party material bodies: pbr 17.4 + flipbook 9.0 + toon 5.0 + unlit 2.2
  11 KB  mipmap.wgsl       (emitted because mips=on — even with zero textures)
   8 KB  light_access.wgsl (always)
   6 KB  math.wgsl         (always)
  ~75 KB top-level kernel bodies (compute.wgsl cs_opaque + bind_groups + dynamic wrapper)
  rest:  textures, standard, mesh_meta, positions, camera, frame_globals, …  (all always)
```

## The core design flaw

`ShaderIncludes` (`materials/src/shader_includes.rs`) is **one menu used for two
incompatible purposes**:

1. First-party bases declaring their own implementation-internal modules. PBR
   declares `MATERIAL_COLOR_CALC | APPLY_LIGHTING | BRDF | …` (`pbr.rs:450`).
2. The capability menu a Custom (dynamic) material picks from.

Those PBR modules are **not generic helpers** — they are bound to the
`PbrMaterial` / `PbrMaterialColor` types:

- `material_color_calc.wgsl` — `PbrMaterial` → `PbrMaterialColor` (samples all PBR
  textures/extensions). PBR-internal.
- `apply_lighting.wgsl` — every entry point takes `PbrMaterialColor`. PBR-internal.
- `brdf.wgsl` — **mixed**: generic primitives (`fresnel_schlick`, `distribution_ggx`,
  `geometry_smith`, IBL samplers, lines 66–367) **plus** `brdf_direct` / `brdf_ibl`
  taking `PbrMaterialColor` (PBR-internal orchestrators).
- `light_access.wgsl` — genuinely generic (`get_lights_info`, `get_light`, `Light`).

A Custom material has its own layout/textures/logic; it has **no reason to ever call
`material_color_calc`** or the PBR `apply_lighting`/`brdf` orchestrators. Offering
them on the custom menu (and defaulting Custom to `all()`) is what drags ~87 KB of
PBR code into a noise shader. The earlier "emit the PBR fragment for Custom if
`material_color_calc` is declared" idea was wrong for the same reason — that carve-out
should not exist.

**Design principle going forward:** a custom/dynamic material may opt into *generic*
capabilities (bindings + reusable helpers: math, color-space, camera, light iteration,
texture-pool sampling, vertex-color/UV access, shadows, skybox, extras, generic BRDF
primitives). It may **not** reach into any built-in shading model's internals. If it
wants PBR-like shading, it supplies that WGSL itself (optionally built on the generic
primitives we expose).

## Module taxonomy (target)

**Tier A — generic helpers (any material, incl. Custom, may opt in):**
math · color_space · camera · light_access (iterate punctual lights) · textures
(pool sampling) · texture-UV / vertex-color accessors · shadows (sampling) · skybox
(sampling) · extras · **brdf_primitives** (fresnel/ggx/smith/IBL samplers, split out
of brdf.wgsl).

**Tier B — shading-model internals (owned by one built-in base; never on the custom menu):**
`PbrMaterial` struct + `pbr_get_material` accessor · material_color_calc ·
apply_lighting · **brdf_pbr** (the `PbrMaterialColor` orchestrators) · `pbr_get_gradients`
(the PBR half of mipmap.wgsl) · toon/unlit/flipbook shading bodies + their accessors.

Bindings stay full and pass-owned (stable ABI, ~free); gating targets WGSL *code* only.

## Plan

### Phase 0 — benchmark correctness (do first, independent)

- [x] In `experiments/compare-threejs-materials/awsm/src/materials.rs`, set
      `shader_includes: ShaderIncludes::empty()` and
      `fragment_inputs: FragmentInputs::empty()`. (API is `empty()`, not `none()`.)
      The noise body only reads `world_position` / `world_normal`, always provided.
      → Done. Swapped `all()`→`empty()` for both; noise body uses only WGSL built-ins + always-provided inputs.
- [x] Re-run the benchmark; record new per-material size + precompile in `report.md`.
      Isolates "benchmark over-declared" from real engine cost.
      → Done (fresh-context browser run). 256: 273→201 KB avg, 8.1→2.9 s. 512: 6.5 s. 1024:
      28.7→14.8 s and **now renders at 30 fps with a clean console** (was blank + device loss) —
      the ~66 KB/shader drop alone fixed the 1024 failure. Full table in report.md.

### Phase 1 — taxonomy audit (design foundation, no behavior change)

- [x] Classify every shared/helper WGSL module as Tier A (generic) or Tier B
      (model-internal) per the table above; capture in a short doc comment in
      `shader_includes.rs` and in `opaque_kernel_includes.wgsl`.
      → Done. Authoritative taxonomy table (module → tier → current-gate →
      target-gate) added to `materials/src/shader_includes.rs` module doc; a
      one-line pointer added to `opaque_kernel_includes.wgsl` (kept minimal to
      avoid WGSL bloat). 247 tests green.
- [x] For each module, list its callers and which `base`/include gates it.
      Output: a checklist of every `{% include %}` and whether it’s currently gated.
      → Done — the "current gate" column of that table is the checklist; it shows
      almost every Tier A module is currently `always` (why empty() still emits ~160 KB).

> Phase 1 is doc-only (Rust doc + one WGSL comment line — no rendering-logic change),
> so the phase-end browser run is skipped here; render verification resumes at Phase 2
> where emitted WGSL logic actually changes. `cargo test -p awsm-renderer` (247) green.

### Phase 2 — split the mixed modules

- [ ] Split `shared_wgsl/lighting/brdf.wgsl` into:
      - `brdf_primitives.wgsl` (Tier A): fresnel/ggx/smith, anisotropy/sheen/clearcoat
        math that takes plain params, IBL samplers.
      - `brdf_pbr.wgsl` (Tier B): `brdf_direct` / `brdf_ibl*` (operate on `PbrMaterialColor`).
- [ ] Split the PBR-gradient half of `mipmap.wgsl` (`pbr_get_gradients`, Tier B) from
      its generic UV-derivative machinery (Tier A, gate on textures/UV).
- [ ] Keep `apply_lighting.wgsl` and `material_color_calc.wgsl` as Tier B wholesale.

### Phase 3 — separate the custom menu from first-party internals (kills #1 + the dead 33 KB)

- [ ] Make `ShaderIncludes` (the custom-facing menu) enumerate **only Tier A**. Remove
      `MATERIAL_COLOR_CALC` / `APPLY_LIGHTING` / PBR-orchestrator `BRDF` from it.
      `ShaderIncludes::all()` then means “all generic helpers” — a safe lazy default for
      a custom material, no PBR internals.
- [ ] Introduce a separate first-party-internal include set (e.g. `ModelInternals` or
      per-base const) that PBR/Toon/Unlit/Flipbook declare for their own kernels. The
      opaque template emits Tier B modules gated on `base`, not on the custom menu.
- [ ] In `template.rs` (`TryFrom<&ShaderCacheKeyMaterialOpaque>`), `materials_wgsl` for
      `base == Custom` emits **nothing** (Custom only ever calls `custom_shade_dynamic`).
      First-party bases emit only their own fragment. Drop
      `build_materials_wgsl_filtered(None)`-for-Custom entirely.
- [ ] Validation: render Custom × {empty, every Tier A bit} × {mips,no-mips} × {msaa,no-msaa}
      and confirm each compiles (WGSL validation, not just string checks). Confirm no
      un-gated reference to `PbrMaterial`/toon/unlit/flipbook structs survives on the
      Custom path.

### Phase 4 — complete the gating + wire FragmentInputs into the opaque path (#3, #4)

- [ ] Gate the currently-unconditional Tier A modules in `opaque_kernel_includes.wgsl`
      on the resolved include set: textures + texture_uvs + generic mipmap → `TEXTURES`;
      light_access + the unconditional `get_lights_info()` call in `compute.wgsl` →
      `LIGHT_ACCESS`/`LIGHTS`; vertex_color/_attrib → `VERTEX_COLOR`.
- [ ] Add `FragmentInputs` to `ShaderCacheKeyMaterialOpaque` and consume it in the
      compute template so the kernel computes/unpacks only declared inputs (TBN unpack,
      lights read, UV/vertex-color fetch). Today the opaque kernel is inert to
      `FragmentInputs` and computes everything.
- [ ] Each newly-gated module needs its flag field, the `{% if %}` in the include host,
      and matching guards at its call sites in `compute.wgsl`.
- [ ] First-party bases must declare the Tier A modules they actually use so they don’t
      regress (audit pbr/toon/unlit/flipbook declarations against the new gates).

### Phase 5 — de-duplicate the MSAA shading path (#5)

Sequenced **after** Phases 2–4: the thing to extract is exactly the `{% if base == ... %}`
shading match, which those phases are still reshaping. After Phase 3 each pipeline emits one
base's arm, so the factoring is "wrap the single rendered arm in a function, call it twice."

- [ ] Factor the per-material shading glue (the `{% if base == ... %}` match producing
      `(color, base_alpha)`, **plus** the instance-tint and wireframe blocks — also currently
      copy-pasted in both) into one helper used by both `cs_opaque` and `shade_sample`. The
      helper returns `(color, base_alpha)`; the caller decides the sink (`cs_opaque` →
      `textureStore`, `cs_edge` → accumulate).
- [ ] Note: the heavy per-base *bodies* (`compute_material_color`, `apply_lighting_per_froxel`,
      `compute_toon_lit_color`, `custom_shade_dynamic`, …) are already defined once — only the
      glue is duplicated, so the size win is ~10–15 KB at MSAA4 (not the full MSAA delta;
      `msaa.wgsl` + `cs_edge` orchestration are inherent). The primary win is maintenance:
      one shading path instead of two that already drift.
- [ ] The Custom path already factors through `OpaqueShadingInput` + `custom_shade_dynamic`
      (both call sites just build the struct from primary vs per-sample data), so for dynamic
      materials this is nearly free.
- [ ] Snags to handle explicitly (do **not** fold into the helper):
      - PBR debug branch (`pbr_material.debug_bitmask != 0u`) does `textureStore` + early
        `return` inside the body — keep it primary-only in `cs_opaque` around the shared call.
      - `debug.normals` early-return stays primary-only.
      - `shade_sample` already uses sample-0 depth via `get_standard_coordinates(coords, …)`
        (workaround at `compute.wgsl:456`), so `world_position`/`surface_to_camera` are
        identical primary-vs-sample; only normal/TBN/barycentric/instance_id/material-offset
        are genuinely per-sample inputs to the helper.

### Phase 6 — (optional) generic lighting helpers for custom materials

- [ ] Expose a small, documented Tier A lighting helper so a custom material can light
      itself without reaching into PBR internals (e.g. iterate punctual + a simple
      lambert / GGX-on-plain-params built on `brdf_primitives` + `light_access`). Lets
      authors get “lit” cheaply; anything fancier they supply themselves.

### Phase 7 — editor frontend + MCP integration (depends on Phase 2–4)

When the taxonomy split lands, the custom-material **authoring surfaces** must expose the new
Tier-A-only opt-in menu (the Tier-B PBR modules are no longer author-selectable), and the full
helper catalog must be discoverable via MCP. Surfaces found:

- **Editor** `packages/frontend/editor/src/controller/custom_material.rs`: `CustomMaterial`
  carries `shader_includes: Mutable<Vec<String>>` + `fragment_inputs`, and `ALL_SHADER_INCLUDES`
  (~line 142) / `ALL_FRAGMENT_INPUTS` (~line 158) drive the picker UI. Update these menus to the
  Tier-A set (drop `apply_lighting`/`brdf`/`material_color_calc`; add `brdf_primitives` and any
  newly-split keys). Default `fragment_inputs` already sensible (`normals`+`view_dir`).
- **editor-protocol** `packages/mcp/editor-protocol/src/command.rs`:
  `SetCustomMaterialShaderIncludes` / `SetCustomMaterialFragmentInputs` (Vec<String> keys) and
  `project.rs` persistence stay structurally the same — but **migrate saved projects**: keys that
  Phase 3 removes from the menu (`apply_lighting`/`brdf`/`material_color_calc`) must be dropped or
  remapped on load (the "unknown keys are dropped" contract already covers forward-compat, but a
  project that *relied* on those for PBR-like shading needs a note/upgrade path).
- **MCP** `packages/mcp/src/mcp.rs:2070`: the `SetCustomMaterialShaderIncludes` tool description
  **hardcodes** the legal key list (currently includes the Tier-B keys). Two tasks:
  1. Update the legal-key list to the Tier-A menu.
  2. **Expose the helper catalog via MCP** (the user's ask): add a query/tool that returns the
     available Tier-A helpers — key, one-line description of what WGSL each provides, its
     `FragmentInputs` deps — so an agent/editor discovers the opt-in set instead of relying on a
     hardcoded string. Source it from `awsm_materials::ShaderIncludes` (single source of truth)
     rather than duplicating the list.

- [ ] Update editor `ALL_SHADER_INCLUDES` / picker UI to the Tier-A menu.
- [ ] Update + (ideally) data-drive the MCP `SetCustomMaterialShaderIncludes` legal-key list.
- [ ] Add an MCP query exposing the full Tier-A helper catalog (key + description + input deps).
- [ ] Saved-project migration/notes for removed Tier-B keys.

### New finding (Phase 0) — per-material shader size grows ~O(N) → total ~O(N²)

Measured per-material WGSL grew 201→215→241 KB as N went 256→512→1024. Cause: the
`bucket_entries` list is templated into the `ClassifyBuckets` struct in **every** per-material
shader (`bind_groups.wgsl`), and it scales with the live bucket count. So each of N shaders
carries an O(N) struct → total emitted WGSL is ~O(N²), and this term will dominate at high N
no matter how lean Phases 1–5 make the per-material *base*.

- [ ] **(candidate phase)** Stop embedding the full `bucket_entries`/`ClassifyBuckets` layout in
      every per-material shader. Options: a fixed-size/runtime-indexed bucket table read from a
      buffer instead of a templated struct; or only emit the offsets a given shader actually reads
      (a shader needs its own bucket's offset, not all N). Needs a design pass — deferred until the
      Tier-A/Tier-B split lands so we're not reshaping `bind_groups.wgsl` twice.

### Out of scope (tracked separately)

- Shared resolve kernel + per-material dispatch (avoid re-embedding the full kernel per
  pipeline). The deferred design makes the material shader *be* the resolve kernel, so
  per-program size is bounded below by the kernel regardless of the above. Larger design
  change under WebGPU’s no-linking model; revisit if Phases 1–5 don’t bring N=1024 within
  budget.

## Verification

- Commit a render-size regression test asserting upper bounds on emitted WGSL for
  representative Custom configs (the probe used in this investigation).
- WGSL-validate (not just string-match) every Custom × include-bit combination.
- `cargo test -p awsm-renderer` green (template render + empty-registry bit-identical
  invariants — first-party pipelines must not change when no dynamic materials exist).
- Re-run `experiments/compare-threejs-materials` at N = 256/512/1024; update `report.md`;
  confirm N = 1024 renders without device loss.

## Expected impact (rough)

Starting from the benchmark's 262 KB (MSAA4+mips, `all()`):

- Phase 0 (declare `empty()`): 262 → ~196 KB
- Phase 3 (Custom emits no model-internal bodies; `all()` is generic-only): ~196 → ~163 KB
- Phase 4 (gate textures/mipmap/lights/vertex-color for a no-texture, no-light material):
  ~163 → ~110–120 KB
- Phase 5 (MSAA path dedup): removes the duplicate shading body (~-15–20 KB at MSAA4)

Net: a truly lean dynamic material lands roughly 2–2.5× smaller, residual dominated by
the fixed deferred-resolve kernel (out-of-scope item). The compartmentalization is the
real deliverable — size is the measurable proxy for it.
