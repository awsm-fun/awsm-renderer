# Atmospheric haze — Phase 2 (reflection-path)

Status: **Phase 1 shipped. Phase 2 not started** (re-verified 2026-07-27 —
`grep atmosphere` finds nothing in `ssr_wgsl/trace.wgsl` or `brdf_pbr.wgsl`).

Phase 1 (view-path fog in the effects pass) landed along with the config types,
the editor/MCP surface, `wgsl_validation` pins and
`examples/test-scenes/atmosphere/`. The froxel volumetrics that followed also
shipped, and `atmosphere.mode` now selects between them
(`volumetric` | `fog` | `off`). Volumetrics' own design record was deleted on
completion — it is in git history if the froxel details are ever needed again.

Everything below is what remains.

## The gap

Reflections do not see the air. A reflected ray that leaves the screen falls
back to the environment and returns it **unhazed**, while the surface it
reflects onto is fogged. The arena's probe-baked haze hemisphere was a scene
fake standing in for exactly this, and it is still the only thing covering it.

This is unfinished under BOTH haze modes: `fog` never touched the reflection
path, and the froxel volume is a VIEW-path integration — a reflected ray is
not a view ray, so it never samples the volume either.

Three consumers, and it has to be all three:

- **SSR miss fallback** (`ssr_wgsl/trace.wgsl`):
  `env = env * t_ray + color * (1 - t_ray)`, where `t_ray` is the distance the
  REFLECTED ray travels through the medium. For the env/probe fallback that is
  the `box_project_env_dir` intersection distance, which the code already
  computes; otherwise a fixed far distance.
- **IBL specular** (`brdf_pbr.wgsl`): the same term on the prefiltered sample,
  probe-box distance when a probe is enabled, else fixed.
- **BVH hits** (`bvh_trace.wgsl`): `t` over the real hit distance — `best_t`
  is already there, so this one is nearly free.

All three because SSR hands off to IBL specular at `spread_cutoff`: hazing one
and not the other puts a visible seam right at the cutoff.

SSR *screen-space hits* need nothing — they sample the colour buffer, which
Phase 1 already fogged. One caveat to document rather than fight: SSR samples
the PRE-effects composite, so a hit sees unfogged colour and the reflection is
composited before fog runs. The reflected content is then fogged by the
RECEIVER's distance rather than by the reflected path length. That is the
standard game-engine approximation.

## Why this is bigger than the design implies

**The haze params have to reach the MATERIAL shaders**, and `AtmosphereParams`
is owned by the effects pass, which the material passes never bind. Three
routes were considered against the code:

- *A new binding on every material bind group* — `material_opaque`'s group 0
  is already 25+ entries, and the same uniform would have to be added to
  transparent + decal + edge_resolve too. Widest change, most churn.
- **Pack the haze block into `frame_globals`** — RECOMMENDED. That uniform is
  already "bound alongside the camera in every pass that pulls camera": the
  effects pass has it at binding 5, `material_opaque` at binding 22. It
  currently carries 16 bytes of pure padding in a 32-byte buffer. Growing it to
  64 costs one shared struct edit (`shared_wgsl/frame_globals.wgsl` + its Rust
  writer) and reaches every consumer with NO new bindings anywhere. Phase 1's
  dedicated `AtmosphereParams` then becomes redundant and should be FOLDED IN,
  not left as a second source of the same numbers.
- *Leave the reflection paths unhazed* — rejected, per the seam above.

Once the uniform reaches the materials: an `atmosphere` structural axis on
`ShaderCacheKeyMaterialOpaque` (and transparent/decal) so "zero cost when off"
still holds there, then the term itself at the three sites.

## Decide this before starting

Volumetrics shipped after this was scoped, and the original design asserted
that volumetrics **replaces** the analytic fog rather than stacking with it
(same medium — running both double-counts the air). If that holds, Phase 2
needs a decision about what it means under `mode = volumetric`:

- does the reflection path take the ANALYTIC term even when the view path is
  froxel-integrated — cheap, but two models for one medium; or
- does it sample the froxel volume along the reflected ray — correct, and much
  more expensive, since the volume is built for view rays?

The cheap answer is very likely right for a stylized look, but it should be a
decision rather than a default. Nothing here should start until it is made.

## Arena migration (pending, gated on the above)

When Phase 2 lands: delete the probe's baked haze hemisphere (`gen-assets.py`)
and set `atmosphere: { color: ~[0.016, 0.019, 0.028], density: ~0.008,
base_height: 0, height_falloff: ~0.05 }` in `author.js` — same look, but it
applies to any scene and to both light paths.

## Non-goals

Per-light in-scattering (that is volumetrics, shipped); physically-derived
Rayleigh/Mie — this is a stylized uniform medium, one colour and one density.

## Gates

`task lint` + `cargo test --all-features` green on every commit;
`wgsl_validation` pins for each new variant (term present when on, absent when
off, across the msaa × reverse-z matrix); browser-verified before it is called
done; no player perf regression when atmosphere is off.
