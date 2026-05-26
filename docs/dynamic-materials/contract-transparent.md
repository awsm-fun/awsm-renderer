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

## Input — `TransparentShadingInput`

```wgsl
struct TransparentShadingInput {
    world_position: vec3<f32>,
    world_normal: vec3<f32>,
    world_tangent: vec4<f32>,         // (xyz: tangent, w: bitangent sign ±1)
    surface_to_camera: vec3<f32>,     // normalized
    front_facing: bool,
    material_offset: u32,             // byte offset for material_load_* calls
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

**What the wrapper does NOT pre-materialize** (vs. an earlier
draft of this contract):

- `uv0` / `uv1` — the wrapper has no UV gradients pre-computed.
  Authors that need UVs reconstruct from the vertex-attribute
  fetch via the per-mesh attribute helpers (see `vertex_attribute`
  in `material_transparent_wgsl/includes.wgsl`).
- `COLOR_0` vertex attribute — same: fetch it explicitly if the
  mesh has one; the wrapper trades the cost-per-pixel of always
  loading optional attributes for keeping the common case cheap.
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
  `shared_wgsl/lighting/lights.wgsl` only when the mesh's
  `material_mesh_meta.receive_shadows` bit is set.
- **No SSCS** (screen-space contact shadows). The transparent pass
  shadow include compiles SSCS to `return 1.0;` — sampling its own
  depth target would be a feedback loop.
- **No `frame_globals` mesh-light slice path** — the per-mesh light list
  is opaque-only. Transparents iterate the full punctual-light set via
  `get_lights_info()`.
- **`opaque_background` is bound on the transparent pass** so you can
  sample the pre-blit opaque render target for refraction / transmission.
  PBR's `sample_transmission_background` helper is the prior art —
  reuse it via `let bg = sample_transmission_background(uv, ...);`.

---

## Sorting

Transparent meshes are sorted back-to-front by the existing transparent
pass machinery (the same path PBR transmission + Unlit blend draw
through). Custom transparents participate on equal footing — there is no
per-shader-id sort order; only per-mesh world-space depth.

---

## Alpha mode

`alpha_mode = Blend` → this contract.

`alpha_mode = Mask { cutoff }` still routes through the opaque compute
kernel (the alpha-mask discard is set by your fragment's
`output.alpha = 0.0`) — see [opaque contract](contract-opaque.md).

`alpha_mode = Opaque` → opaque kernel.

Custom materials cannot override the alpha-mode-driven routing
(`is_transparency_pass()` derives from `alpha_mode` directly).

---

## Reserved names

Same list as opaque — see
[contract-opaque.md § Reserved names](contract-opaque.md#reserved-names).

---

## Example — soft-glass (Phase 7)

```wgsl
// shader.wgsl for the soft-glass material
//
// Layout (material.json):
//   uniforms: [tint: Color3 = [0.85, 0.92, 1.0],
//              refraction_strength: F32 = 0.05]
//   textures: []
//   buffers:  []
//
// alpha_mode: Blend, double_sided: true

let camera = camera_from_raw(camera_raw);

// Refract the opaque background sample by perturbing the screen-space
// lookup along the surface normal.
let normal_screen = (camera.view * vec4<f32>(input.world_normal, 0.0)).xy;
let offset = normal_screen * input.material.refraction_strength;
let bg_uv = vec2<f32>(0.5, 0.5) + offset;
let bg_rgb = textureSampleLevel(input.opaque_background, input.opaque_sampler, bg_uv, 0.0).rgb;

// Schlick-ish view-angle alpha (more opaque at grazing angles).
let cos_theta = clamp(dot(input.world_normal, input.surface_to_camera), 0.0, 1.0);
let alpha = mix(0.85, 0.25, cos_theta);

let color = bg_rgb * input.material.tint;
return TransparentShadingOutput(vec4<f32>(color, alpha));
```

(Phase 7 of the dynamic-materials plan lands this exact material as the
first end-to-end transparent test case.)
