# Plan: upstream-improvements — make built-in materials richer, custom shading first-class, the animation editor usable

> **Status: ACTIONABLE — drive start-to-finish with one loop.** (2026-06-21)
>
> This is the executable rewrite of a third-party consumer's handoff report. **Every original ask was
> re-verified against the live code on branch `improvements`** — and the report turned out to be
> significantly stale: several items are already built, one "bug" appears already fixed, and a couple of
> "papercuts" no longer reproduce in the code. The verified current state is recorded per task below
> (with `file:line` evidence) so the loop doesn't rebuild what exists. The original report text is
> preserved verbatim in the **Appendix** for traceability.
>
> **This doc is the SSOT.** Update the STATUS LOG at the bottom after each task (verified PASS/CHANGED,
> with the evidence). Do NOT leave a broken/half-done state committed — if a task can't land GREEN, revert
> to the last working state and record why.

## Guiding principle (from the report — unchanged, still correct)

Most friction has one root cause: **users get pushed into dynamic (custom WGSL) materials to do things the
built-in materials should support directly** — and once there, pushed further into re-deriving lighting,
the wrong layer. So:

1. **Add the missing knobs to the built-in materials** (animatable UV transforms, more animatable params,
   texture flow). "PBR, but the texture scrolls" should be a built-in parameter, **not** a shader.
2. **Dynamic materials are for genuinely custom shading**, and must **never** require re-implementing PBR.
   Where a dynamic material needs a general building block the engine already has internally (IBL sampling,
   normal-map TBN), **expose it behind the existing `includes` gate** — small, opt-in, composable. Never
   hand people a blank shader and make them rebuild the lighting model.

---

## How to drive + verify this loop (MANDATORY — chrome-devtools live)

Every task is verified **in the running browser**, not just by `cargo build`. The editor dev server runs
on **http://localhost:9085** (trunk; auto-rebuilds wasm on save). The editor exposes a wasm command/query
seam on `window.wasmBindings` — drive it with chrome-devtools `evaluate_script`:

```js
// WRITE: dispatch an EditorCommand (serde-tagged JSON, same shape as the MCP EditorCommand enum)
window.wasmBindings.editor_dispatch_json(JSON.stringify({ /* { "<Variant>": { ...fields } } */ }));

// READ: run an EditorQuery and get JSON back (snapshot, diagnostics, value/pixel readback)
await window.wasmBindings.editor_query_json(JSON.stringify({ /* EditorQuery */ }));
await window.wasmBindings.editor_snapshot_json();          // whole-editor snapshot (mode/tree/selection/animation)
window.wasmBindings.editor_query_mode();                   // "scene" | "animation"
await window.wasmBindings.editor_query_scene_png();        // base64 viewport PNG (visual proof)
await window.wasmBindings.editor_query_material_png(id);   // material thumbnail PNG
await window.wasmBindings.editor_tick_animation(dt_ms);    // advance the anim clock (player-path tick)
```

**Verification protocol per task:** (1) reload `http://localhost:9085/` after the wasm rebuilds; (2) build
a minimal repro scene via `editor_dispatch_json`; (3) exercise the new behavior; (4) confirm with
`editor_query_json` readback **and** a `editor_query_scene_png` screenshot (use chrome-devtools
`take_screenshot` / read the PNG); (5) check `list_console_messages` for **zero** `GPUValidationError`
(the only benign warning is "Unable to preventDefault inside passive event listener"). Record the result
in the STATUS LOG.

> Save→reload via the OS File-System-Access dialog can't be driven by chrome-devtools; exercise the
> equivalent code path through `editor_dispatch_json` (e.g. the `Replace` node-sync arm) instead, and note
> the substitution.

---

## Task order (dependency-sequenced)

`T0` re-audited the stale claims **(DONE — see STATUS LOG)** and re-scoped what follows. Order now:
**`D2-fix` first** — T0 confirmed it's a live black-screen bug + lying diagnostics, the single
highest-impact item. Then `A1` (vec2/vec4 tracks) unblocks animating the `B1` UV offset; `B1`
settable-transform unblocks `B2`/`B3`; `D1`/`D3` are independent; `U2` is the last real UX gap.
P1, U1, U3 were **closed by T0** (not reproducible / already built).

