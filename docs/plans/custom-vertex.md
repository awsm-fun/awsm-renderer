# Custom vertex shaders — design & plan

> **Goal.** A custom (dynamic-WGSL) material can control its **vertices**, the same
> way it already controls **masking and color** in the fragment stage. The agent
> writes a small WGSL body; the renderer wraps it as a template hook, gates it
> into the rasterizing passes, and gives that material its **own pipeline**.
> *Straightforward in shape, not small in surface* (renderer + editor + MCP).
>
> Status: **design doc / not started.** This is the deferred "part 2" of §16
> (mcp-improvements) — explicitly NOT blocking the current MCP PR. The
> displace-from-texture data hook already shipped (`7bbca00a`); this is the
> programmable-WGSL version.

---

## 1. The key architectural fact (don't skip this)

The opaque material pass is **deferred**: a **compute shader** (`cs_opaque`)
shades from a visibility buffer. **It has no vertex stage.** Vertices are
rasterized earlier, by the **geometry (visibility) pass**, which writes
triangle-id + barycentric + world-normal into the visibility texture that the
compute kernel reads.

So the custom *fragment* hook (`custom_shade_dynamic`) lives in the deferred
**compute** kernel — but a custom *vertex* hook must live in the **raster**
passes that actually run a `@vertex` stage. There are five, and **every one
shares the same transform function** `apply_vertex()`:

| Pass | File (vertex) | Role | Must match? |
|---|---|---|---|
| **Geometry / visibility** | `render_passes/geometry/shader/geometry_wgsl/vertex.wgsl` | rasterize → visibility buffer (drives the deferred opaque shade) | **yes — this is the one** |
| **Geometry masked** | `render_passes/geometry/shader/masked_template.rs` (reuses geometry vertex) | alpha-tested cutout into the visibility buffer | yes |
| **Transparent** | `render_passes/material_transparent/shader/material_transparent_wgsl/vertex.wgsl` | forward-shaded blend | yes |
| **Shadow** | `shadows/shader/shadow_wgsl/vertex.wgsl` | depth-only into shadow maps | yes |
| **Shadow masked** | `shadows/shader/shadow_masked_wgsl/vertex.wgsl` | alpha-tested depth | yes |

All five call **`render_passes/shared/shared_wgsl/vertex/apply_vertex.wgsl`**,
which runs the canonical chain: **morph → skin → model/instance transform →
world → clip**, plus the inverse-transpose normal/tangent transform and the
billboard override.

**Correctness invariant (the whole reason this is subtle):** if a vertex is
displaced in one pass but not another, the silhouette, depth, shadows, and
masked cutout stop matching the shaded surface. **The displacement must run
identically in all five passes.** Injecting it into the single shared
`apply_vertex()` is what makes that automatic — every pass that compiles the
custom-vertex variant of `apply_vertex` gets the same displacement for free.

Your instinct was right on both counts: *"make sense in the visibility shader as
a template"* → the geometry pass + `apply_vertex`; *"custom materials get their
own pipeline since that's going to include/gate"* → see §3.

---

## 2. The WGSL hook & contract (mirror the fragment machine)

### 2a. Where it injects

Add one gated hook to `apply_vertex.wgsl`, **after morph, before skin** — so the
agent always works in a **consistent post-morph LOCAL frame** (skinned and
non-skinned alike), and skinning / instancing / the model→world transform then
deform the displaced mesh exactly as they would the base. (Injecting *after* skin
would hand the agent world-space positions for skinned meshes but local for
rigid ones — inconsistent; rejected. A post-skin/world-space variant is a later
opt-in flag, not v1.)

```wgsl
// shared_wgsl/vertex/apply_vertex.wgsl  (inside fn apply_vertex)
//   ... morph targets applied (local) ...
{% if has_custom_vertex %}
    let _d = custom_displace_vertex(VertexDisplaceInput(
        vertex.position, normal, tangent, uv0, vertex.vertex_index, instance_id, material
        {% if inc.camera %}, frame_globals {% endif %}
    ));
    vertex.position = _d.position;
    normal  = _d.normal;
    tangent = _d.tangent;
{% endif %}
//   ... skinning (deforms the displaced local frame) ...
//   ... model/instance transform → world → clip (inverse-transpose on _d.normal) ...
```

