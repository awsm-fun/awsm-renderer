# Skinny Materials — per-material shader compilation

> Status: **in progress** (branch `skinny-materials`). Core mechanism landed; see
> "Progress" below. Owner decisions captured inline as ✅.
> This lives in `docs/plans/` — a temporary working file (vs. `docs/*.md`, which
> is permanent reference). Delete it once the work lands.

## Progress (as of this writing)

**Landed (committed, compiles, NOT yet GPU-verified — per §0/§11 verify at the end):**
- ✅ **ShaderIncludes + FragmentInputs infra** (`crates/materials/src/shader_includes.rs`):
  bit-sets + per-module direct-dep table + transitive-closure resolver + unit tests.
  `MaterialShader::shader_includes()/fragment_inputs()`; per-material module consts
  (PBR full; toon = light_access+textures+camera; unlit/flipbook/scanline =
  textures+color_space; dynamic = conservative `all()`). Registry carries them +
  `declarations_for_shader_id()`.
- ✅ **lights.wgsl split** → `light_access.wgsl` (toon+pbr) + `apply_lighting.wgsl`
  (pbr-only → brdf). All 4 hosts include both. Pure reorg.
- ✅ **brdf + apply_lighting gating** wired into the renderer (`resolved_includes_for_base`
  + `ShaderIncludeFlags` in `dynamic_materials`; `inc` field on the opaque-compute,
  edge-resolve, and transparent-includes templates). Hosts gate `brdf.wgsl` (889) +
  `apply_lighting.wgsl` (375) behind `{% if inc.brdf/apply_lighting %}`. PBR keeps
  both (byte-identical); **unlit/toon/flipbook drop ~1264 lines** of BRDF/lighting.
- ✅ **PBR material-color builder gating** (`inc.material_color_calc`): the `_pbr_*`
  helpers in `material_color_calc.wgsl` (~700) + their callers (`compute_material_color`
  in material_shading, `pbr_get_gradients` in mipmap) gated. Unlit builder stays ungated.
- ⏪ **materials_wgsl filtering — IMPLEMENTED THEN REVERTED** (`b33ee8d` → `60eda69`).
  Filtering to the base's fragment broke non-base pipelines: ungated helpers reference
  filtered-out material *types* (`compute_unlit_material_color` → `UnlitMaterial`; the
  transparent `material_color_calc` PBR refs → `PbrMaterial`). Fixing needs per-type
  base-gating of those helpers in BOTH passes (transparent `includes` template lacks a
  `base` field — would need one added). Deferred. The include gating above is independent
  and stands.

**✅ VERIFIED (in-browser GPU, the real check):**
- PBR **byte-identical**: AlphaBlendMode (IBL+skybox+alpha) max-abs **0**; AnisotropyBarnLamp
  (anisotropy+clearcoat) max-abs **0** — vs same-renderer baselines.
- UnlitTest renders correctly with its skinny pipeline (no brdf/apply_lighting/PBR-builder).
- The GPU check earlier CAUGHT the materials_wgsl-filter breakage (unresolved `UnlitMaterial`),
  which is why filtering was reverted. Verification works; the dev server DOES rebuild on
  renderer `.wgsl` changes (must `cargo build -p awsm-renderer` first to avoid trunk lock
  contention, then `touch model-tests/src/main.rs`).

**Key implementation findings (for whoever continues):**
- `material_color_calc.wgsl` is **shared** — it defines BOTH `compute_material_color`
  (PBR) *and* `compute_unlit_material_color` (unlit, ~line 739). It **cannot** be gated
  wholesale; to gate the PBR half it must be split, or its PBR fns wrapped in
  `{% if %}` (they're also referenced by `material_shading.wgsl`'s `compute_material_color`
  and `mipmap.wgsl`'s `pbr_get_gradients` — gate those PBR fns together).
- `debug_to_copy.wgsl` references `brdf_*` but is **dead** (included nowhere) — ignore it.
- `light_access.wgsl` is called by the **pass scaffolding** (`get_lights_info()` at
  compute.wgsl:309) *before* the shade dispatch, so gating it requires also gating that
  call behind `FragmentInputs::LIGHTS` (the §5 input-gating work). Left unconditional
  for now.
- WGSL allows module-scope **forward references**, so include order doesn't matter and
  every *referenced* symbol (even in uncalled fns) must still be defined — that's what
  drives the cascade gating above.
- `ShadingBase` has no `Scanline` variant; scanline → `Custom` → conservative `all()`.
- Whitespace from `{% if %}` wrappers is harmless (templates use `whitespace="minimize"`);
  GPU pixels are byte-identical for PBR regardless.

**Remaining (suggested order, see §10 — reorder freely):**
- Filter `materials_wgsl` to the pipeline's own base (drops other materials' bodies,
  incl. the big PBR fragment from non-PBR pipelines). Mind the dynamic/Custom path
  (`dynamic_wgsl_fragment` is separate).
