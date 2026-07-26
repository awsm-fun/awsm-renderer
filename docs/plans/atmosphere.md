# Atmospheric haze — design

Status: design (2026-07-13), extracted from the jetpack-arena work per David:
"atmospheric haze is a completely separate feature". The arena currently
FAKES both halves by baking a haze hemisphere into its probe cubemap
(gen-assets.py rev f) — that is scene set-dressing standing in for a
renderer feature, and it only covers the reflection path, not direct view.

## What the arena taught us

A scene lit by emissives with EMPTY overhead reads wrong in two ways:
1. **Direct**: distant geometry and the void behind it are pitch black —
   no aerial perspective, no sense of air.
2. **Reflections**: steep reflection rays that point at nothing return
   black, so glossy floors die to black in the near field while blazing in
   the far field — a brightness cliff that sweeps with the camera.

Both are the same missing phenomenon: light scattered by the air along the
view/reflection path.

## Feature shape

`PostProcess.atmosphere` (persisted like bloom):

```
atmosphere: {
  enabled: bool,           // structural (compiles the fog term)
  color: [f32; 3],         // linear radiance of fully-saturated haze
  density: f32,            // extinction per meter (1/e distance = 1/density)
  base_height: f32,        // world y where density is full
  height_falloff: f32,     // exponential thinning above base_height (0 = uniform)
}
```

### Phase 1 — view-path fog (the classic)

In the EFFECTS pass (it already binds depth for DoF), before bloom:

```
t   = exp(-density * dist(depth))          // height-integrated when falloff > 0
rgb = rgb * t + color * (1 - t)
```

- Sky pixels use a large fixed distance → the skybox blends toward the haze.
- Structural `atmosphere` axis on the effects cache key (zero cost when off);
  color/density/heights are live uniforms.
- Analytic height integration (exponential medium along a ray) is closed-form
  — no marching.

### Phase 2 — reflection-path haze (what the probe fake covered)

Reflections must see the same air:
- **SSR miss fallback** (trace.wgsl): `env = env * t_ray + color * (1 - t_ray)`
  where `t_ray` uses the DISTANCE THE REFLECTED RAY TRAVELS through the
  medium — for the env/probe fallback that is "to the probe box wall" (the
  box_project intersection already computes it) or a fixed far distance.
- **IBL specular** (brdf_pbr): same term on the prefiltered sample, using the
  probe-box distance when a probe is enabled, else a fixed distance.
- **BVH hits** (bvh_trace.wgsl): `t` over the actual hit distance — free,
  `best_t` is already there.
- SSR *screen-space hits* need nothing: they sample the color buffer, which
  Phase 1 already fogged... note the ordering caveat: SSR samples the
  PRE-effects composite, so hits see unfogged color and the composite adds
  reflection before fog runs — the reflected content then gets fogged by the
  RECEIVER's distance, not the reflected path length. That is the standard
  game-engine approximation; document, don't fight.

### Plumbing (mechanical, mirrors bloom/ssr_temporal)

scene/post_process.rs AtmosphereConfig (+serde defaults) → scene-loader map →
renderer post_process.rs (structural triggers on `enabled`) →
editor-protocol SetPostProcess fields → editor state.rs apply + inverse →
mcp set_post_process params + description → effects cache_key/template axis +
uniforms → wgsl_validation pins (fog term present when on, absent when off).

### Arena migration

When Phase 1+2 land: delete the probe's baked haze hemisphere
(gen-assets.py) and set `atmosphere: { color: ~[0.016,0.019,0.028],
density: ~0.008, base_height: 0, height_falloff: ~0.05 }` in author.js —
same look, but it applies to any scene and both light paths.

## Landing sequence (verified against the code, 2026-07-26)

Read the relevant files before writing this up; the notes below are what the
code actually looks like today, not what the design assumed.

1. ✅ **Config types.** `AtmosphereConfig { enabled, color, density, base_height,
   height_falloff }` in `packages/crates/scene/src/post_process.rs` (serde with
   `#[serde(default)]` initialisers matching the renderer defaults, exactly like
   `SsrConfig`), mirrored as `Atmosphere` in
   `packages/crates/renderer/src/post_process.rs`. The two are **mirrored, not
   shared** — that is the established house convention here (`SsrConfig` ↔
   `Ssr`); don't "improve" it as a side effect.