The wrapper that holds the agent's body (mirrors `custom_shade_dynamic`), emitted
into each rasterizing template under `{% if has_custom_vertex %}`:

```wgsl
struct VertexDisplaceInput {
    position: vec3<f32>,   // post-morph LOCAL position
    normal:   vec3<f32>,   // post-morph LOCAL normal
    tangent:  vec4<f32>,   // LOCAL tangent (w = handedness)
    uv:       vec2<f32>,   // uv0 (gate more sets behind an include if needed)
    vertex_index: u32,
    instance_id:  u32,     // for per-instance displacement (sentinel if non-instanced)
    material: MaterialData,// the SAME auto-generated struct as the fragment hook
    {% if inc.camera %} globals: FrameGlobals, {% endif %} // time, camera
};
struct VertexDisplaceOutput {
    position: vec3<f32>,   // displaced LOCAL position
    normal:   vec3<f32>,   // the shader's LOCAL normal (REQUIRED — see §6)
    tangent:  vec4<f32>,   // the shader's LOCAL tangent (passthrough if unchanged)
};
fn custom_displace_vertex(input: VertexDisplaceInput) -> VertexDisplaceOutput {
{{ dynamic_wgsl_vertex|safe }}
}
```

### 2b. The contract (what the agent gets / returns)

- **In:** local `position`, `normal`, `tangent`, `uv`, `vertex_index`,
  `instance_id`, the material's declared `material.*` uniforms/textures/buffers
  (so it can sample a heightmap or read a `time`/`amplitude` uniform), and —
  behind the `camera` include — `globals` (time, camera) for animated displacement.
- **Out:** displaced local `position`, `normal`, and `tangent` — the shader owns
  the whole surface frame (§6). Passthrough is the explicit "I didn't change it"
  (return the input value).
- **Available helpers:** the same auto-generated `MaterialData` struct +
  `material_data_load()` the fragment hook uses (`materials/src/dynamic_layout.rs`)
  — identical byte layout, so the vertex stage and fragment stage read the same
  uniform buffer. **Vertex texture fetch** (sampling a heightmap in the vertex
  stage) is allowed by WebGPU; it requires the material texture pool to be bound
  in the geometry pass for custom-vertex draws (see §3).

### 2c. Includes / gating

Reuse `ShaderIncludes` (`materials/src/shader_includes.rs`), but the vertex stage
wants a **narrower** set than the fragment (no lighting/IBL/shadows — those are
fragment concerns). Two options:
- **(recommended)** a `for_vertex(includes)` mask (sibling of `for_custom`) that
  forces off everything except `math` / `camera` / `textures` / `vertex_color`.
- a separate `vertex_shader_includes` list on the material def.

---

## 3. Pipelines: the per-material split (the real lift)

