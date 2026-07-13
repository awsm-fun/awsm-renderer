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

## Non-goals

Volumetric light shafts / froxel scattering (different feature tier);
per-light in-scattering; physically-derived Rayleigh/Mie (this is a stylized
uniform medium — one color, one density).