2. ✅ **scene-loader** maps scene → renderer in the same place it maps `ssr`.
   (Also: both `RendererProfile` defaults bundles enumerate `PostProcessing`
   field-by-field, so they need the new field — haze stays OFF even in
   `Cinema`, since it's scene-authored art direction, not a quality tier.)
3. ✅ **Cache key.** Add `atmosphere: bool` to `ShaderCacheKeyEffects`
   (`render_passes/effects/shader/cache_key.rs`) and an `{% if atmosphere %}`
   arm in `effects_wgsl/compute.wgsl` + `template.rs`. **`enabled` is the
   structural axis; every other field is a live uniform.** Off ⇒ the fog term
   is not compiled in at all, which is what "zero cost when off" has to mean.
4. ✅ **The effects pass has NO params uniform buffer today** — bloom/SSR each own
   theirs and the effects pass owns none. One must be added: copy the
   `BloomParams` shape in `render_passes/bloom/render_pass.rs` (gpu_buffer +
   `raw_data: [u8; BYTE_SIZE]` + `MappedUploader`, `BYTE_SIZE` padded to 16),
   add a bind-group entry in `effects/bind_group.rs`, and write it per frame
   from `render.rs` next to the existing `bloom.params.write(...)` call. This is
   the bulk of Phase 1 and the reason it isn't a one-file change.
5. ✅ **Phase 1 shader**: `effects_wgsl/helpers/atmosphere.wgsl` with
   `apply_atmosphere(rgb, coords, camera)` — reconstruct view distance from
   depth (the pass already binds depth for DoF), `t = exp(-density * dist)`
   with the closed-form height integral when `height_falloff > 0`, sky pixels
   at a large fixed distance. Apply **before** bloom in `compute.wgsl`.
   As built: `load_depth` / `linearize_depth` moved out of `dof.wgsl` into a
   shared `helpers/depth.wgsl` gated on `dof || atmosphere` — haze without DoF
   couldn't otherwise see them. `AtmosphereParams` is carried through
   `RenderPassesDescriptors` (like `hzb_texture`) because `from_resolved` is
   sync and has no gpu handle. Browser-verified (2026-07-26) as a three-state A/B/C in the `atmosphere`
   test scene: haze off = no depth ramp at all; haze on with falloff 0.25 =
   pillar ramp + mast height gradient + sky gradient; haze on with falloff 0 =
   pillar ramp unchanged, mast and sky both flat. State B is what proves the
   height integral is real rather than the distance term in disguise.
6. **Phase 2** — NOT STARTED. Scoped against the code 2026-07-26; it is a
   bigger change than the design implies, for one reason: **the haze params
   have to reach the MATERIAL shaders**, and `AtmosphereParams` is owned by the
   effects pass, which the material passes never bind.

   Three routes were considered:
   - *A new binding on every material bind group* — `material_opaque`'s group 0
     is already 25+ entries and the same uniform would have to be added to
     transparent + decal + edge_resolve too. Widest change, most churn.
   - **Pack the haze block into `frame_globals`** (RECOMMENDED). That uniform is
     already documented as "bound alongside the camera in every pass that pulls
     camera", it is already bound by the effects pass (binding 5) AND
     `material_opaque` (binding 22), and it currently carries 16 bytes of pure
     padding in a 32-byte buffer. Growing it to 64 bytes costs one shared
     struct edit (`shared_wgsl/frame_globals.wgsl` + its Rust writer) and
     reaches every consumer with NO new bindings anywhere. Phase 1's dedicated
     `AtmosphereParams` would then be redundant and should be folded in rather
     than left as a second source of the same numbers.
   - *Leave the reflection paths unhazed* — rejected: SSR hands off to IBL
     specular at `spread_cutoff`, so hazing one path and not the other puts a
     visible seam right at the cutoff. Phase 2 is all-or-nothing across SSR
     miss + IBL specular + BVH hits.

   The remaining cost after the uniform reaches the materials: an `atmosphere`
   structural axis on `ShaderCacheKeyMaterialOpaque` (and transparent/decal) so
   "zero cost when off" still holds there, then the term itself on the SSR miss
   fallback (`ssr_wgsl/trace.wgsl` — reuse the `box_project_env_dir`
   intersection distance), IBL specular (`brdf_pbr.wgsl`), and BVH hits
   (`bvh_trace.wgsl`, `best_t` is already there).
7. ✅ **Editor surface**: `SetPostProcess` fields in editor-protocol → editor
   `state.rs` apply + inverse → MCP `set_post_process` params **and its
   description** (there is a native test asserting MCP tools and docs stay in
   sync — it will fail if the description isn't updated).
8. ✅ **`wgsl_validation` pins**: assert the fog term is present when on and
   absent when off.
9. ✅ **Test scene** `examples/test-scenes/atmosphere/` — `author.js` +
   `project/` + `bundle/` + `verify.md`. NO `golden.png`: it was authored on a
   portrait window and goldens follow the live viewport aspect, so committing
   this capture would mislead anyone on a landscape window. `verify.md` is
   written against what the three states actually showed, per the `shadows-all`
   lesson.

## Non-goals

Volumetric light shafts / froxel scattering (different feature tier);
per-light in-scattering; physically-derived Rayleigh/Mie (this is a stylized
uniform medium — one color, one density).
