# Transparent dynamic-material WGSL contract

This is the load-bearing surface for authoring a runtime-registered
**transparent** custom material (`alpha_mode = Blend`). For opaque
materials see [contract-opaque.md](contract-opaque.md).

> Single source of truth. This document — together with
> [contract-opaque.md](contract-opaque.md) — is the published author
> contract. The renderer's template substitution emits exactly the shape
> described here; the `material-editor` "Contract" pane renders this file
> inline when `alpha_mode == Blend`. If the contract changes, this file
> changes first and the renderer follows.

---

## How your fragment is injected

Your `shader.wgsl` is wrapped at template-emission time into:

```wgsl
fn custom_shade_<ID>(input: TransparentShadingInput) -> TransparentShadingOutput {
    // <your shader.wgsl body, verbatim>
}
```

The transparent fragment shader (which runs per mesh, per pipeline,
back-to-front sorted by the existing transparent pass) dispatches
`custom_shade_<ID>(input)` from its `@fragment` entrypoint and writes
the result as the fragment color.

Same module-scope rules as opaque (see opaque contract): top-level
items must be declared above the function body; the function ends with
`return TransparentShadingOutput(...)`.

---

## Specialization & the bucket cap

Transparent materials specialize the same way opaque ones do — each
registered custom material compiles its **own** pipeline, gated at compile
time to exactly its feature-set (there is no shared "uber" transparent
fragment). First-party transparent PBR likewise specializes per
feature-set. Each transparent material is one bucket and counts against the
same `MAX_BUCKET_ENTRIES` cap; overflow is the same hard error. See
[contract-opaque.md § Specialization & the bucket cap](contract-opaque.md#specialization--the-bucket-cap).

---

## Input — `TransparentShadingInput`

```wgsl
struct TransparentShadingInput {
    world_position: vec3<f32>,
    world_normal: vec3<f32>,
    world_tangent: vec4<f32>,         // (xyz: tangent, w: bitangent sign ±1)
    surface_to_camera: vec3<f32>,     // normalized
    front_facing: bool,
    material_offset: u32,             // byte offset for material_load_* calls
    // Forwarded interpolated attribute sets (one field per set the mesh carries):
    color_0: vec4<f32>, color_1: vec4<f32>, …   // COLOR_n  (read via material_vertex_color)
    uv_0: vec2<f32>,    uv_1: vec2<f32>,    …   // TEXCOORD_n (read via material_uv)
    material: MaterialData,           // your auto-generated struct (see opaque docs)
}
```

Field order mirrors the emitted struct exactly (see
`material_transparent_wgsl/fragment.wgsl::TransparentShadingInput`).

`world_tangent` is a `vec4`: xyz is the tangent direction in world
space, w is the bitangent sign (`±1`); reconstruct the bitangent via
`cross(world_normal, world_tangent.xyz) * world_tangent.w`.

`MaterialData` is auto-generated from your `material.json` layout — see
[contract-opaque.md § Per-material data](contract-opaque.md#per-material-data--materialdata)
for the field order + alignment rules. The shape is identical across both
alpha modes.

**Per-vertex UVs + colours.** Any `TEXCOORD_n` / `COLOR_n` the mesh carries is read
with the same accessors as the opaque path. Each is emitted only when you declare
its **shader-include** (`set_material_includes`) — `material_uv` needs `"textures"`,
`material_vertex_color` needs `"vertex_color"`. ⚠️ These are includes, NOT
`fragment_inputs` — declaring `fragment_inputs:["uv"]` does not bring `material_uv`
into scope (it only affects the vertex-attribute layout).

```wgsl
// set_material_includes ["textures", "vertex_color"]
let uv1 = material_uv(input, 1u);            // interpolated TEXCOORD_1  (needs "textures")
let c0  = material_vertex_color(input, 0u);  // interpolated COLOR_0     (needs "vertex_color")
```

A set the mesh lacks returns a benign default (`vec2(0)` / `vec4(1)`) — there is
no presence guard on the custom path, so author against a mesh that has the set.

**What the wrapper does NOT pre-materialize:**

- UV *gradients* (ddx/ddy) — not pre-computed; `material_uv` returns the
  interpolated coordinate, not derivatives.
- `opaque_background` texture + sampler — bound on the transparent
  pass globally (not on the wrapper struct). Authors sample it via
  `sample_transmission_background(uv, ...)` (see Helpers in
  scope below) — PBR's transmission code is the prior art.

---

## Output — `TransparentShadingOutput`

```wgsl
struct TransparentShadingOutput {
    color: vec4<f32>,    // (rgb, alpha) — pre-multiplied? see below
}
```

The transparent pipeline uses the standard `(src_alpha, one_minus_src_alpha)`
blend equation. Return **non-premultiplied** color: the fragment is blended
as `dst = src.rgb * src.a + dst * (1 - src.a)`.

---

## Helpers in scope

Same surface as opaque (see
[contract-opaque.md § Helpers in scope](contract-opaque.md#helpers-in-scope))
with these differences:

- **Shadow sampling is available**, but receive-shadow gating is the
  caller's responsibility — call `apply_lighting(...)` from
  `shared_wgsl/lighting/apply_lighting.wgsl` only when the mesh's
  `material_mesh_meta.receive_shadows` bit is set.
- **No SSCS** (screen-space contact shadows). The transparent pass
  shadow include compiles SSCS to `return 1.0;` — sampling its own
  depth target would be a feedback loop.
- **No `frame_globals` mesh-light slice path** — the per-mesh light list
  is opaque-only. Transparents iterate the full punctual-light set via
  `get_lights_info()`.
- **`opaque_background` is bound on the transparent pass** (the pre-blit
  opaque render target). Note that the PBR `sample_transmission_background`
  helper needs `frag_pos` + the camera struct, which `TransparentShadingInput`
  does not expose — so screen-space refraction isn't readily available to
  custom transparents (promote to first-party PBR for true transmission;
  see the example below).

---

## Sorting

Transparent meshes are sorted back-to-front by the existing transparent
pass machinery (the same path PBR transmission + Unlit blend draw
through). Custom transparents participate on equal footing — there is no
per-shader-id sort order; only per-mesh world-space depth.

---

## Alpha mode

`alpha_mode = Blend` → this contract (transparent fragment pass).

`alpha_mode = Mask { cutoff }` → **NOT this contract** — masked custom
materials are alpha-tested **opaque**: the main fragment shades in the
opaque compute path (`OpaqueShadingInput` / `OpaqueShadingOutput`, see the
[opaque contract](contract-opaque.md)) and the cutout itself is a
**second, alpha-only WGSL window** (`set_custom_material_alpha_wgsl`,
wrapped into `fn custom_alpha_dynamic(input: MaskAlphaInput) -> f32`)
compiled into the masked visibility raster — so cut fragments never enter
the depth/visibility buffer at all, and the mask gets the optimized opaque
shading path instead of transparent sorting. A Mask registration with an
EMPTY alpha window has no cutout variant to raster through; the editor
bridge and the player scene-loader both downgrade that combination to
plain Opaque with a warning (it used to render silent black). Cut-out
leaves / grates / etched panels → Mask + alpha window. Genuinely
see-through surfaces (soft glass, fades) → Blend, this contract.

`alpha_mode = Opaque` → [opaque contract](contract-opaque.md).

Custom materials cannot override the alpha-mode-driven routing
(`Material::is_transparency_pass` derives from the registration's
`alpha_mode` directly).

---

## Reserved names

Same list as opaque — see
[contract-opaque.md § Reserved names](contract-opaque.md#reserved-names).

---

## Example — soft-glass

```wgsl
// shader.wgsl for the soft-glass material
//
// Layout (material.json):
//   uniforms: [tint: Color3 = [0.85, 0.92, 1.0],
//              edge_alpha: F32 = 0.85,
//              face_alpha: F32 = 0.25]
//   textures: []
//   buffers:  []
//
// alpha_mode: Blend, double_sided: true

// Linear view-angle alpha — more opaque at grazing angles
// (edges of curved surfaces), more transparent face-on. A straight
// `mix` on `cos_theta`, not a Schlick `pow(1 - cos_theta, 5)` term. The output
// alpha drives the standard (src.a, 1-src.a) blend the transparent
// pass uses; the kernel composites the dst (opaque background)
// behind us automatically, so the dynamic shader doesn't have to
// sample it itself.
let cos_theta = clamp(dot(input.world_normal, input.surface_to_camera), 0.0, 1.0);
let alpha = mix(input.material.edge_alpha, input.material.face_alpha, cos_theta);

let color = input.material.tint;
return TransparentShadingOutput(vec4<f32>(color, alpha));
```

**Why this example doesn't sample the opaque background directly:**
the `opaque_background` target is bound on the transparent pass, but the
renderer's `sample_transmission_background(...)` helper needs `frag_pos`
+ the camera struct, which `TransparentShadingInput` deliberately does not
expose (the per-pixel surface is kept minimal to avoid per-fragment
materialization cost). So custom transparents can do standard alpha
blending against the framebuffer (what this example does), but true
refractive sampling (glass, dispersion) should **promote to first-party
PBR with `KHR_materials_transmission`** rather than work around the
minimal dynamic surface.
