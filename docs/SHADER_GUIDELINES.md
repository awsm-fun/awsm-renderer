# Shader Guidelines

This document captures best practices and gotchas learned from debugging WGSL shaders, particularly for visibility buffer rendering with MSAA.

## Loop Unrolling

### Don't manually unroll loops

WGSL compilers optimize loops with compile-time-known bounds effectively. Manual unrolling using template `{% for %}` blocks can cause issues:

**Bad (template unrolling):**
```wgsl
{% for i in 0..4 %}
    let sample_{{ i }} = textureLoad(tex, coords, {{ i }});
    // ... process sample_{{ i }}
{% endfor %}
```

This template-generates code with different variable names (`sample_0`, `sample_1`, etc.) which can cause unexpected behavior and is harder to debug.

**Good (runtime loop with compile-time bound):**
```wgsl
for (var i = 0u; i < 4u; i++) {
    let sample = load_sample(coords, i);  // Use helper function
    // ... process sample
}
```

### Template constants for loop bounds are fine

Using template values for loop bounds is acceptable because the compiler can still optimize:

```wgsl
for (var s = 0u; s < {{ msaa_sample_count }}u; s++) {
    // This is fine - the bound is known at compile time
}
```

## Variable Declarations in Loops

### Move code to helper functions

Don't declare variables inline in loops or use complex expressions. Instead, extract them to helper functions:

**Bad:**
```wgsl
for (var s = 0u; s < MSAA_SAMPLES; s++) {
    var vis_data: vec4<u32>;
    switch(s) {
        case 0u: { vis_data = textureLoad(tex, coords, 0); }
        // ...
    }
    let triangle_id = join32(vis_data.x, vis_data.y);
    // ... more processing
}
```

**Good:**
```wgsl
fn load_sample_triangle_id(coords: vec2<i32>, s: u32) -> u32 {
    var v: vec4<u32>;
    switch(s) {
        case 0u: { v = textureLoad(visibility_data_tex, coords, 0); }
        case 1u: { v = textureLoad(visibility_data_tex, coords, 1); }
        case 2u: { v = textureLoad(visibility_data_tex, coords, 2); }
        case 3u, default: { v = textureLoad(visibility_data_tex, coords, 3); }
    }
    return join32(v.x, v.y);
}

// Then in main code:
for (var s = 0u; s < MSAA_SAMPLES; s++) {
    let triangle_id = load_sample_triangle_id(coords, s);
    // ... clean, simple processing
}
```

## WGSL textureLoad() Sample Index Requirements

In WGSL, `textureLoad()` for multisampled textures requires the sample index to be a compile-time constant literal. You cannot use a runtime variable:

**Won't compile:**
```wgsl
let sample = textureLoad(msaa_texture, coords, sample_index);  // Error!
```

**Solution - use switch statement:**
```wgsl
fn load_msaa_sample(coords: vec2<i32>, s: u32) -> vec4<f32> {
    var result: vec4<f32>;
    switch(s) {
        case 0u: { result = textureLoad(msaa_texture, coords, 0); }
        case 1u: { result = textureLoad(msaa_texture, coords, 1); }
        case 2u: { result = textureLoad(msaa_texture, coords, 2); }
        case 3u, default: { result = textureLoad(msaa_texture, coords, 3); }
    }
    return result;
}
```

## MSAA Processing Patterns

