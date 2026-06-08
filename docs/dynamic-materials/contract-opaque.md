# Opaque dynamic-material WGSL contract

This is the load-bearing surface for authoring a runtime-registered **opaque**
custom material (`alpha_mode = Opaque` or `alpha_mode = Mask { cutoff }`).
For transparent materials (`alpha_mode = Blend`) see
[contract-transparent.md](contract-transparent.md).

> Single source of truth. This document — together with
> [contract-transparent.md](contract-transparent.md) — is the published
> author contract. The renderer's template substitution emits exactly the
> shape described here; the `material-editor` "Contract" pane renders this
> file inline. If the contract changes, this file changes first and the
> renderer follows.

---

## Specialization & the bucket cap

The renderer is **specialize-only**: every shader is gated at compile time
to exactly the features it needs (there is no shared "uber" shader). Each
registered custom material compiles into its **own pipeline** — a "bucket",
keyed by its registry-assigned `shader_id`. (First-party PBR materials
instead share a bucket per *feature-set*; custom materials are never
deduped — one registration is always one bucket.)

The total number of buckets across the whole renderer (first-party PBR
feature-sets + every custom registration) is capped at
`MAX_BUCKET_ENTRIES` = `MAX_BUCKET_WORDS × 32` (default **32** — the
classify pass packs one bucket bit per `u32` of its tile mask). Registering
a material that would push past the cap is a **hard error**
(`AwsmDynamicMaterialError::BucketCapExceeded`) — there is no silent
fallback. To allow more, raise `MAX_BUCKET_WORDS` in
`packages/crates/renderer/src/dynamic_materials/mod.rs` and rebuild.

Registration is **transactional**: you submit a batch, and if any entry is
invalid (duplicate name, reserved field name, WGSL compile failure, or cap
overflow) the whole batch is rejected and nothing is registered.

---

## How your fragment is injected

Your `shader.wgsl` is wrapped at template-emission time into:

```wgsl
fn custom_shade_dynamic(input: OpaqueShadingInput) -> OpaqueShadingOutput {
    // <your shader.wgsl body, verbatim>
}
```

The wrapper is named `custom_shade_dynamic` literally — not parameterized
by the registry-assigned shader id. Each dynamic material's WGSL is
template-instantiated as its own pipeline (the `shader_id` lives in the
`ComputePipelineCacheKey`), so collisions on the function name aren't a
concern: every pipeline has its own copy of the wrapper. The opaque
compute kernel dispatches one workgroup per tile containing your
material's `shader_id` (driven by the classify pass) and, per pixel,
calls `custom_shade_dynamic(input)` then writes its output to
`opaque_tex`.

Your fragment **must end with `return OpaqueShadingOutput(...)`**. You may
declare local helper functions inside the function body but cannot declare
new top-level items (structs, globals) — the substitution wraps the
fragment in a function and naga only permits item declarations at module
scope.

If you need extra structs or helpers, declare them above the function
body — at the top of `shader.wgsl`, *outside* the implicit
`custom_shade_dynamic` wrapper. The renderer emits all author-declared items
at module scope before the wrapper.

### Dual-context invariant — primary opaque AND edge_resolve

Your fragment is wrapped into **two** compute kernels per `shader_id`,
not one: the **primary opaque** kernel (full-pixel shading across the
tile) and the per-shader-id **edge_resolve** kernel (single-sample
shading at MSAA boundary pixels — see
`packages/crates/renderer/src/render_passes/material_opaque/edge_pipeline.rs`
and `…/shader/edge_template.rs`). The same `custom_shade_dynamic` body is
emitted into both; the wrapper supplies the right `OpaqueShadingInput`
in each context (full pixel vs. masked sub-sample). There is no PBR
`msaa_resolve_samples` fallback — cross-material MSAA edges render with
your exact shading code, not a generic substitute. Write one fragment;
keep it free of state that assumes a particular call-site, and both
contexts work without any extra opt-in.

---

## Input — `OpaqueShadingInput`

