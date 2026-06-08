# Custom-Material Recipes (Cookbook)

Copy-paste WGSL + the MCP calls to register each. These are **custom
(dynamic-WGSL) materials** — best for *effects* (unlit/emissive/procedural/
animated/stylized). Read [`contract-opaque.md`](contract-opaque.md) (and
[`contract-transparent.md`](contract-transparent.md)) for the full ABI; this is
the quick-reference.

> **For a normal lit, shaded surface, do NOT write a custom material** — use a
> built-in PBR material instead:
> `add_builtin_material { "shading": "pbr" }` then `set_builtin_param { node,
> param: "base_color" | "metallic" | "roughness" | "emissive", value }`.
> Custom materials return color *directly* (linear HDR, unlit unless you
> hand-roll lighting), so they're for looks the PBR material can't express.

## How to register any recipe

Each recipe = a layout (uniform/texture slots) + a WGSL body + the fragment
inputs it reads. The general flow:

```jsonc
add_custom_material                         // → <mat>
set_material_layout { "material": <mat>,
   "uniforms": [{ "name":"tint","ty":"vec3<f32>","val":"1.0,0.6,0.2" }],
   "textures": [{ "name":"tex","ty":"texture_2d<f32>" }] }      // omit if none
set_material_fragment_inputs { "material": <mat>, "keys": ["normals","view_dir"] }
set_material_wgsl  { "material": <mat>, "wgsl": "<body below>" }
get_material_diagnostics { "asset": <mat> }    // expect ok:true
assign_material    { "node": <node>, "material": <mat> }
set_material_texture { "node": <node>, "slot": "tex", "texture": <texId> }  // if textured
```

Notes that apply to every recipe:
- A custom **opaque** body must end with `return OpaqueShadingOutput(color, 1.0);`
  (`color` is linear HDR). A **transparent** body ends with
  `return TransparentShadingOutput(vec4<f32>(color, alpha));`.
- `input.material.<name>` reads your declared uniforms/slots.
- Time: `let g = frame_globals_from_raw(frame_globals_raw); let t = g.time;`
- Read uniforms you declared; read only the fragment inputs you declared in
  `keys` (valid: `normals, tangents, uv, view_dir, lights, vertex_color`).
- Texture **UV**: simplest is screen-space
  `vec2<f32>(input.coords) / vec2<f32>(input.screen_dims)`, or a normal-derived
  UV (varies over curved surfaces). For true mesh UVs declare `keys:["uv"]` and
  use `texture_uv(...)` (see the contract).

---

## 1. Solid / emissive color
Layout: `uniforms: [{name:"color",ty:"vec3<f32>",val:"0.9,0.2,0.4"}]`,
fragment_inputs: `["view_dir"]` (any one input is fine).
```wgsl
let color = input.material.color;     // linear HDR; >1.0 reads as "glowing" after bloom/tonemap
return OpaqueShadingOutput(color, 1.0);
```

## 2. Fresnel rim (view-angle glow)
fragment_inputs: `["normals","view_dir"]`. uniforms:
`[{name:"rim",ty:"vec3<f32>",val:"0.6,0.7,1.0"},{name:"base",ty:"vec3<f32>",val:"0.05,0.06,0.1"}]`.
```wgsl
let n = normalize(input.world_normal);
let v = input.surface_to_camera;                       // normalized, surface→camera
let f = pow(1.0 - max(dot(n, v), 0.0), 3.0);
let color = input.material.base + input.material.rim * f;
return OpaqueShadingOutput(color, 1.0);
```

## 3. Textured (unlit)
Layout: `textures: [{name:"tex",ty:"texture_2d<f32>"}]`,
fragment_inputs: `["normals"]`. Bind a texture with `set_material_texture`.
```wgsl
// Normal-derived UV: varies across curved surfaces (great on a sphere).
let n = normalize(input.world_normal);
let uv = vec2<f32>(n.x * 0.5 + 0.5, n.y * 0.5 + 0.5);
let color = material_sample_tex(input.material, uv).rgb;
return OpaqueShadingOutput(color, 1.0);
```
`material_sample_<slot>(input.material, uv)` is the generated sampler — no
offset math, correct sampler, returns black for an unbound slot.

## 4. Scrolling texture (animated UV)
Layout: `textures:[{name:"tex",ty:"texture_2d<f32>"}]`,
`uniforms:[{name:"speed",ty:"vec2<f32>",val:"0.1,0.0"}]`, fragment_inputs `["normals"]`.
```wgsl
let g = frame_globals_from_raw(frame_globals_raw);
let n = normalize(input.world_normal);
let uv0 = vec2<f32>(n.x * 0.5 + 0.5, n.y * 0.5 + 0.5);
let uv = fract(uv0 + input.material.speed * g.time);
let color = material_sample_tex(input.material, uv).rgb;
return OpaqueShadingOutput(color, 1.0);
```

## 5. Pulsing emissive (time)
fragment_inputs `["view_dir"]`, uniforms `[{name:"color",ty:"vec3<f32>",val:"0.2,0.8,1.0"}]`.
```wgsl
let g = frame_globals_from_raw(frame_globals_raw);
let pulse = 0.5 + 0.5 * sin(g.time * 3.0);
return OpaqueShadingOutput(input.material.color * pulse, 1.0);
```
For a deterministic screenshot of a specific phase, `set_frame_time { seconds }`
before capturing, then `clear_frame_time`.

## 6. Procedural checker (no texture)
fragment_inputs `["normals"]`, uniforms `[{name:"scale",ty:"f32",val:"8.0"}]`.
```wgsl
let n = normalize(input.world_normal);
let uv = vec2<f32>(n.x * 0.5 + 0.5, n.y * 0.5 + 0.5) * input.material.scale;
let c = (i32(floor(uv.x)) + i32(floor(uv.y))) & 1;
let color = mix(vec3<f32>(0.05), vec3<f32>(0.9), f32(c));
return OpaqueShadingOutput(color, 1.0);
```

## 7. Glass / Fresnel-alpha (transparent)
**Transparent** material — set `set_material_alpha_mode { material, mode:"blend" }`
*and* author against the transparent contract (returns `TransparentShadingOutput`).
fragment_inputs `["normals","view_dir"]`,
uniforms `[{name:"tint",ty:"vec3<f32>",val:"0.7,0.85,1.0"}]`.
```wgsl
let n = normalize(input.world_normal);
let v = input.surface_to_camera;
let fresnel = pow(1.0 - max(dot(n, v), 0.0), 3.0);
let alpha = mix(0.15, 0.9, fresnel);     // edges more opaque
return TransparentShadingOutput(vec4<f32>(input.material.tint, alpha));
```

## 8. Advanced: a PBR-lit custom material
If you need real punctual + IBL lighting inside a custom material (rather than a
built-in PBR material), set `set_material_includes { material, keys:
["apply_lighting","brdf","light_access","material_color_calc"] }` and
`fragment_inputs:["normals","view_dir","uv","tangents"]`, build a
`PbrMaterialColor`, and call
`apply_lighting(material_color, input.surface_to_camera, input.world_position,
lights_info, 1u)`. This is non-trivial — see `contract-opaque.md` ("Helpers in
scope" → lighting) and the renderer's `lighting/apply_lighting.wgsl`. For most
lit surfaces the built-in PBR material is the right tool.