**Order:** `T0` ✅ → `D2-fix` → `A1` → `A2` → `B1` → `B2` → `B3` → `D1` → `D3` → `P2` → `U2`.
(`P2` — "frame node inside subject" — was not exercised in T0; verify it live in its own iteration.)

---

### T0 — Re-audit the stale claims LIVE ✅ DONE (2026-06-21)

Verified live in the browser via the wasm seams. Results (full evidence in STATUS LOG):

- **D2(a) (codegen "black screen" bug) — CONFIRMED REAL, NOT fixed.** A static read was misleading: the
  struct generator emits `_pad_N: u32` members (`dynamic_layout.rs` `generate_wgsl_struct` L254-279), but
  the loader/constructor (`generate_wgsl_loader` `emit` closure L388-460) only *advances* `byte_offset`
  past the pad (L389) — it emits **no constructor argument** for the pad fields. So `[a: f32, b: vec2<f32>]`
  → struct has 3 members (`a`, `_pad_0`, `b`), constructor passes 2. **Reproduced live verbatim:**
  `GPUValidationError: structure constructor has too few inputs: expected 3, found 2 … CreateShaderModule
  "Material Opaque"` → box renders **black**, "0 buckets". The `vec3_padding_against_following_field` test
  (~L897) only checks the byte **packer**, not that the generated WGSL struct+constructor field-counts
  match — so it never caught this. → **fix in D2-fix.**
- **D2(b) (diagnostics lie) — CONFIRMED REAL.** With the broken material assigned and the GPU error
  spewing each frame, `editor_query_json { material_diagnostics }` still returned
  `{ registered:true, ok:true, errors:[] }` (`query.rs` `CompileDiagnostics` L85-91). The async GPU
  pipeline-creation outcome is not reflected. → **fix in D2-fix.**
- **P1 (camera locked in clip mode) — CLOSED (not reproducible).** With a clip made current
  (`set_current_clip`), `set_camera_orbit` from pose A→B visibly moved the view (3/4 → top-down, zoomed
  out, screenshot-confirmed). Camera control is not gated by clip mode. No action.
