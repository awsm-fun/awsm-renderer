# awsm-renderer / awsm-scene — Improvement Handoff

Issues and proposed improvements for the awsm-renderer / awsm-scene editor,
with enough repro/context to act on.

## Guiding principle

Most of the friction below comes from one root cause: **users are pushed into
dynamic (custom WGSL) materials to do things the built-in materials should
support directly** — and once there, they're pushed further into
re-deriving lighting, which is the wrong layer to work at.

So the asks follow two rules:

1. **Add the missing knobs to the built-in materials** (animatable UV
   transforms, more animatable params, texture flow). A request like "PBR, but
   the texture scrolls" should be answered by a built-in parameter, **not** by
   writing a shader.
2. **Dynamic materials are for genuinely custom shading**, and they must
   **never** require re-implementing or forking PBR. Where a dynamic material
   needs a general building block the engine already has internally (IBL
   sampling, normal-map TBN, …), **expose it behind the existing `includes`
   gate** — small, opt-in, composable. Don't hand people a blank shader and
   make them rebuild the lighting model.

All of the items below need to be implemented — this isn't a prioritized
shortlist, it's the full set.

---

## 1 — Built-in material capabilities (so a dynamic material isn't needed)

### B1. Animatable UV transform (offset / scale / rotation) on built-in materials
**Symptom:** built-in materials expose no UV transform at all, animatable or
otherwise. This is **not PBR-specific** — it applies to every built-in that
samples textures (PBR, unlit, toon, and the flipbook atlas mapping).
**Impact:** any "moving texture" effect — scrolling treads/conveyors/belts,
flowing water/lava, drifting clouds, sliding UI/decals, parallax — is
impossible on the built-in path and forces a custom material.
**Suggestion:** add a **per-texture** `uv_transform` (offset `vec2`, scale
`vec2`, optional rotation) — one transform per texture reference/slot, not a
single shared per-material one. Per-texture is both more flexible (scroll the
base color while the normal map stays put; run two layers at different speeds)
and aligns with glTF's `KHR_texture_transform`, so it maps cleanly onto the
import path. Expose each transform both as a settable param **and** as an
animation-track target. With this, the entire "scroll a texture" class of
effects is a built-in, riggable feature.

### B2. Broaden the set of animatable built-in params
**Symptom:** animation tracks can drive only `base_color | metallic |
roughness | emissive` on built-ins.
**Impact:** can't animate `normal_scale`, `emissive_strength`, `occlusion`,
alpha cutoff, the UV transform from B1, toon ramp knobs, flipbook fps/offset,
etc. — all of which are natural things to keyframe.
**Suggestion:** make the built-in material params uniformly settable **and**
animatable (treat "settable param" and "animatable track target" as the same
list), rather than a hand-picked subset.