Today the **geometry pass uses one pipeline for all opaque meshes** — it's
material-agnostic (writes triangle-id/normal, doesn't care what shades later).
That's why it's cheap. Custom vertex displacement breaks that: a mesh whose
material displaces vertices needs its **own** geometry-pass vertex pipeline
(compiled with that material's WGSL). This is the "own pipeline" you predicted.

Concretely:
- The geometry / masked / transparent / shadow / shadow-masked **cache keys** gain
  `dynamic_vertex_shader: Option<DynamicVertexShaderInfo>` (mirror of
  `DynamicShaderInfo`: `{ struct_decl, loader_decl, wgsl_vertex, includes }`).
  `None` → the existing shared fast pipeline (zero cost for everyone else).
- The geometry-pass **draw loop** must bucket meshes by vertex-shader id: meshes
  with no custom vertex go through the one shared pipeline (as today); each
  custom-vertex material gets its own pipeline + draw. The dispatch/bucketing
  machinery already exists for the fragment side (`bucket_entries`,
  `dispatch_hash`); extend it to the vertex axis.
- Custom-vertex geometry draws must additionally **bind the material's uniform +
  texture-pool bind groups** (the shared geometry pipeline doesn't today), so the
  vertex stage can read uniforms / sample heightmaps. These bind groups are
  already global (the deferred pass uses them) — it's wiring, not new resources.
- **Shadows:** the same custom-vertex variant must compile for the shadow +
  shadow-masked pipelines, or shadows detach from the displaced silhouette.

This is the bulk of the renderer work. Risk is concentrated in the geometry-pass
draw loop and the shadow loop (per-material pipeline selection where there was
one). Budget accordingly.

---

## 4. Registration & cache key (mechanical mirror)

Mirror exactly what `alpha_wgsl` (the 2nd, mask-only window) does end-to-end:

- `MaterialRegistration` (`dynamic_materials/registry.rs`) gains
  `wgsl_vertex: Option<String>` (alongside `wgsl_fragment`, `alpha_wgsl`).
- `build_registration` (`engine/bridge/dynamic.rs`) reads the new editor window,
  and **folds `wgsl_vertex` into `wgsl_hash`** so an edit recompiles. Idempotent
  registration is keyed on `(name, layout_hash, wgsl_hash)` — unchanged shape.
- The cache-key `DynamicShaderInfo` already carries `struct_decl` / `loader_decl`
  (the auto-generated `MaterialData` + loader from `dynamic_layout.rs`) — reuse
  them verbatim for the vertex hook (same uniform layout). Add `wgsl_vertex`.
- `dispatch_hash` already invalidates pipelines when the registry changes — the
  vertex WGSL rides in `wgsl_hash`, so no new invalidation channel is needed.

---

## 5. Validation (mirror `validate_dynamic_material_wgsl`)

`renderer.rs::validate_dynamic_material_wgsl` assembles the opaque (or transparent)
template with the agent's fragment + runs naga. Add the sibling: assemble the
**geometry-pass** template with the custom-vertex hook + run naga, returning
`line: None`-style errors to the editor (the existing fragment validator already
omits naga line numbers because they index the assembled module — keep that).
This catches the agent's vertex-WGSL errors at edit time, before a GPU compile.

> NB §20 lesson: the editor's lightweight `controller::custom_material::compile_wgsl`
> pre-check (the line-by-line "missing `;`" heuristic) runs on *every* WGSL window
> before naga — make sure the vertex window routes through the continuation-aware
> version, not a fresh copy of the old heuristic.

---

## 6. Normals are the shader's responsibility (RESOLVED)

**Decision: the hook returns the surface frame (`normal` required, `tangent`
passthrough-or-recomputed). The renderer does NOT recompute normals.**

Rationale: displacing positions invalidates normals, and **perturbing the normal
is itself a primary use case** — a custom vertex shader may want to displace the
normal directly (detail/wobble/anisotropy) with or without moving the position.
A renderer-side derivative or neighbor recompute would *fight* the shader (it'd
overwrite the shader's intended normal) and is strictly less expressive. So the
contract makes the frame the shader's job, exactly like color/alpha are the
fragment shader's job.

Mechanics: the agent's returned local `normal` flows through the existing
inverse-transpose transform in `apply_vertex` and is written to the visibility
buffer (and interpolated in the transparent pass) with **zero new machinery** —
the geometry vertex shader already transforms + emits whatever normal
`apply_vertex` produces. Passthrough (`return input.normal`) is the explicit
"unchanged."

Authoring help (docs/examples, not renderer logic): for height-field
displacement the analytic normal is a few lines — sample the height at two
epsilon-offset UVs and cross the tangent deltas, or return the closed-form
gradient. The contract doc ships a worked example so authors don't have to
rediscover it. (A convenience `recompute_normal_from_height(...)` WGSL helper
could live behind an include later, but it's a helper the shader *calls*, not
something the renderer imposes.)

---

## 7. Editor changes (mirror the alpha_wgsl window)

`CustomMaterial` already has two WGSL windows (`wgsl`, `alpha_wgsl`). Add a third,
`vertex_wgsl`, plumbed identically:
- A **toggle** "custom vertex" on the material (so non-vertex materials keep the
  shared fast geometry pipeline — the default must be OFF).
- The Material-mode Studio gets a vertex WGSL editor window when the toggle is on.
- `vertex_wgsl` feeds `build_registration` + the `wgsl_hash` + the `compile_wgsl`
  pre-check + `validate_dynamic_material_wgsl` (vertex variant) for live diagnostics.
- A starter body that renders non-trivially out of the box (e.g. a gentle sine
  ripple along the normal using `globals.time`), like the default fragment body.

---

## 8. MCP changes

- `set_material_vertex_wgsl { material, wgsl }` — the typed setter (mirror
  `set_material_wgsl` / `set_material_alpha_wgsl`).
- `get_material_contract` gains a `vertex: true` mode returning the vertex ABI
  (the `VertexDisplaceInput`/`Output` structs + the include list), mirroring the
  existing `transparent: true` contract switch.
- A new `docs/dynamic-materials/contract-vertex.md` describing the ABI + the
  normal caveat + a worked example (heightmap displacement + analytic normal).

---

## 9. Phasing

1. **Core raster path.** ABI + `apply_vertex` hook + geometry & shadow per-material
   vertex pipelines + registration/cache-key + naga validation. Verify: a custom
   vertex material ripples a plane AND its shadow + silhouette match (no detached
   shadow). This is the correctness baseline.
2. **Cover the rest of the passes.** Transparent + geometry-masked + shadow-masked
   custom-vertex variants. Verify a *transparent* + *masked* custom-vertex mesh.
3. **Authoring surface.** Editor window + toggle + MCP setter + contract doc +
   starter body + live diagnostics.
4. **Polish (optional).** Derivative normal recompute (§6.2); a vertex-stage
   heightmap-sampling worked example; skinned-mesh interaction tests.

Phases 1–2 are the renderer lift; 3 is the mechanical mirror of `alpha_wgsl`; 4 is
opt-in quality.

---

## 10. Performance

- **Zero cost for materials that don't use it.** The hook is gated
  (`has_custom_vertex` defaults off); every existing mesh keeps the single shared
  geometry/shadow pipeline and the current batched draw. This must be guarded by a
  test/benchmark — the whole point is that the common path is untouched.
- **Cost is per-custom-vertex-material, and opt-in.** Each such material adds a
  geometry-pass pipeline + its own draw bucket (breaking the one-pipeline batch for
  *those* meshes only) + binding the material uniform/texture groups in the
  geometry + shadow passes. For a handful of custom-vertex materials this is
  negligible; a scene that makes *every* material custom-vertex pays the
  visibility pass becoming material-bucketed (closer to a traditional forward
  vertex cost). That's inherent and acceptable — it's the agent's choice.
- **Vertex texture fetch** (heightmap sampling in the vertex stage) is a real but
  agent-chosen cost; no different from any engine's vertex displacement.
- Compile cost: one extra pipeline per custom-vertex material per pass it appears
  in (geometry/shadow/transparent/masked), compiled once on registration (the
  `wgsl_hash`/`dispatch_hash` cache already dedups).

---

## 11. Resolved decisions

- **Normal/tangent ownership** → the shader returns the surface frame; the
  renderer never recomputes (§6).
- **Injection order** → after morph, **before skin**, in the post-morph LOCAL
  frame, consistently for skinned + rigid meshes (§2a). A post-skin/world-space
  variant is a later opt-in flag, not v1.
- **Skinned + custom-vertex** → resolved by the order above: displacement is in
  rest-pose local space and gets skinned along (a displaced character's detail
  follows the skin — the intuitive default).
- **Instanced + custom-vertex** → `instance_id` is in `VertexDisplaceInput`, so an
  author can drive per-instance displacement (index a declared buffer by it). The
  model/instance transform still applies after the hook, unchanged.
- **Tangents** → carried in the output frame (passthrough by default; the shader
  recomputes them when it does normal-mapped detail). Resolved as part of §6.
- **MSAA / masked edges** → orthogonal: the vertex hook only changes position/
  frame; the masked fragment's sample-mask AA path is untouched. Preserve it
  as-is in the masked custom-vertex variant.

## 12. Remaining implementation risk (not a design question)

The one genuinely hard part is mechanical, not a decision: the **geometry-pass +
shadow-pass draw loops** go from one shared pipeline to per-vertex-material
pipeline selection + binding the material groups for custom-vertex draws. Scope
this first (Phase 1) and gate the success criterion on **shadows + silhouette
matching the displaced surface** — that's the proof the five passes stayed in
sync.