- **U1 / U3 (add-track affordance) — CLOSED (already built).** In animation mode the "Add Track" button is
  prominent in the top bar **and** as the empty-state CTA ("Add a track to bind a bone, morph weight, or
  material uniform"); `animation_mode/add_track.rs` covers Transform/Light/Camera/BuiltinParam/Morph/Uniform.
  Only residual: **morph index >0** is capped at 0 (`add_track.rs` L18-21) — folded into **U2**.
- **U2 (outliner in animation) — CONFIRMED STILL MISSING.** The animation-mode left rail shows only the
  clip list ("Animations"), no scene-tree. Remains a real task below.

---

### A1 — Add `vec2` / `vec4` keyframe + uniform-track value kinds

**Verified state — STILL-VALID.** `TrackValue` is `Vec3 | Quat | Scalar`
(`packages/crates/scene/src/animation.rs` L58-65). Uniforms already support `Vec2`/`Vec4`
(`scene/src/dynamic_material.rs` `UniformValue` L152-180), but the animation→uniform conversion rejects
anything but F32/Vec3/Quat (`packages/crates/renderer/src/animation/animations.rs` L238-254, `WrongKind`).

**Do.** Add `Vec2([f32;2])` and `Vec4([f32;4])` to `TrackValue`; thread them through: the curves/sampler
interpolation (`packages/crates/curves/`), the animation→uniform conversion map (`animations.rs` L238),
the MCP `TrackValue` mirror + (de)serialization (`packages/mcp/editor-protocol/src/command.rs`), and the
editor inspector/keyframe widgets. Componentwise lerp for Vec2/Vec4; cubic if the sampler supports it.

**Verify (live).** Create a clip, add a uniform/material track, `AddKeyframe` a `Vec2` and a `Vec4` value,
`editor_tick_animation` across the keys, and `editor_query_json` the interpolated value at a mid-time —
confirm it matches componentwise lerp. Screenshot a scene whose visible param is a Vec2/Vec4 track.

**Done when:** a single Vec2 track (not two scalars) drives a uniform end-to-end, verified live; no GPU error.

---

### A2 — Accept an optional `interp` on `add_keyframe`

**Verified state — STILL-VALID but tiny.** `AddKeyframe` has no `interp`
(`packages/mcp/editor-protocol/src/command.rs` ~L674); the handler derives interp from the track sampler
(`controller/state.rs` `sampler_to_interp`). `SetKeyframe` **already** carries `interp: Option<Interp>`
(command.rs ~L698), so setting it today needs a second call.

**Do.** Add `#[serde(default)] interp: Option<Interp>` to `AddKeyframe`; in the handler, use it when
`Some`, else fall back to the sampler default. Reuse the existing `Interp` type — no new plumbing.

**Verify (live).** `AddKeyframe` with `interp: "step"` via `editor_dispatch_json`, then `editor_query_json`
the keyframe and confirm its interp is `step` without a follow-up `SetKeyframe`.

**Done when:** one `AddKeyframe` call sets a non-default interp, verified by readback.

---

### B1 — Per-texture UV transform: make it settable AND animatable on built-in materials

**Verified state — PARTIAL (infra already exists; only the runtime/anim/UI surface is missing).** The
data model + GPU plumbing are done: `TextureTransform { offset, scale, rotation, origin }`
(`packages/crates/renderer/src/textures.rs` ~L960), per-texture `MaterialTexture::transform_key`
(`packages/crates/materials/src/texture.rs` L18), and `KHR_texture_transform` round-trips on glTF import
across all slots (`packages/crates/renderer-gltf/src/populate/material.rs` L784-809). **What's missing:**
(a) no runtime API to *set* a transform after material creation (it's load-time-only / read-only); (b) no
animation-track target for it; (c) no editor UI to edit it on a built-in material.

> Distinct from the existing per-texture **UV-set selector + wrap mode** in the material assign UI — that
> picks *which* UV channel and wrap, it is **not** an offset/scale/rotation transform. Don't conflate them.

**Do.** (a) Add a settable path — extend `AwsmRenderer::update_material` use / a `MaterialTexture`
transform setter so a transform can be written live (repacks the material uniform buffer on next prep);
expose an MCP/editor command (mirror the `SetMaterialTexture` shape, add transform fields). (b) Add a
UV-transform animation target (new `BuiltinParamKind`/`BuiltinMaterialParam` arm, or a dedicated
texture-transform target) carrying the offset `Vec2` (uses **A1**) + scale `Vec2` + rotation scalar,
**per texture slot**. (c) Add the editor UI (offset/scale/rotation fields per texture slot on built-in
materials), and wire it as an Add-Track target in `animation_mode/add_track.rs`.

**Verify (live).** Built-in PBR mesh with a base-color texture: set a non-zero `offset` via dispatch →
screenshot shows the texture shifted. Animate the offset as a single Vec2 track → `editor_tick_animation`
→ screenshots over time show it scrolling. Confirm the normal-map slot's transform is independent
(per-texture, not shared). No GPU error.

**Done when:** a built-in texture's UV offset is both live-settable and animatable per slot, proven by
before/after + over-time screenshots.

---

### B2 — Broaden the animatable/settable built-in material params

**Verified state — STILL-VALID.** Only `BaseColor | Metallic | Roughness | Emissive` are animatable, in
two enums + one resolver that must stay in sync: `BuiltinParamKind`
(`packages/crates/scene/src/animation.rs` L86-94), `BuiltinMaterialParam`
(`packages/crates/renderer/src/animation/clip_group.rs` L84-96), and `builtin_param()`
(`packages/crates/scene-loader/src/animation.rs` L261-268).

**Do.** Treat "settable param" and "animatable track target" as the **same list**. Add the natural knobs:
`normal_scale`, `emissive_strength`, `occlusion_strength`, alpha cutoff; toon ramp knobs (diffuse bands,
specular steps, shininess, rim strength/power — see `materials/src/toon.rs`); flipbook `fps` / `time_offset`
/ `frame_count` (`materials/src/flipbook.rs` L78-99). Extend both enums + the resolver together, the MCP
`SetBuiltinParam`/readback, and the Add-Track BuiltinParam list (`add_track.rs` L510-558).

**Verify (live).** For each new param: set it via dispatch → screenshot reflects the change; animate it →
ticked screenshots show it moving. Spot-check a toon material and a flipbook material specifically.

**Done when:** the new params are settable + animatable, each verified live; no GPU error.

---

### B3 — First-class texture `flow` (direction + speed), advanced automatically

**Verified state — STILL-VALID (absent).** No `flow`/`scroll` anywhere; flipbook uses the global
`frame_globals.time` for frame selection, not per-material UV velocity.

**Do.** A thin convenience over **B1**: a per-texture-slot `flow` param (direction `vec2` + speed) that
the runtime advances each frame by accumulating into the slot's UV offset (reuse B1's transform — flow is
just an auto-driver of `offset`). Expose from the param API + GUI. Keep it optional; B1 is load-bearing.

> **Surface the content caveat in tooling:** UV-scroll only works when the mesh has a continuous UV axis
> along the scroll direction. Baked/tiled geometry (e.g. a tank tread of separate cleat-links sharing one
> atlas patch) has no such axis — scrolling walks off into unrelated atlas regions. Add an editor
> **detect-and-warn** ("this mesh has no continuous UV parameterization along U/V") so users don't chase an
> effect the geometry can't support.

**Verify (live).** Set `flow` on a textured plane → without any manual keyframes, ticked screenshots show
the texture moving at the set speed/direction. Confirm two slots can flow at different speeds.

**Done when:** flow auto-scrolls a built-in texture with no animation track, verified over time; the
no-continuous-UV warning fires on a baked-UV mesh.

---

### D1 — Expose `ibl` and `normal_map`/TBN building blocks behind `includes` gates (biggest item)

**Verified state — STILL-VALID.** The `includes` gate exists with a `KEY_TABLE`
(`packages/crates/materials/src/shader_includes.rs` L165-238); dynamic materials get the Tier-A set
(math, camera, color_space, textures, vertex_color, light_access, shadows, skybox, extras —
`materials/src/dynamic.rs` L183-189). `light_access` is **punctual-only** (`get_lights_info` / `get_light`
/ `light_sample` — `renderer/src/render_passes/shared/shared_wgsl/lighting/light_access.wgsl` L18-126), so
in an IBL-lit scene with no punctual lights a dynamic material renders ~black while built-in PBR beside it
is lit. The BRDF/IBL **primitives exist** but only inside the Tier-B aggregate (`brdf.wgsl`,
`apply_lighting`, `material_color_calc` are marked `tier_a: false`) — not split into a gatable include.
`tangents` is available as a fragment input but **no TBN matrix / `perturb_normal` helper** is exposed.

**Do.**
- **`ibl` include** — split the IBL primitives (diffuse irradiance + specular prefilter + BRDF-LUT lookup)
  out of the Tier-B BRDF aggregate into a small Tier-A include exposing `sample_ibl(normal, roughness)`
  (and the pieces). Add the key to `KEY_TABLE`, gate its cost so it's only paid when requested. This is a
  general primitive, **not** a PBR re-implementation (explicitly do NOT add a "reimplement/fork PBR"
  helper).
- **`normal_map` / TBN include** — supply a TBN when `tangents` is requested, and/or a
  `perturb_normal(sample)` helper. Note the opaque compute kernel samples LOD0 with no hardware
  derivatives, so the TBN must come from the requested `tangents` input, not `dpdx/dpdy`.

**Verify (live).** Build an IBL-only scene (environment, **no** punctual lights). Add a built-in PBR mesh
(lit) and a dynamic material that opts into `ibl` and calls `sample_ibl` — screenshot: the dynamic mesh is
lit comparably, **not black**. Then a dynamic material opting into `normal_map`/TBN with a normal texture —
screenshot shows correct tangent-space perturbation. No GPU error.

**Done when:** both includes work from a custom material in a real IBL scene, screenshot-proven, without
re-deriving PBR.

---

### D2-fix — Fix the padding-codegen "black screen" bug AND make diagnostics reflect the real GPU outcome

**Verified state — BOTH halves CONFIRMED REAL (T0, live).** This is the highest-impact item: any
custom-material uniform layout that needs alignment padding (e.g. `f32` before `vec2`/`vec3`/`vec4`,
`vec2` before `vec4`) generates a `MaterialData` struct with `_pad_N` members but a constructor that omits
them → naga rejects the whole "Material Opaque" module → **every mesh on that kernel renders black**, and
material diagnostics falsely report `ok:true`.

**Do — (a) codegen.** In `generate_wgsl_loader` (`packages/crates/materials/src/dynamic_layout.rs` L367-501),
emit a constructor argument for **every** struct member, including the pad fields. The struct generator
(`generate_wgsl_struct` L254-279) emits one `_pad_N: u32` per 4-byte gap; the loader's `emit` closure must
mirror that — when it advances `byte_offset` for alignment (L389), emit a literal `0u` for each skipped pad
word **before** the real field's value, so the constructor's positional argument list matches the struct's
member list exactly. (Alternative: drop the named pad members from the struct and rely on `@align`/`@size`
attributes — but the matching-args approach is the smaller, more local change and keeps the struct
self-describing.) Walk the same gap arithmetic in both functions so they can't drift again.

**Do — (b) regression test.** Add a `naga_validate`-backed test (the harness in
`packages/crates/renderer/src/wgsl_validation.rs` parses+validates the assembled kernel natively, no GPU)
that builds a `MaterialLayout` for `[a: f32, b: vec2<f32>]` (and `[f32, vec3]`, `[f32, vec4]`,
`[vec2, vec4]`), generates struct+loader, assembles the opaque kernel, and asserts naga accepts it. This
test FAILS today (reproduces "too few inputs") and passes after (a).

**Do — (c) diagnostics.** Make `CompileDiagnostics` (`packages/mcp/editor-protocol/src/query.rs` L85-91)
and the `SetCustomMaterialWgsl` result reflect the real `CreateShaderModule`/pipeline-creation outcome, not
just the pre-wrap WGSL parse — surface the GPU validation error in `errors` and flip `ok:false`. The GPU
compile is async/deferred; wire the deferred compile-status back into the diagnostics the
`material_diagnostics` query reads (see `renderer.rs` `dynamic_material_compile_status` ~L2363 and the
compile scheduler). A pre-check via the `naga_validate` path can also catch the padding class synchronously
and report it author-relative.

**Verify (live).** Re-run the T0 repro: a `[f32, vec2<f32>]` custom material assigned to a box now renders
the shaded color (NOT black), "buckets" > 0, **zero** `GPUValidationError` in the console. Then register a
*deliberately* GPU-invalid material → `material_diagnostics` now reports `ok:false` with the real error; a
valid one reports `ok:true`.

**Done when:** the padding layout renders correctly (screenshot, no GPU error), the new `naga_validate`
test passes, and diagnostics no longer lie about a material that fails GPU pipeline creation.

---

### D3 — Setting a material uniform must affect the LIVE value, not only the default

**Verified state — STILL-VALID at the editor/MCP layer.** `SetMaterialUniform` is documented as setting
the **default** value of a declared slot (`packages/mcp/editor-protocol/src/command.rs` L558-561), so a
write only takes effect on re-register or via an animation track. The renderer *has* a live path
(`AwsmRenderer::update_material` callback repacks the uniform buffer each prep —
`packages/crates/renderer/src/materials.rs` L70-83), but the editor command writes the default and
re-registers rather than writing the live buffer.

**Do.** Route `SetMaterialUniform` through the live `update_material` path so the value writes the live
uniform buffer and shows immediately (no re-register). If both semantics are wanted, keep "default" and
add an explicit live variant — but the report's ask is that the existing setter previews live; prefer
making it live and documenting it.

**Verify (live).** Register a custom material with a color uniform, assign to a mesh, `SetMaterialUniform`
a new color → screenshot updates **without** re-register; `editor_query_json` Uniform readback matches.

**Done when:** a uniform write changes the render immediately, screenshot-proven.

---

### P1 — Camera control / gizmo in clip-edit mode  ✅ CLOSED by T0 (not reproducible)

T0 confirmed camera orbit works while a clip is current (pose A→B moved the view, screenshot-proven), so
the reported lock does not reproduce on the current build. No action. (If a user re-reports it, capture the
exact gesture — it may have been an interactive mouse-drag capture issue in an older build, not the orbit
command path.)

---

### P2 — "Frame node" can place the camera inside the subject

**Verified state — PARTIAL.** The fit uses `frame_aabb(aabb, 1.15×)` (`controller/state.rs` FrameNode
handler) with breathing room but **no min-distance clamp**; the fit math lives in the external
`awsm_web_shared` camera lib (`FreeCamera::frame_aabb`), so a large/odd-aspect subject can still seat the
camera inside the bounds.

**Do.** Add a min-distance clamp (and/or revisit the fit so distance derives from the bounding sphere +
vertical FOV with a floor that keeps the camera outside the AABB). If `frame_aabb` is in the external lib,
apply the clamp at the editor call site or extend the lib call.

**Verify (live).** `FrameNode` on a large mesh → screenshot shows a proper fit (whole subject visible),
not an interior close-up. Repeat on a small mesh (still framed reasonably).

**Done when:** framing a large subject never lands inside it, screenshot-proven on big + small meshes.

---

### U2 — Bring an outliner / scene-tree into the animation context (shared selection)

**Verified state — MISSING (the one real UX gap left).** A full outliner exists in scene mode
(`packages/frontend/editor/src/scene_mode/outliner.rs`) but the animation-mode workspace left column shows
only ClipLibrary + KeyInspector (`animation_mode/workspace.rs` L17-33) — no scene-tree, no visible
selection context, so you can't see what a track binds to. (U1/U3 are already addressed — see T0.)

**Do.** Surface a collapsible outliner / scene-tree in the animation workspace, reusing the scene-mode
outliner component. Make selection **visible and shared** between scene and animation editing, and wire it
to drive the selection-aware Add-Track flow (pick node → choose property → add track). Optionally lift the
morph index>0 cap if the mesh's morph-target count can be exposed to the editor `Node` (`add_track.rs`
L18-21).

