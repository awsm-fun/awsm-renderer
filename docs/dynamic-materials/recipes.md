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

## 4. Scrolling texture (animated UV) — **screen-space / normal-derived**
⚠️ This recipe derives the UV from the surface **normal**, not from the mesh's own
parameterization. It's great for a **glowing panel / forcefield / sky** look where
the texture just needs to drift, but it is **NOT** a conveyor/tread/road scroll: the
UV isn't anchored to the geometry, so it won't read as the surface *travelling*. For
a belt that scrolls **along its own surface**, see **§4b. Geometry-locked scroll**.

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

## 4b. Geometry-locked scroll (conveyor / tread / road)
Make a surface look like it's **travelling along itself** — tank treads, conveyor
belts, flowing roads/rivers. The motion rides the mesh's **own** UV, so it reads as
real travel (unlike §4, which drifts a normal-derived UV).

**Prerequisite — a continuous, tileable strip UV.** A baked atlas UV (each face packed
into its own island) is useless here: scrolling slides every sample off its island
onto unrelated atlas content. The belt needs UVs where **one axis = travel** (V along
the loop, U across the width), paired with a **tileable** texture so the seam at
V=0↔1 is invisible. Authoring that strip UV is the job of `set_vertex_uvs` +
`strip_parameterize` (below) — it's the step that makes scrolling possible at all.

### Clean path (built-in material, no custom WGSL)
1. **Select the belt band.** `select_vertices_where {store:true}` (e.g. an AABB around
   one belt's outer face) → a selection handle.
2. **Parameterize it.** `strip_parameterize { node, selection, axis:[..] }` → per-vertex
   `(along, across)` in `[0,1]`. **Pass the belt's axle explicitly** — auto-fit is
   best-effort and unreliable on near-isotropic bands (see `awsm://docs/mesh-tools`).
   Use `along` for V (travel), `across` for U; scale V by the number of cleats so the
   tile repeats once per grouser.
3. **Write the UVs.** `set_vertex_uvs { mesh, indices, uvs }` (the handle's vertices in
   stored order; read them back with `get_vertex_data { selection }`).
4. **Bind a tileable tile.** `create_texture` a small seamless grouser tile (+ normal
   map), `set_node_texture` it with `wrap_v:"repeat"`.
5. **Scroll V over time.** Either a `texture_transform` **offset** animation track
   (`add_track` target `texture_transform` / prop `offset`, keyframe V 0→1, loop), or
   the auto-scroll `flow` field on `set_node_texture_transform` (monotonic renderer
   time — no clip needed). Reverse via `set_clip_direction` / `set_clip_speed`.

### Fallback (custom WGSL, no UV authoring) — vertex-color scroll coordinate
When you can't author UVs, smuggle the along-belt arc-length through a **vertex-color**
channel and scroll stripes in the shader. Bake `along` into `COLOR_0.r` (across into
`.g`) with `paint_vertex_colors`, assign a custom material (declare
`set_material_includes ["vertex_color"]` so `material_vertex_color` is in scope —
for the clean-path variant that reads the strip UV instead, use `material_uv` with
the `"textures"` include) that reads `material_vertex_color(input, 0u)` +
`frame_globals.time`:
```wgsl
// set_material_includes ["vertex_color"]
let g = frame_globals_from_raw(frame_globals_raw);
let vc = material_vertex_color(input, 0u);       // .r = along-belt arc-length [0,1]
let v = fract(vc.r * CLEATS - g.time * SPEED);   // CLEATS = grousers per loop
let stripe = step(0.5, fract(v));                 // or sample a tileable tile at (vc.g, v)
return OpaqueShadingOutput(mix(DARK, LIGHT, stripe), 1.0);
```
Caveats: `paint_vertex_colors` is terminal (freezes the mesh stack) and replaces the
node's material; you lose any baked PBR look. Prefer the clean path now that
`set_vertex_uvs` exists.

**See also:** `set_vertex_uvs`, `strip_parameterize`, `set_node_texture_transform`,
`set_material_uniform`; `awsm://docs/mesh-tools` (vertex authoring), `awsm://docs/animation`.

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

## 8. Lit (Lambert + Phong) — your own lighting, no PBR
You do **not** need the PBR/BRDF stack to react to the scene's lights. The light
list is **always in scope** (no `set_material_includes` needed): walk it with
`get_lights_info()` / `get_light(i)` and sample each with `light_sample(light,
normal, world_position)`, which returns a shading-model-agnostic
`LightSample { light_dir, radiance, n_dot_l }` (attenuation + spot cone already
applied). Compose any model you like.

fragment_inputs: `["normals","view_dir"]`,
uniforms: `[{name:"albedo",ty:"vec3<f32>",val:"0.8,0.3,0.3"},{name:"shininess",ty:"f32",val:"32.0"}]`.
```wgsl
let n = normalize(input.world_normal);
let v = input.surface_to_camera;            // surface→camera, normalized
let info = get_lights_info();
var lit = vec3<f32>(0.0);
for (var i = 0u; i < info.n_lights; i = i + 1u) {
    let s = light_sample(get_light(i), n, input.world_position);
    let diffuse = s.radiance * s.n_dot_l;                                  // Lambert
    let r = reflect(-s.light_dir, n);
    let spec = pow(max(dot(r, v), 0.0), input.material.shininess) * s.radiance * step(0.0001, s.n_dot_l);  // Phong
    lit += diffuse + spec;
}
let ambient = vec3<f32>(0.03);
let color = input.material.albedo * (lit + ambient);
return OpaqueShadingOutput(color, 1.0);
```
(Iterating `n_lights` is fine for a handful of lights. For hundreds of punctuals
prefer froxel-culled walking — declare `fragment_inputs:["lights"]` and use
`apply_lighting_per_froxel`; see the contract.)

## 9. Advanced: full PBR (GGX + IBL) inside a custom material
For physically-based shading equal to the built-in PBR material, set
`set_material_includes { material, keys:["apply_lighting","brdf","material_color_calc"] }`
(`light_access` is already always present), build a `PbrMaterialColor`, and call
`apply_lighting(material_color, input.surface_to_camera, input.world_position,
get_lights_info(), 1u)`. This pulls in the GGX/Fresnel/IBL math — only worth it
when you need true PBR that the built-in material can't express; otherwise
recipe #8 (or a built-in PBR material) is lighter.