- Split/gate `material_color_calc.wgsl` PBR half (+ `material_shading`/`mipmap` PBR fns).
- Fragment-input gating (gate `get_lights_info()` + TBN unpack by declared inputs) — this
  also unlocks gating `light_access` for unlit.
- Gate the remaining hosts (`empty.wgsl`).
- Dedicated skybox writer (A1) — §7.1.
- Per-extension PBR lobe gating — §7.4.
- **Then** the §11 verification pass (capture PBR baselines from the base commit
  `c567ef5`, diff for max-abs 0; confirm unlit/toon render identically; measure shrink).

## 0. How to work on this ✅

- **Breaking things is fine and expected.** This is a large, cross-cutting
  refactor; intermediate states will not compile or render correctly. That's OK.
- **Do not test until *everything* has landed.** No interim GPU checks, no
  "does it still render" between stages. Verification is a single pass at the
  very end (§11). Testing mid-flight here is wasted effort.
- **Commit freely in broken states.** Make small, logical commits — even
  non-compiling ones — if they make the history easier to bisect later. Lean on
  commit granularity rather than on keeping each step green.
- **The staging in §10 is a suggestion, not a mandate.** If a different order
  (or a different decomposition entirely) turns out to make more sense once
  you're in the code, do that. The decisions in §3–§9 are what's fixed; the path
  to them is not.

## 1. Goal & motivation

Every material pipeline today compiles the *same* large pile of WGSL regardless
of shading model. A trivial unlit (or solid-color debug) pipeline still parses
the full PBR BRDF and more. We want each pipeline to compile **only the WGSL it
actually uses**, so simple shaders are cheap to compile and the minimal case
approaches "emit a single color, include nothing."

Concretely, the cost we're cutting (line counts as of this writing):
- `shared_wgsl/lighting/brdf.wgsl` — **889**
- `material_opaque/.../helpers/material_color_calc.wgsl` — **856**
- `shared_wgsl/lighting/lights.wgsl` — **554**

…all compiled into *every* opaque pipeline, including unlit/toon.

## 2. Current state (the bloat)

1. **All materials concatenated.** `build_materials_wgsl()` emits *every* enabled
   material's `wgsl_fragment` into one module, even though each opaque pipeline
   is already specialized to one `shader_id` + `base`
   (`material_opaque/shader/template.rs`).
2. **Static shared includes.** The shading hosts (`compute.wgsl`,
   `edge_resolve.wgsl`, transparent `includes.wgsl`) `{% include %}` the shared
   modules unconditionally — so lighting/brdf/material_color_calc land in every
   pipeline.
3. **Pre-shade inputs always unpacked.** TBN/normals, barycentric-derived
   attributes, and `get_lights_info()` are computed before the shade call
   regardless of whether the material uses them.
4. **PBR owns the skybox.** `owns_skybox = (shader_id == PBR)`; classify routes
   skybox/uncovered pixels to bucket bit 0 (PBR). The canonical PBR bucket emits
   the *full* PBR shading body purely to run `sample_skybox` — so **any scene
   with a skybox compiles a PBR pipeline, even with zero PBR materials.**
   (Note: the MSAA *edge* skybox already has a decoupled minimal writer,
   `skybox_edge_resolve.wgsl` — only `color_space`+`camera`+`math`+`skybox`.)

## 3. Design principles ✅

- **No global core set.** Nothing is force-added to a material. Built-ins
  declare what they use; a material may declare *zero* includes and *zero*
  inputs.
- **Declare what you use.** Each material declares its shared-module includes and
  its pre-shade fragment inputs. The pass declares its own routing scaffolding.
- **Closure resolution.** Each shared module declares its direct deps; the
  renderer compiles the transitive closure of
  `(pass scaffolding ∪ material includes ∪ input-implied includes)`.
- **Skinny *code*, stable *bindings*.** ✅ Gating targets WGSL **code** (function
  and struct bodies — where ~all the compile cost is). The `@group/@binding`
  surface stays fixed and pass-owned; gating a module does **not** drop its
  bindings (see §9 for why). "No core set" is a statement about *code*, not about
  the binding ABI.

### Material vs. pass scaffolding ✅
A material's `wgsl_fragment` is only its `compute_*_color(...)` **shading body**;
it receives resolved inputs and returns a color. The **pass scaffolding** (the
`@compute` entry: visibility read, coords, `shader_id` guard, call, output write)
is pass-owned, identical for every material in that pass, and declares *its own*
minimal includes — it is **not** a per-material tax. A solid-color material's
body pulls nothing; the pass still routes the fragment to it.

## 4. Trait API (`crates/materials/src/shader.rs`)