**Verify (live).** In animation mode: the outliner is visible; selecting a node there highlights it and
the Add-Track picker is scoped to it; selecting a track shows which node it binds to. Confirm selection
matches `editor_snapshot_json`'s `selection`.

**Done when:** animation mode shows a working, selection-shared outliner, verified via snapshot + UI.

---

## STATUS LOG (append after each task — this is the loop's running record)

> Format: `YYYY-MM-DD — <task> — PASS/CHANGED/CLOSED — <one-line live evidence (screenshot/readback + no GPU error)>`

- 2026-06-21 — plan rewritten from the stale third-party handoff; all items re-verified against branch
  `improvements` code; live-drive harness (`window.wasmBindings.editor_*`) confirmed working on :9085.
- 2026-06-21 — **T0 re-audit DONE (live).** Built Box+DirLight via `editor_dispatch_json`; results:
  - **D2(a) CONFIRMED REAL** — custom material layout `[a: f32, b: vec2<f32>]` assigned to the box →
    `GPUValidationError: structure constructor has too few inputs: expected 3, found 2 … CreateShaderModule
    "Material Opaque"` (console), box rendered **black**, "0 buckets". Root cause read in
    `dynamic_layout.rs`: `generate_wgsl_struct` emits `_pad_N` members but `generate_wgsl_loader` omits the
    matching constructor args. Not covered by existing packer tests.
  - **D2(b) CONFIRMED REAL** — `material_diagnostics` returned `{registered:true, ok:true, errors:[]}` for
    that same broken material while the GPU error spewed each frame. Diagnostics lie.
  - **P1 CLOSED** — with a clip current, `set_camera_orbit` A→B visibly moved the viewport (screenshots).
    Not reproducible.
  - **U1/U3 CLOSED** — "Add Track" affordance prominent in animation mode (top bar + empty-state CTA),
    `add_track.rs` covers all target families. Residual morph-index>0 cap folded into U2.
  - **U2 STILL MISSING** — animation-mode left rail is clip-list only; no scene-tree outliner.
  - Re-scoped: D2-fix promoted to first real task; P1/U1/U3 closed. Next: D2-fix.