```wgsl
struct OpaqueShadingInput {
    // Per-pixel data ----------------------------------------------------
    coords: vec2<i32>,              // pixel coordinate (output texture space)
    screen_dims: vec2<u32>,         // output texture dimensions
    triangle_index: u32,            // visibility buffer triangle index
    barycentric: vec3<f32>,         // interpolated barycentric (sums to 1)
    main_instance_id: u32,          // INSTANCE_ATTR_NONE if no per-instance tint
    // Shading-frame data ------------------------------------------------
    world_normal: vec3<f32>,        // world-space normal
    world_position: vec3<f32>,      // world-space surface position
    surface_to_camera: vec3<f32>,   // normalized vector from surface to camera
    // Per-material data -------------------------------------------------
    material_offset: u32,           // byte offset for material_load_* calls
    material: MaterialData,         // your auto-generated struct (see below)
}
```

Field order mirrors the emitted struct exactly (see
`material_opaque_wgsl/compute.wgsl::OpaqueShadingInput`). The
wrapper exposes the world-space normal but does NOT pre-compute a
tangent / bitangent frame — authors that need one reconstruct it
themselves from `world_normal` + the per-pixel UV derivatives.
Most dynamic materials (overlay effects, scanlines, simple PBR
tints) don't need a TBN, so the wrapper trades flexibility for
keeping the per-pixel cost low.

`MaterialData` is **auto-generated** from your `material.json` layout — see
"Per-material data" below.

---

## Output — `OpaqueShadingOutput`

```wgsl
struct OpaqueShadingOutput {
    // Linear HDR color (the kernel writes it directly to `opaque_tex`;
    // tonemap + display-encode is the post-processing pass's job).
    color: vec3<f32>,
    // Final alpha — for opaque materials, normally `1.0`. For
    // alpha-masked (`alpha_mode: Mask`), set to `0.0` for discarded
    // fragments — the kernel passes through your alpha to the output
    // and downstream passes treat `alpha < 1.0` as transparent in the
    // alpha-aware sort.
    alpha: f32,
}
```

There is no `discard` on the compute side, and the kernel **always**
`textureStore`s whatever your wrapper returns — a material cannot skip the
write by early-returning (the returned value is written regardless). To
represent a dropped / cut-out fragment, return `alpha = 0.0` (with
`alpha_mode: Mask`): the kernel passes your alpha through to `opaque_tex`
and downstream passes treat `alpha < 1.0` as transparent in the alpha-aware
sort (see `OpaqueShadingOutput.alpha` above). Skybox / uncovered pixels
(`triangle_index == U32_MAX`) are not your concern — a dedicated skybox
pipeline (`skybox_primary.wgsl`) writes them in a separate pass.

---

## Per-material data — `MaterialData`

The renderer emits a `struct MaterialData { ... }` declaration above your
fragment, derived from your `material.json` layout. Field order:

1. **Uniforms** in declaration order, WGSL alignment-respecting:
   - `F32` → `f32`
   - `Vec2` → `vec2<f32>`
   - `Vec3` → `vec3<f32>` (16-byte aligned; 12 bytes payload + 4 padding)
   - `Vec4` → `vec4<f32>`
   - `U32` → `u32`
   - `IVec2` / `IVec3` / `IVec4` → `vec2<i32>` / `vec3<i32>` / `vec4<i32>`
   - `Mat3` → `mat3x3<f32>` (16-byte aligned; 48 bytes payload)
   - `Mat4` → `mat4x4<f32>`
   - `Color3` → `vec3<f32>` (UI-only distinction)
   - `Color4` → `vec4<f32>` (UI-only distinction)
   - `Bool` → `u32` (0 / 1)
2. **Texture slots** in declaration order, one `<name>_index: u32` per
   slot.
3. **Buffer slots** in declaration order, one `<name>_offset: u32` and
   one `<name>_length: u32` per slot.

Example: a layout with uniforms `[tint: Color3, scan_freq: F32]`,
textures `[base]`, and no buffers emits:

```wgsl
struct MaterialData {
    tint: vec3<f32>,        // 12 bytes
    _pad0: u32,             // align next field to 16 (vec3 padding)
    scan_freq: f32,
    base_index: u32,        // <name>_index      (array_index | layer<<12)
    base_uv_sampler: u32,   // <name>_uv_sampler (uv_set | sampler<<8)
}
```

Inside `custom_shade_dynamic`, the wrapper has already populated `input.material`
for you — read fields directly: `let tint = input.material.tint;`.

---

## Helpers in scope

Every symbol declared in the renderer's `shared_wgsl/` directory is in
scope for your fragment. The most useful:

### `shared_wgsl/frame_globals.wgsl`

```wgsl
struct FrameGlobals {
    time: f32,            // seconds since renderer construction (monotonic)
    delta_time: f32,      // seconds since previous render() call
    frame_count: u32,
    resolution: vec2<u32>,
}
```

Read via `let fg = frame_globals_from_raw(frame_globals_raw);`. See
[`docs/TEMPORAL_SHADERS.md`](../TEMPORAL_SHADERS.md) for the wall-clock /
fixed-step / paused semantics of `time` and `delta_time`.

### `shared_wgsl/camera.wgsl`

```wgsl
let camera = camera_from_raw(camera_raw);
// camera.view, camera.projection, camera.position, ...
```

### `shared_wgsl/textures.wgsl`

For each texture slot `<name>` you declare, the renderer **generates two
helper functions** for you — use these instead of decoding the raw slot
words by hand:

```wgsl
// Sample the bound texture at `uv` (LOD 0 in the opaque/edge kernels,
// hardware-derivative in the transparent kernel). Returns transparent
// black when the slot is unbound.
fn material_sample_<name>(m: MaterialData, uv: vec2<f32>) -> vec4<f32>

// The reconstructed descriptor, if you need array/layer/sampler directly.
fn material_<name>_texture_info(m: MaterialData) -> TextureInfo
```

So sampling is a one-liner:

```wgsl
let base = material_sample_base(input.material, uv).rgb;
```