### B3. (Convenience) first-class texture "flow / scroll"
**Symptom:** even with B1, the common case ("this belt's texture should move at
speed v") is a manual offset-vs-time setup.
**Impact:** small, but it's such a common need (treads, conveyors, water) that
a dedicated affordance pays off.
**Suggestion:** a thin convenience over B1 — a `flow` param (direction +
speed) that the runtime advances automatically — usable from both the GUI and
the param API. Optional; B1 is the load-bearing part.

> **Content caveat worth surfacing in tooling (relevant to B1/B3):** UV-scroll
> only works when a mesh has a continuous UV axis along the scroll direction.
> Baked/tiled geometry (e.g. a tank tread where each cleat-link is separate
> geometry mapping to ~the same atlas patch, with scattered per-vertex UVs) has
> no such axis — scrolling shifts texture per-link and walks off into unrelated
> atlas regions. The editor could **detect and warn** ("this mesh has no
> continuous UV parameterization along U/V") so users don't chase an effect the
> geometry can't support, and reach for a geometry/path-based solution instead.

---

## 2 — Animation system

### A1. Keyframe / uniform-track value kinds are only `vec3 | quat | scalar`
**Symptom:** no `vec2` or `vec4`, so multi-component values must be decomposed
into separate scalar tracks.
**Impact:** e.g. the B1 UV offset (`vec2`) can't be a single track; a `vec4`
tint/rect can't either.
**Suggestion:** add `vec2` and `vec4` value kinds to keyframes and uniform
tracks.

### A2. `add_keyframe` can't set interpolation
**Symptom:** keys are created with a default interp; setting linear/step/cubic
needs a second patch call.
**Suggestion:** accept an optional `interp` on `add_keyframe`.

---

## 3 — Dynamic-material ergonomics (for genuinely custom shading)

> Framing: these make the *custom-shading* path pleasant **without** asking
> anyone to re-implement PBR. The recurring need is small, general building
> blocks exposed behind `includes` — the same opt-in mechanism dynamic
> materials already use for `textures`, `light_access`, etc.

### D1. Expose general lighting building blocks behind `includes` gates
The engine already computes these internally; dynamic materials just can't
reach them. Each should be an opt-in include, costed only when used:

- **`ibl` — environment irradiance / prefiltered radiance sampling.** Today the
  generic light API iterates **punctual lights only**, so in an IBL-lit scene
  with no punctual lights a dynamic material renders ~black while built-in PBR
  meshes beside it are lit correctly. A gated `sample_ibl(normal, roughness)`
  (diffuse irradiance + specular prefilter + the BRDF LUT lookup) is the single
  biggest "make custom materials first-class in real scenes" win — and it's a
  general primitive, not a PBR re-implementation.
- **`normal_map` / TBN — tangent-space normal mapping support.** The opaque
  compute kernel samples LOD0 with **no hardware derivatives**, and the shading
  wrapper provides **no TBN**, so applying a normal map means requesting the
  `tangents` fragment input and reconstructing the TBN by hand (undocumented).
  Expose a gated helper (supply the TBN when `tangents` is requested, and/or a
  `perturb_normal(sample)` function).

Both are reusable across *any* custom material (overlays, stylized shading,
splatting), which is exactly why they belong as gated includes rather than
copy-pasted shader math.

> Explicitly **not** suggested: a "reimplement PBR" helper or a "fork the
> built-in PBR into an editable custom material" path. Those were workarounds
> for the gaps above; the fix is B1–B2 (so you stay on built-in PBR) plus these
> small gated primitives (for real custom work) — not making PBR re-derivation
> easier.

### D2. **BUG:** malformed `MaterialData` constructor when a scalar precedes a `vec2` — and diagnostics report success
**Repro:** dynamic-material layout `uniforms: [a: f32, b: vec2<f32>]`.
The generated struct inserts alignment padding (so `vec2` lands on an 8-byte
boundary) → **11 fields**, but the generated constructor passes only **10**:
```
error: structure constructor has too few inputs: expected 11, found 10
    return MaterialData( material_load_f32(base + 0u),               // a
                         vec2<f32>(material_load_f32(base+2u), …),   // b
                         … )
```
**Impact (two distinct bugs):**
  a. **Codegen:** the whole "Material Opaque" GPU module fails to create, so
     *every* mesh on that kernel renders **black**. Any `f32`-before-`vec2`
     layout reproduces it (alignment padding emitted in the struct but not in
     the constructor).
  b. **Diagnostics lie:** the failure produced **no signal** on the API
     surface — the WGSL-set call returned `ok`, and material diagnostics
     reported `{ registered:true, ok:true, errors:[] }`. The real error was
     only in the renderer's `tracing` console as a GPU `CreateShaderModule`
     validation error.
**Suggestion:** fix the padding codegen (emit the pad field, or omit it from
the struct); and make material diagnostics / the WGSL-set result reflect the
**actual GPU pipeline-creation outcome**, not just the pre-wrap WGSL parse.

### D3. Setting a material uniform affects only the DEFAULT, not the live value
**Symptom:** writing a custom-material uniform doesn't change the render; it
only takes effect on re-register or via an animation track, despite an API
name/description that imply a live write.
**Impact:** no way to preview a uniform value without animating it.
**Suggestion:** write the live uniform buffer (or add an explicit `*_live`
variant and document the distinction).

---

## 4 — Editor / runtime papercuts

### P1. Camera control appears locked while a clip is "current"
After making a clip current, camera-orbit calls return `ok` but the viewport
doesn't move, and a transform gizmo persists even after clearing the selection;
clearing the current clip restores camera control. *Suggestion:* allow camera
framing in clip-edit mode (or document the lock); let "clear selection" remove
the gizmo there.

### P2. "Frame node" can place the camera inside the subject
Fitting a large subject can end up as an extreme interior close-up rather than a
fit. *Suggestion:* revisit the fit math / expose a min-distance.

---

## 5 — Editor UX: animation editor

> Headline from a human user: **"right now it's unusable."** Likely needs
> back-and-forth UI design discussion.

### U1. Overall: the animation editor needs a usability pass
The end-to-end flow of authoring a clip in the GUI is not discoverable or
usable. U2–U3 are concrete symptoms.

### U2. No visible selection context while animating
You can't see which node/mesh is selected, so you can't tell what a track will
bind to. **Ask:** bring an **outliner / scene-tree into the animation context**
(collapsible). Selection should be visible and shared between scene and
animation editing.

### U3. No visible way to add tracks
It's unclear how to add an animation track from the UI at all. **Ask:** an
obvious "add track" affordance covering the full target range — **material
parameters, node/mesh transforms, lights, cameras, morphs, custom-material
uniforms** — ideally driven off the current selection (pick a selected
node/material → choose a property → add the track).