---

## Appendix — original third-party handoff (verbatim, for traceability)

> The text below is the report as received. It is **superseded** by the verified tasks above where they
> disagree; kept only so each ask is traceable to its origin. Item codes (B1–B3, A1–A2, D1–D3, P1–P2,
> U1–U3) map 1:1 to the tasks above.

### 1 — Built-in material capabilities
- **B1.** Animatable per-texture UV transform (offset/scale/rotation) on built-in materials (PBR, unlit,
  toon, flipbook); settable param **and** animation-track target; aligns with glTF `KHR_texture_transform`.
- **B2.** Broaden animatable built-in params beyond `base_color | metallic | roughness | emissive`
  (normal_scale, emissive_strength, occlusion, alpha cutoff, UV transform, toon ramp, flipbook fps/offset).
- **B3.** First-class texture flow/scroll (direction + speed) advanced automatically; thin convenience over
  B1. Content caveat: needs a continuous UV axis; editor should detect-and-warn on baked/tiled geometry.

### 2 — Animation system
- **A1.** Add `vec2` / `vec4` keyframe + uniform-track value kinds (today only `vec3 | quat | scalar`).
- **A2.** Accept an optional `interp` on `add_keyframe` (today keys default; setting interp needs a 2nd call).

### 3 — Dynamic-material ergonomics
- **D1.** Expose general lighting building blocks behind `includes`: `ibl` (sample_ibl: diffuse irradiance
  + specular prefilter + BRDF LUT) and `normal_map`/TBN (supply TBN when `tangents` requested /
  `perturb_normal`). Explicitly NOT a "reimplement/fork PBR" helper.
