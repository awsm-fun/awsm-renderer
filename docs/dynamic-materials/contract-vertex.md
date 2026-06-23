# Vertex-displacement dynamic-material WGSL contract

This is the load-bearing surface for authoring the **third** custom-material
WGSL window: the runtime **vertex-displacement hook**. It is independent of the
alpha-mode of the material — opaque, mask, and blend materials may all declare
one. For the fragment (shading) contract see
[contract-opaque.md](contract-opaque.md) and
[contract-transparent.md](contract-transparent.md).

> Single source of truth. This document is the published author contract for
> the vertex hook. The renderer's template substitution
> (`shared_wgsl/vertex/custom_vertex.wgsl`) emits exactly the shape described
> here; the `material-editor` "Vertex (displacement)" pane authors against it.
> If the contract changes, this file changes first and the renderer follows.

---

## What the hook is for

The vertex hook lets a material **move its own vertices** — animated ripples,
height-field displacement, wind sway, inflate/deflate, twist — without a CPU
mesh edit. A non-empty body specializes the geometry (and shadow) raster into a
dedicated pipeline that compiles your displacement; an **empty** body keeps the
material on the shared fast pipeline at **zero cost** (the default).

The hook runs **post-morph / pre-skin in LOCAL (model) space**, and runs
**IDENTICALLY in the geometry pass and the shadow pass** — so a displaced
silhouette casts a matching displaced shadow. (For mask materials the cutout
shadow is alpha-tested on top of the displaced position.)

---

## How your body is injected

Your vertex WGSL is wrapped at template-emission time into:

```wgsl
fn custom_displace_vertex(input: VertexDisplaceInput) -> VertexDisplaceOutput {
    // <your vertex WGSL body, verbatim>
}
```

The wrapper is named `custom_displace_vertex` literally (the per-material
`shader_id` lives in the pipeline cache key, so the function name never
collides across materials). It is called once per vertex, inside
`apply_vertex`, after morph targets are applied and **before** skinning,
instancing, and the model→world transform — so you author entirely in the
mesh's LOCAL frame.

Your body **must end with `return VertexDisplaceOutput(...)`** (or `return o;`
for a `var o: VertexDisplaceOutput;` you fill in). You may declare local helper
functions inside the body but cannot declare new top-level items (structs,
globals) — the substitution wraps the body in a function and naga only permits
item declarations at module scope.

---

## Input — `VertexDisplaceInput`

```wgsl
struct VertexDisplaceInput {
    position: vec3<f32>,      // post-morph LOCAL position
    normal: vec3<f32>,        // post-morph LOCAL normal
    tangent: vec4<f32>,       // LOCAL tangent (w = handedness)
    uv: array<vec2<f32>, 4>,  // ALL of the mesh's UV sets, read per-vertex
    uv_count: u32,            // number of valid UV sets in `uv` (0..=4)
    vertex_index: u32,        // index of this vertex in the mesh
    instance_id: u32,         // u32::MAX (INSTANCE_ATTR_NONE) when non-instanced
    material: MaterialData,   // the SAME auto-generated struct as the fragment hook
    globals: FrameGlobals,    // use input.globals.time for animation
}
```

Field order mirrors the emitted struct exactly (see
`shared_wgsl/vertex/custom_vertex.wgsl::VertexDisplaceInput`).

- **`position` / `normal` / `tangent`** are the post-morph LOCAL surface frame.
  Displace them and return the new frame (see Output below).
- **`uv`** is a 4-element array of **ALL** the mesh's UV sets, read per-vertex —
  full parity with the fragment hook's multi-UV access. `input.uv[0]` is the
  classic TEXCOORD_0; `input.uv[1]` is TEXCOORD_1, etc.; unused sets are
  `(0.0, 0.0)`. **`uv_count`** is the number of valid sets (`0..=4`) — index
  defensively if your material may bind to meshes with fewer sets. The **same
  real per-vertex UVs are read in the geometry, shadow, and transparent passes**
  (opaque/Mask geometry + shadow reconstruct them per-vertex from the merged
  geometry pool by `original_vertex_index`; transparent reads its real per-mesh
  UV attributes), so a UV-driven height field displaces + casts a matching
  shadow on **every** alpha mode. Sample a declared texture with any set via
  `material_sample_<name>(input.material, input.uv[i])`.