> **Material authors note**: MSAA is a separate dispatch chain decoupled
> from materials (see
> [`PERFORMANCE.md` §1a](PERFORMANCE.md#1a-msaa-as-a-separate-dispatch-chain-decoupled-from-materials)),
> so opaque material authors **do not write MSAA code**. You author a
> single-sample shading function; the framework's per-shader-id
> `edge_resolve` pipeline drives the per-sample loop around your code
> at edge pixels — specialized to the same feature-set as the primary
> pass (see
> [`contract-opaque.md` § Specialization](dynamic-materials/contract-opaque.md#specialization--the-bucket-cap)),
> which also codifies this dual-context invariant. The patterns below
> apply to **framework code** that handles samples directly —
> `material_classify`, the per-shader `edge_resolve` template,
> `skybox_edge_resolve`, `final_blend` — not to your custom material's
> shading body.

### Shared vs Per-Sample Data

For MSAA resolve, distinguish between:
- **Shared data**: Computed once and reused for all samples (e.g., `standard_coordinates`, `lights_info`)
- **Per-sample data**: Must be loaded/computed for each sample (e.g., visibility data, barycentric coordinates)

```wgsl
// Compute shared data once
let standard_coordinates = get_standard_coordinates(coords, screen_dims);
let lights_info = get_lights_info();

// Process each sample
for (var s = 0u; s < MSAA_SAMPLES; s++) {
    let sample_result = process_sample(
        standard_coordinates,  // Shared
        lights_info,          // Shared
        load_sample_textures(coords, s)  // Per-sample
    );
    // accumulate results...
}
```

### Encapsulate Sample Processing

Create a helper function that processes one sample and returns a result struct:

```wgsl
struct SampleResult {
    color: vec3<f32>,
    alpha: f32,
    is_valid: bool,
}

fn process_sample(
    shared_data: SharedData,
    sample_textures: SampleTextures,
) -> SampleResult {
    // All sample processing in one place
}

fn msaa_resolve_samples(/* ... */) -> ResolveResult {
    var color_sum = vec3<f32>(0.0);
    var valid_count = 0u;

    for (var s = 0u; s < MSAA_SAMPLES; s++) {
        let result = process_sample(shared, load_sample_textures(coords, s));
        if (result.is_valid) {
            color_sum += result.color;
            valid_count++;
        }
    }

    return ResolveResult(color_sum, valid_count);
}
```

## Tint / SPIR-V codegen gotchas

### Dynamic indexing into `vec4<u32>` (and sometimes `array<u32, N>`) writes silently no-ops

Discovered during Stage 3 MSAA debugging. On the current Tint → SPIR-V
/ Metal compile path, writes to a `vec4<u32>` (or, in some
configurations, an `array<u32, N>`) with a dynamic-index `i` inside a
loop **silently NO-OP** — no validation error, no warning, the writes
just don't land. Reads from the dynamic index still work, which makes
the bug invisible until you trace what changed.

**Bad** — writes silently disappear, the array stays at its initial
state for every invocation:

```wgsl
var sample_shader_ids: vec4<u32> = vec4<u32>(0xFFu, 0xFFu, 0xFFu, 0xFFu);
for (var s = 0u; s < 4u; s++) {
    let sid = compute_shader_id_for_sample(s);
    sample_shader_ids[s] = sid;            // NO-OP on Tint/Metal
}
let any_differs = (sample_shader_ids[0] != sample_shader_ids[1])
               || (sample_shader_ids[0] != sample_shader_ids[2])
               || (sample_shader_ids[0] != sample_shader_ids[3]);
```

**Workaround** — fully unroll, use individual `let`s or scalar `var`s,
no dynamic index into a vec4 / array:

```wgsl
let sid_0 = compute_shader_id_for_sample(0u);
let sid_1 = compute_shader_id_for_sample(1u);
let sid_2 = compute_shader_id_for_sample(2u);
let sid_3 = compute_shader_id_for_sample(3u);
let any_differs = (sid_0 != sid_1) || (sid_0 != sid_2) || (sid_0 != sid_3);
```

This pattern shows up across the MSAA classify + edge_resolve shaders;
when adding new per-sample logic, **never write to a `vec4` / array
via a dynamic index inside a loop**. Unroll, or use four scalar
locals.

Symptom to watch for: a shader that "looks correct" but produces
output as if a per-sample state variable was never updated past its
initial value. Diagnose by writing a binary high-contrast colour at
the point the value should have changed (see
[`DEBUGGING-PREVIEW.md`](DEBUGGING-PREVIEW.md)) — if the colour never
appears, the dynamic-index write is being dropped.

### Loop unrolling for the dispatch-time-known case

If a `var` array is small (≤4 elements) AND the loop bound is a
compile-time constant, prefer **fully unrolled scalar `let`s** over
either a `vec4` or `array<u32, N>` for per-sample state. This sidesteps
both the dynamic-index-write bug above and any ambiguity about whether
the compiler will inline-vs-spill the array. The MSAA classify shader
uses this pattern for `sid_0..sid_3`, `seen_0..seen_3`, etc.

---

## Specialize-only materials (skinny shaders)

Every opaque/transparent material pipeline is specialized to one
`shader_id` + `ShadingBase`, and compiles **only the WGSL it actually
uses** — there is no shared "core" pile that every material pays for. The
key pieces:

- **Each material declares its dependencies.** `MaterialShader` exposes
  `shader_includes()` (`ShaderIncludes` bitflags over the optional shared
  modules — `brdf`, `apply_lighting`, `material_color_calc`, `ibl`,
  `light_access`, …) and `fragment_inputs()` (`FragmentInputs` — the
  pre-shade work it needs). PBR declares the full set; unlit/toon declare a
  small subset; a solid-color debug material can declare nothing. Lives in
  `crates/materials/src/shader_includes.rs`.
- **The host emits the transitive closure.** Each shared module declares
  its direct deps; the renderer resolves the closure of `(pass scaffolding
  ∪ material includes)` once per pipeline cache key and gates each
  `{% include %}` behind `{% if inc.<module> %}`. `materials_wgsl` is
  filtered to the pipeline's own base, so a non-PBR pipeline never parses
  the PBR fragment. Net: unlit/toon shed ~1200+ lines of
  BRDF/lighting/PBR-builder vs. the old "everything everywhere" build.
- **Skinny code, stable bindings.** Gating targets function/struct
  *bodies* (where ~all the compile cost is). The `@group/@binding` surface
  stays full, fixed, and pass-owned — a binding declaration is ~free to
  parse, and the dynamic-material ABI relies on stable slot indices. So
  "no core set" is about *code*, not bindings.
- **`light_access` vs `apply_lighting`.** The old `lights.wgsl` was split:
  `light_access.wgsl` (light unpack / `get_light` / `LightsInfo` — cheap,
  always-present infra, like the bindings) and `apply_lighting.wgsl` (the
  punctual-light walk that calls `brdf`, gated, PBR-only). Toon can pull
  `light_access` without dragging in the heavy BRDF.
- **Skybox is its own pipeline.** Skybox / uncovered pixels
  (`triangle_index == U32_MAX`) are written by a dedicated
  `skybox_primary.wgsl` pass — no material pipeline owns the skybox, so a
  scene that is "unlit + skybox" compiles zero PBR.

When adding a material or shared module, declare its includes/inputs (and a
module's direct deps) accurately: an over-declaration just compiles dead
code, but an **under-declaration fails to resolve a referenced symbol**.
WGSL allows module-scope forward references, so *every referenced symbol —
even one inside an uncalled function — must still be defined*; that is what
drives the gating granularity (e.g. a helper that names a PBR type must be
gated together with the PBR include, not left ungated).

### Prep pass: read-vs-recompute (keeping shaders skinny)

The other half of "skinny shaders" is the **prep pass** (`render_passes/material_prep/`).
Before per-material shading, one compute pass materializes the **material-independent**
per-pixel work into buffers; the per-material kernel then reads it. This both saves the
recompute *and* lets the materialized work's code drop out of every specialized module.
It is unconditional — there is no prep on/off flag and no prep-vs-non-prep variant; the
opaque path always preps. (Transparent is forward, so it has no visibility buffer to read
prep from and recomputes inline — a different model, not a flag.)

**What to prep vs. what to recompute — the rule.** Prep materializes work only when the
read beats the recompute, *or* when caching it evicts bulky code from the material
modules. Trivially-cheap work is left to be re-derived in the shading wrapper:

| Work                         | Decision   | Why |
|------------------------------|------------|-----|
| Per-light shadow visibility  | **Prep**   | Expensive `sample_shadow_*` block; caching it (full-screen for interiors + a compact per-edge-sample buffer, `EdgeShadowBuffer`, for MSAA silhouettes) lets ~50 KB of sampling code drop from the MSAA opaque module — the bulk of its size win. |
| Interior UV0 / vertex-color  | **Prep**   | Compute ≈ a wash either way, but the gather code then lives once in the prep shader instead of being duplicated into every material variant, and it piggybacks on the shadow prep pass that runs anyway. |
| **Edge** UV0 / vertex-color  | Recompute  | The edge shading arm already holds the per-sample triangle + barycentric in-register, so the lerp is a few reads — cheaper than computing it in `cs_prep_edge`, writing it, reading it back, plus ~16–48 MB of VRAM, to evict ~10 lines of code. |
| World position               | Recompute  | Cheap depth re-projection; never worth storing. |

In short: **prep the expensive common work; re-derive the trivially-cheap work.** The
canonical statement of the rule lives in `material_prep/buffers.rs`.

**It's invisible to material authors.** A material (built-in *or* dynamic) never writes a
gather. The shading wrapper computes the single always-needed values once and exposes them
as fields on `OpaqueShadingInput` (`input.world_position`, `input.world_normal`, …), and
forwards the ingredients for the multi-set attributes behind accessor functions
(`material_uv(input, set)`, `material_vertex_color(input, set)`). The accessor decides
prep-read vs recompute internally (interior → prep buffer; edge → in-register recompute),
so the read-vs-recompute split never leaks into authored shading code.

---

## General Best Practices

1. **Keep functions small and focused** - easier to debug and maintain
2. **Use descriptive struct names** - `MsaaSampleTextures` over `TexData`
3. **Early exit for invalid cases** - check for `U32_MAX`, zero values, etc.
4. **Match main code path logic** - MSAA sample processing should mirror the main non-MSAA path
5. **Avoid magic numbers** - use named constants
6. **Comment non-obvious optimizations** - especially for lazy-loading patterns