Under the hood the slot is two `u32` words — `<name>_index`
(`array_index | layer<<12`) and `<name>_uv_sampler` (`uv_set | sampler<<8`),
matching `shared_wgsl/textures.wgsl::TextureInfoRaw`. The generated helper
unpacks both, builds a `TextureInfo` (identity UV-transform), and calls the
kernel's variant-agnostic `texture_pool_sample`. You normally never touch
the raw words; `material_<name>_texture_info` is the escape hatch if you do.
```

For convenience, the kernel exposes the per-pixel barycentric UV via
`texture_uv(...)` (see `material_opaque_wgsl/helpers/texture_uvs.wgsl`).

### `shared_wgsl/lighting/`

`brdf.wgsl` (Schlick / Lambert / GGX), `light_access.wgsl` (light unpack
/ `get_light` / `LightsInfo`), `apply_lighting.wgsl` (the punctual-light
walk → `brdf`). (`lights.wgsl` was split into `light_access.wgsl` +
`apply_lighting.wgsl` — see the specialize-only materials notes in
`docs/SHADER_GUIDELINES.md`.) The unlit output helper `compute_unlit_output`
is **not** a shared module — it lives in the unlit material fragment
(`crates/materials/src/wgsl/unlit_material.wgsl`).

### `shared_wgsl/material.wgsl`

`material_load_u32(index)`, `material_load_f32(index)`,
`material_load_shader_id(byte_offset)`. The auto-generated wrapper has
already loaded your `MaterialData` for you, so direct calls to these are
rarely needed.

### `shared_wgsl/extras.wgsl`

```wgsl
extras_load_u32(index)         // raw u32 word from the extras pool
extras_load_f32(index)         // bitcast<f32> of the same word
extras_load_vec4_f32(index)    // 4 consecutive f32 words as a vec4
```

For each `BufferSlot`, `MaterialData.<name>_offset` is the index into
the extras pool where your slice starts; `MaterialData.<name>_length` is
its length in u32 words. Example:

```wgsl
// Read the i'th vec4 from a `BufferSlot` named "frames":
let base = input.material.frames_offset + i * 4u;
let cell = extras_load_vec4_f32(base);
```

### `shared_wgsl/shadow/bind_groups.wgsl`

Shadow-sampling helpers are bound on the opaque pass. The kernel
already weaves them into PBR / Unlit / Toon's lighting calls; custom
materials wanting shadow reception should call `apply_lighting(...)` or
`apply_lighting_per_froxel(...)` from `lighting/apply_lighting.wgsl` and
forward the `receive_shadows` mask from `input.material_offset`'s mesh
meta.

---

## Reserved names

Your layout cannot use any of these names for a uniform, texture, or
buffer slot — they collide with kernel-provided symbols:

`material`, `texture_pool`, `extras_pool`, `frame_globals`, `camera`,
`frag`, `vert`.

The loader (`load_material_folder`) rejects layouts that violate this
with `MaterialFolderError::ReservedName`.

---

## Skybox

Skybox / uncovered pixels (`triangle_index == U32_MAX`) are written by a
dedicated `skybox_primary.wgsl` pipeline in its own pass — **no** material
pipeline (PBR, built-in, or custom) owns the skybox anymore. Your material
kernel simply early-returns (no write) on skybox pixels; the skybox pass
fills them. A mixed-material tile is therefore safe: your material writes
its covered pixels, the skybox pass writes the uncovered ones, with no
double-write and no per-material skybox cost.

---

## Alpha mode

`alpha_mode = Opaque` routes through the opaque compute kernel
(this contract).

`alpha_mode = Mask { cutoff }` ALSO routes through the opaque compute
kernel — the alpha-mask discard happens via your fragment setting
`output.alpha = 0.0` when `sampled.a < cutoff`, and the downstream
transparency pass picks up the partially-transparent fragments for
alpha-aware sorting.

`alpha_mode = Blend` routes through the transparent fragment shader —
see [contract-transparent.md](contract-transparent.md).

Custom materials cannot override the alpha-mode-driven routing
(`is_transparency_pass()` derives from `alpha_mode` directly). If you
need finer routing (e.g. an opaque material that uses the transparency
pass for transmission like PBR does), implement it as a built-in
(first-party) material instead.

---

## Example — scanline

```wgsl
// shader.wgsl for the scanline material
//
// Layout (material.json):
//   uniforms: [tint: Color3 = [0.6, 0.9, 0.6],
//              scan_freq: F32 = 80.0,
//              scan_speed: F32 = 0.5,
//              scan_strength: F32 = 0.3]
//   textures: [base]
//   buffers:  []
//
// alpha_mode: Opaque, double_sided: false

let fg = frame_globals_from_raw(frame_globals_raw);
let uv = vec2<f32>(f32(input.coords.x), f32(input.coords.y))
       / vec2<f32>(f32(input.screen_dims.x), f32(input.screen_dims.y));

// Sample the `base` texture slot. `material_sample_<name>` is the
// renderer-generated, variant-agnostic helper for each declared texture —
// no offset math, correct sampler + UV, unbound-safe. Do NOT hand-roll
// `material_load_texture_info_raw(...)` or call `texture_pool_sample_no_mips`
// / `texture_pool_sample_grad` directly (each exists in only one mipmap
// variant; a custom fragment compiles into both).
let base = material_sample_base(input.material, uv).rgb;

// Animated horizontal scanline pattern.
let scan = sin(uv.y * input.material.scan_freq
             + fg.time * input.material.scan_speed);
let overlay = mix(vec3<f32>(0.0), input.material.tint,
                  scan * input.material.scan_strength);

let color = base + overlay;
return OpaqueShadingOutput(color, 1.0);
```

This is the dynamic worked example shipped at
[`assets/test-materials/scanline/`](../../assets/test-materials/scanline)
(`material.json` + `shader.wgsl`).