```rust
pub trait MaterialShader {
    // …existing…
    fn shader_includes(&self) -> ShaderIncludes;   // bitflags, may be empty
    fn fragment_inputs(&self) -> FragmentInputs;    // bitflags, may be empty
}
```

- `ShaderIncludes` — bitflags over the *optional* shared modules
  (`light_access`, `apply_lighting`, `brdf`, `material_color_calc`, `ibl`,
  `shadows`, `skybox`, `extras`, `vertex_color`, …). Each flag maps to a module
  descriptor `{ wgsl_key, deps: ShaderIncludes }`. (Modules name the bindings they
  *use*, but those bindings are always declared by the pass — see §9 — so the
  descriptor doesn't drive bind-group layouts.)
- `FragmentInputs` — bitflags over pre-shade work
  (`NORMALS | TANGENTS | UV | LIGHTS | VIEW_DIR | INSTANCE_TINT | …`). The
  scaffolding only unpacks/computes the declared ones; an input may pull an
  include (e.g. `LIGHTS ⇒ light_access` + the lights bind group).
- Dynamic materials carry their declared sets in the dynamic shader cache key
  (`DynamicShaderInfo`).

## 5. Module & dependency model

Each shared module gets a stable descriptor (key + deps). The renderer resolves
the closure once per pipeline cache key. Key dependency edges (after the lights
split, §7.2) — these are *code* deps; the bindings the code touches are always
present (§9):

- `apply_lighting` → `brdf` → `math`, `camera`
- `brdf` (IBL paths) uses the IBL bindings (13–18, always declared)
- `light_access` uses the lights group (group 1, always declared)
- `material_color_calc` (PBR) → `textures`, `camera`, per-extension lobes
- `skybox` (writer) → `camera`, uses the skybox bindings (11–12)

## 6. Per-material declarations (target)

| Material | Includes | Inputs |
|---|---|---|
| **PBR** | light_access, apply_lighting, brdf, material_color_calc, ibl, shadows, textures, camera, math, extras | normals, tangents, uv, lights, view_dir |
| **Toon** | light_access, camera, textures | normals, lights, view_dir |
| **Unlit** | textures | uv |
| **Solid-color debug** | — | — |
| **Skybox writer** (pass-level) | camera, skybox | — |
| **Dynamic** | declared at registration | declared at registration |

## 7. Structural refactors

### 7.1 Dedicated skybox writer (A1) ✅
Extend the existing `skybox_edge_resolve` pattern to the *primary* skybox write:
a minimal pipeline including only `camera` + `skybox.wgsl` + the visibility
plumbing. Classify gets a **dedicated skybox tile/sample list** (mirroring its
existing skybox *edge* list) — **not** a material bucket bit, so
`MAX_BUCKET_WORDS` is untouched. Delete `owns_skybox` from the PBR cache
key/template and its `{% if owns_skybox %}` branches. Result: unlit+skybox
compiles **zero** PBR. (Verify: skybox renders pixel-identical; MSAA silhouette
edges still handled by the existing edge resolve.)

### 7.2 Split `lights.wgsl`
Into `lighting/light_access.wgsl` (get_light, light_to_brdf, LightsInfo,
attenuation — toon + pbr) and `lighting/apply_lighting.wgsl` (the
`apply_lighting*` orchestration that calls into `brdf` — pbr only). Without this,
"toon needs lights" drags in brdf and defeats the point. Pure reorg ⇒ expect
byte-identical output.

### 7.3 Filter `materials_wgsl`
Emit only the pipeline's own `shader_id`/`base` material body, not the concat of
all enabled materials.

### 7.4 Per-extension PBR lobe gating
Wrap the clearcoat/sheen/iridescence/anisotropy/transmission lobe *definitions*
in `brdf.wgsl` + `material_color_calc.wgsl` behind their existing `pbr_features`
`{% if %}`, so a plain metallic-roughness PBR variant stops compiling those.

## 8. Gating mechanism

Includes stay Askama sub-templates (they reference parent context like
`pbr_features`, `use_froxel_lights`), so the mechanism is conditional inclusion,
not Rust-side concatenation:

```wgsl
{% if includes.brdf %}{% include "shared_wgsl/lighting/brdf.wgsl" %}{% endif %}
```
…across the three shading hosts (opaque `compute`, opaque `edge_resolve`,
transparent `includes`), plus `{% if inputs.normals %}…{% endif %}` around the
pre-shade unpacking. The host template gets an `includes`/`inputs` struct filled
from the specialized material's closure.

## 9. Bindings: leave them alone ✅

**Decision: do not gate or reindex bindings. The `@group/@binding` surface stays
exactly as it is today — full, stable, pass-owned.** We gate *code* only.

Current opaque bindings (canonical indices, `bind_groups.wgsl`), kept as-is:

- **group(0)** — 0–4 geometry textures, 5 visibility_data, 6 material_mesh_metas,
  7 materials, 8 transforms, 9 texture_transforms, 10 camera, 11–12 skybox,
  13–18 IBL (env/irradiance/brdf_lut), 19 opaque_tex (out), 20 instance_attrs,
  21 classify_buckets, 22 frame_globals, 23 extras_pool.
- **group(1)** — 0 lights_info, 1 lights, 2 lights_storage, 3 cull_params.
- **group(3)** — shadows.

### Why not skinny bindings
- **The cruft is ~free.** Compile cost lives in function/struct *bodies*
  (`brdf.wgsl` 889, `material_color_calc.wgsl` 856, lighting orchestration) — the
  ~2,600 lines we gate. A binding *declaration* is one line, parses instantly, and
  emits no code when nothing samples it. At runtime, bind groups are set per-pass
  (not per-invocation) and an unused, un-sampled texture costs nothing on the GPU.
  So keeping the full binding surface captures ~all the compile-time win anyway.
- **Dynamic shaders need a stable binding ABI.** Author-supplied fragments
  reference texture slots (the shared texture pool, camera, etc.) at known,
  fixed indices. Per-pipeline skinny bindings would shift that surface and break
  the contract dynamic (and built-in) fragments compile against.
- **Skinny bindings would *add* complexity for that near-zero win** —
  per-pipeline layouts keyed by declared-binding-set, more distinct layout cache
  entries, and the "empty straddled group" caveat (WebGPU pipeline layouts are a
  dense array of group layouts). Not worth it.

Net: the renderer keeps its single, stable opaque bind-group layout; pipelines
just contain less *code*. (The dedicated skybox writer in §7.1 is the one place a
genuinely smaller binding set is natural — it's a separate, non-material pipeline
that only ever needs camera + skybox + the visibility plumbing, so it gets its
own small layout regardless. That's not "skinny bindings on a material," it's a
distinct pass.)

## 10. Staging — a *suggested* order (not a mandate ✅)

Reorder, merge, or split these freely if the code suggests otherwise (see §0).
This is one reasonable decomposition; the fixed part is the end state (§3–§9),
not the path. Note "byte-identical" below describes the *expected* effect on
final output once a stage is complete — **not** a checkpoint to test at (we don't
test until the end, §0/§11).

1. **Skybox decouple (A1).** Standalone; extends the existing edge-resolve
   pattern. Drops PBR's skybox ownership.
2. **Split `lights.wgsl`** into `light_access` + `apply_lighting`. Pure reorg.
3. **Include scaffolding** (`ShaderIncludes` + descriptors + closure) and wire
   the three hosts to gate; PBR declares the full set.
4. **Skinny material declarations** (toon/unlit/dynamic) ⇒ toon/unlit shrink.
5. **Fragment-input gating** (skip pre-shade unpack the material doesn't need).
   Bindings stay full — no per-pipeline layout work (§9).
6. **Per-extension PBR lobe gating**.

## 11. Verification — once, at the very end ✅

**Do not verify until the entire refactor has landed** (§0). Intermediate states
are expected to be broken; checking them is wasted effort. When it's all in:

- `cargo fmt` + `clippy` + the test suite (160 tests) green.
- **In-browser GPU verification** — the only way to confirm pixels. Use the
  Claude browser plugin over MCP against a real Chrome instance and capture the
  canvas pixels; the full workflow (dev servers, tab/foreground requirements,
  `rAF → drawImage → toBlob → POST :9999` capture recipe, PIL max-abs diffing)
  is documented in [`docs/DEBUGGING-PREVIEW.md`](../DEBUGGING-PREVIEW.md). Follow
  it rather than re-deriving.
- **Byte-identical GPU render on PBR assets** (zero pixel change is the hard
  gate): AlphaBlendMode, a clearcoat/sheen/transmission asset, the
  DiffuseTransmissionPlant — max-abs diff = 0 vs. pre-refactor baselines captured
  before stage ①.
- Toon/Unlit assets render visually identical before/after.
- Report measured emitted-WGSL size + compile-time drop per skinny pipeline
  (unlit, toon, solid-color) — the whole point of the exercise.

> Capture the PBR baselines *before* starting stage ① (while `main`/pre-refactor
> output is reachable), since everything in between is expected to be broken.

## 12. Open risks / notes

- Three shading hosts must stay in sync (opaque compute, opaque edge_resolve,
  transparent includes).
- Dynamic-material registration API gains include/input declarations.
- Bindings are intentionally **not** touched (§9), so there's no per-pipeline
  layout work and no risk to the dynamic-shader binding ABI. The binding surface
  stays full and stable; pipelines just carry less code.
- Pass scaffolding + the full binding surface stay the compile-cost floor; the
  wins come from gating the heavy shading modules + per-pipeline material body,
  not from the routing or the bindings.