- **`vertex_index`** — the mesh vertex index, useful for per-vertex hashes.
- **`instance_id`** — the per-instance id, or `u32::MAX` when the mesh is not
  instanced (compare with `INSTANCE_ATTR_NONE`).
- **`material`** is the **same** auto-generated `MaterialData` struct the
  fragment hook reads — declare uniforms / textures / buffers in your
  `material.json` layout and read them as `input.material.<field>`, sample
  textures via the generated `material_sample_<name>(input.material, uv)`.
- **`globals`** is `FrameGlobals` — use `input.globals.time` (seconds,
  monotonic) for animated displacement.

### `MaterialData`

`MaterialData` is auto-generated from your `material.json` layout exactly as on
the fragment side — see "Per-material data" in
[contract-opaque.md](contract-opaque.md#per-material-data--materialdata). The
wrapper has already loaded `input.material` for you; read fields directly
(`input.material.amplitude`).

### `FrameGlobals`

```wgsl
struct FrameGlobals {
    time: f32,            // seconds since renderer construction (monotonic)
    delta_time: f32,      // seconds since previous render() call
    frame_count: u32,
    resolution: vec2<u32>,
}
```

---

## Output — `VertexDisplaceOutput`

```wgsl
struct VertexDisplaceOutput {
    position: vec3<f32>,   // displaced LOCAL position
    normal: vec3<f32>,     // LOCAL normal (you OWN this — see §6 below)
    tangent: vec4<f32>,    // LOCAL tangent (w = handedness)
}
```

All three are in LOCAL space; the renderer transforms them to world space
downstream (`apply_vertex` does the inverse-transpose normal transform after
your hook returns).

---

## §6 — The hook OWNS the surface frame (normal caveat)

This is the single most important rule of the vertex contract:

> **The hook OWNS the returned normal (and tangent). The renderer does NOT
> recompute the normal from the displaced positions.**

Displacing `position` invalidates the original `normal` — but the renderer
cannot cheaply re-derive a correct normal from neighbouring displaced vertices
in a vertex shader, and **perturbing the normal is itself a primary use case**
(e.g. faking wrinkles). So the contract hands you full ownership:

- If you **only move positions** and want the original lighting frame, you
  **must** pass the normal through: `o.normal = input.normal;` Forgetting this
  leaves `o.normal` undefined (or zero), and the surface will light wrong.
- If you displace along the normal by a smoothly varying amount, the *true*
  normal tilts. For correct shading, **recompute** it analytically — sample the
  displacement at two epsilon-offset neighbours, take the tangent-space deltas,
  and cross them (worked example (b) below).
- The same applies to `tangent`: pass `input.tangent` through unless you have a
  reason to rotate it.

Because the hook runs identically in the geometry and shadow passes, the normal
you return is consistent across both.

---

## Includes in scope — narrower than the fragment

The vertex hook gets a **narrower** include set than the fragment hook:
`math` / `camera` / `textures` / `vertex_color` only (and their transitive
closure — `textures` pulls in `math`). Lighting, IBL, shadows, color-space,
BRDF, and material-color modules are **fragment-only** and are forced off for
the vertex stage — see `ShaderIncludes::for_vertex` in
`packages/crates/materials/src/shader_includes.rs`. (You don't light or shade
in the vertex stage; you move geometry. Declare `textures` if you sample a
height/displacement map.)

---

## Reserved names

Same as the fragment contract — your layout cannot use kernel-provided symbol
names (`material`, `texture_pool`, `extras_pool`, `frame_globals`, `camera`,
`frag`, `vert`). The loader rejects violations with
`MaterialFolderError::ReservedName`.

---

## Example (a) — gentle animated sine ripple along the normal

A position-only displacement: push each vertex along its normal by a small,
time-varying sine of its local position. This is the **starter** the editor's
"Vertex (displacement)" window seeds and `set_material_vertex_wgsl` references.
It works on **any** alpha mode (no UV needed).

```wgsl
// Vertex window for a gentle animated ripple.
//
// Layout (material.json) — optional; constants inlined here for the starter:
//   uniforms: [amplitude: F32 = 0.05, frequency: F32 = 6.0, speed: F32 = 2.0]
//
// Note: this only MOVES vertices. We pass the normal/tangent through unchanged
// (§6 — the hook owns the frame), so lighting uses the original surface frame.
// For a large ripple you'd recompute the normal (see example (b)).

var o: VertexDisplaceOutput;

let amplitude = 0.05;
let frequency = 6.0;
let speed = 2.0;

// Phase from the local position so the wave travels across the surface.
let phase = input.position.x * frequency + input.globals.time * speed;
let offset = sin(phase) * amplitude;

o.position = input.position + input.normal * offset;
o.normal = input.normal;       // §6: pass through (positions moved, frame kept)
o.tangent = input.tangent;
return o;
```

---

## Example (b) — height-field displacement that recomputes the normal

Sample a displacement/height texture at a vertex UV set, push along the normal
by it, **and** recompute the analytic normal from neighbouring height samples.
This is the correct way to keep lighting matching a non-trivial displacement.
Works on **every** alpha mode (opaque, mask, blend) — the geometry, shadow, and
transparent passes all read real per-vertex UVs (see the `uv` field above).

This example uses the **second** UV set (`input.uv[1]`) for the height lookup —
e.g. a dedicated displacement UV channel distinct from the albedo UVs — to show
multi-UV access; `input.uv[0]` works identically for the common single-set case.

### The `recompute_normal_from_height` helper

The renderer provides a shared helper (in scope inside every vertex hook):

```wgsl
fn recompute_normal_from_height(
    n: vec3<f32>,     // incoming LOCAL normal
    t: vec4<f32>,     // incoming LOCAL tangent (w = handedness)
    h_center: f32,    // height at the vertex UV
    h_du: f32,        // height one `eps` step along the tangent (u)
    h_dv: f32,        // height one `eps` step along the bitangent (v)
    eps: f32,         // the UV step used for the two neighbour samples
    strength: f32,    // perturbation scale (0 = unchanged normal)
) -> vec3<f32>        // returns a normalized perturbed normal
```

It builds the bitangent (`cross(n, t.xyz) * t.w`), forms the height slopes
`(h_du - h_center)/eps` and `(h_dv - h_center)/eps`, and tilts the normal away
from the rising direction: `normalize(n - (t.xyz*ddu + bitangent*ddv)*strength)`.

```wgsl
// Vertex window for a height-field displacement (any alpha mode).
//
// Layout (material.json):
//   uniforms: [height_scale: F32 = 0.2]
//   textures: [height]            // a grayscale height map; r channel = height

var o: VertexDisplaceOutput;

let scale = input.material.height_scale;
let eps = 0.01;

// Read the displacement UV set (TEXCOORD_1 here) + two epsilon-offset neighbours.
let uv = input.uv[1];
let h  = material_sample_height(input.material, uv).r;
let hu = material_sample_height(input.material, uv + vec2<f32>(eps, 0.0)).r;
let hv = material_sample_height(input.material, uv + vec2<f32>(0.0, eps)).r;

// Displace this vertex along its normal by the sampled height.
o.position = input.position + normalize(input.normal) * (h * scale);

// Recompute the normal from the neighbouring heights via the shared helper —
// `strength = scale` ties the normal tilt to the displacement magnitude.
o.normal = recompute_normal_from_height(
    input.normal, input.tangent, h, hu, hv, eps, scale,
);
o.tangent = input.tangent;
return o;
```

---

## Pass parity & cost

- **Empty body = no custom vertex = zero cost** (shared fast pipeline). The
  default for every material is empty; declaring a body opts into a dedicated
  geometry + shadow pipeline.
- The body compiles into **both** the geometry raster and the depth-only shadow
  raster (and, for mask materials, the masked variants of each) — one authored
  body, identical displacement everywhere, so shadows track the deformed
  silhouette.
- Runs **post-morph, pre-skin** in LOCAL space, so morphed and skinned meshes
  deform consistently (your displacement composes with morph targets, then
  skinning/instancing/model-transform are applied on top by `apply_vertex`).