- **D2.** BUG: malformed `MaterialData` constructor when a scalar precedes a `vec2` (alignment padding in
  struct but not constructor → whole Material-Opaque module fails → meshes render black); AND diagnostics
  lie (WGSL-set returned ok / diagnostics `{registered:true, ok:true, errors:[]}` despite a GPU
  `CreateShaderModule` validation error). Fix padding codegen; make diagnostics reflect the actual GPU
  pipeline-creation outcome.
- **D3.** Setting a material uniform affects only the DEFAULT, not the live value (only takes effect on
  re-register or via an animation track). Write the live uniform buffer (or add an explicit `*_live`
  variant and document the distinction).

### 4 — Editor / runtime papercuts
- **P1.** Camera control appears locked while a clip is "current"; gizmo persists after clearing selection;
  clearing the current clip restores camera control. Allow camera framing in clip-edit mode (or document);
  let "clear selection" remove the gizmo.
- **P2.** "Frame node" can place the camera inside the subject (extreme interior close-up). Revisit the fit
  math / expose a min-distance.

### 5 — Editor UX: animation editor
- **U1.** Overall: the GUI clip-authoring flow is not discoverable/usable ("right now it's unusable").
- **U2.** No visible selection context while animating — bring an outliner / scene-tree into the animation
  context (collapsible); selection visible and shared between scene and animation editing.
- **U3.** No visible way to add tracks — an obvious "add track" affordance covering material params,
  node/mesh transforms, lights, cameras, morphs, custom-material uniforms, driven off the current selection.
