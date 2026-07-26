# Froxel volumetrics — design + landing sequence

Status: designed 2026-07-26, not started. Scoped against the real code, not
against a generic froxel-volumetrics paper.

The analytic haze in [`atmosphere.md`](./atmosphere.md) (landed) gives *aerial
perspective*: distance fades toward a colour. It cannot give **light shafts** —
a spot beam visible in the air, a pool of lit haze under a fixture — because it
never asks which lights reach a point in the medium. That's this feature.

## Does this want a new crate?

**No.** The `particles` precedent doesn't transfer, and it's worth saying why
rather than re-litigating it later: `awsm-renderer-particles` is a crate because
it's pure CPU simulation with **no renderer dependency at all** (its whole
dependency list is curves + geometry + glam). Volumetrics is bind groups,
compute pipelines, the shadow atlas and the froxel light lists — it cannot be
expressed without `awsm-renderer`'s internals.

What *could* be extracted is the pure math: the slice↔depth mapping, the
Henyey-Greenstein phase function, the jitter sequence. That's a few dozen lines
with no dependencies and no consumers outside this pass. A crate boundary there
buys nothing and costs a workspace member, a version to bump, and a
cross-crate edit every time the froxel layout changes.

So: a render-pass module under `render_passes/volumetrics/`, structured exactly
like `bloom` and `ssr` — self-contained `bind_group.rs` / `pipeline.rs` /
`render_pass.rs` / `shader/`, an `Option<VolumetricsRenderPass>` on
`RenderPasses` that is `None` until the first enable, built awaited by
`set_post_processing` so the next frame dispatches without compiling.

## What the code already gives us (verified 2026-07-26)

Three facts do most of the work, all checked in the source:

1. **`material_prep` is the template, not `bloom`.** It is already a COMPUTE
   pass that binds the lights group AND the shadow group with compute
   visibility — exactly the volumetrics shape. Bloom is the template for the
   *pass scaffolding* (lazy `Option<Pass>`, params uniform, `ensure_size`);
   `material_prep/bind_group.rs` is the template for the *bind groups*.

2. **The shadow bind group is standalone and reusable.**
   `shared::material::bind_group::shadow_bind_group_layout_entries(compute_visibility: bool)`
   builds it in one call — pass `true`, as `material_prep/bind_group.rs:93`
   does. **It has TEN entries (0..=9), not eight**: bindings 8
   (`shadow_cascade_array`) and 9 (`shadow_cube_2d_array`) were added after the
   original set. The WGSL side is
   `shared_wgsl/shadow/bind_groups.wgsl`, parameterised by a
   `shadow_group_index` template var, exposing
   `sample_shadow_descriptor(desc, world_pos, world_normal)` — the same
   function the surfaces call, so the volume cannot disagree with the geometry
   about where the shadows are.
   - **Use the HARD path per froxel.** That function also carries PCSS/Vogel
     soft filtering (`shadow_tap_count`, blocker search). At froxel rates it's
     unaffordable and pointless — the volume integral blurs the result anyway.
   - Bindings 10..12 in the opaque pass (`edge_data` etc.) are an *extended*
     group that `shadow_bind_group_layout_entries` does NOT cover. Don't copy
     opaque's group wholesale.

3. **The lights group is a fixed 4-entry shape**, mirrored verbatim by opaque
   and prep. Copy `create_lights_bind_group_layout_key`
   (`material_prep/bind_group.rs:627`):

       0 lights_info    Uniform            -> ctx.lights.gpu_info_buffer
       1 lights         Uniform            -> ctx.lights.gpu_punctual_buffer
       2 lights_storage ReadOnlyStorage    -> ctx.light_culling_buffers.storage_buffer
       3 cull_params    Uniform            -> ctx.light_culling_buffers.params_buffer

   WGSL side, copy `material_prep/.../bind_groups.wgsl:83-95` — it declares the
   four globals, its own `CullParams` copy, then includes
   `shared_wgsl/lighting/light_access.wgsl` and
   `shared_wgsl/lighting/froxel_walk.wgsl` (which needs the
   `froxel_slice_count` template var). Note the path: froxel_walk lives under
   `shared_wgsl/**lighting**/`, and `LightsInfoPacked` for consumers is the
   80-byte version in `light_access_types.wgsl` (the cull pass declares a
   48-byte tail-truncated copy of the same buffer — use the consumer one).

   `froxel_base_for_pixel(pixel_xy, view_z)` + `froxel_light_count(base)` are
   the entry points; injection calls them with the froxel CENTRE pixel and the
   slice's view depth, so the volume enumerates lights in the order the
   surfaces do — directional prefix included.

**Bind-group budget.** The adapter ceiling is `maxBindGroups = 4` and both
material passes are already at it (which is why the transparent pass can't bind
shadows at all). A compute pass has its own budget, so volumetrics spends
three and keeps one spare: group 0 = volume textures + params + camera,
group 1 = lights (the 4-entry shape above), group 2 = shadows.

## Config shape (landed)

The volumetric knobs live ON `AtmosphereConfig` rather than in a config of
their own, because they describe **the same medium** — `color` is the radiance
unlit air glows at (what an infinitely distant surface fades to), `density` the
extinction, `base_height`/`height_falloff` the profile. Only the integration
changes. So the switch is one three-way `mode`, not an enable plus a style flag:

    mode: AtmosphereMode         // STRUCTURAL — Off | Fog | Volumetric
    scattering_anisotropy: f32   // Henyey-Greenstein g, default 0.3 (forward)
    volumetric_temporal: bool    // STRUCTURAL — reproject + blend the volume

`AtmosphereMode` mirrors the renderer's `AtmospherePhase`, so the meaningless
"volumetric but disabled" state can't be spelled at any layer — schema, wire,
UI or shader key. `atmosphere_phase()` in `effects/pipeline.rs` is the single
place mode → phase is resolved, and it's now near-identity.

**Surfaced everywhere**: editor drawer (one `Haze` select + density / base
height / falloff / scatter-g / temporal rows), `SetPostProcess`, MCP
`set_post_process` + `get_post_process`, and persisted through project.toml ⇄
scene.toml (so it rides the player bundle). Per-light participation is separate:
`set_light_volumetric_intensity`, also animatable.

**Zero cost when off** is a test, not a claim: with `mode: Off` the emitted
effects shader contains no atmosphere identifier at all — no include, no
uniform, no branch (`atmosphere_term_is_present_only_when_enabled`, which
strips comments so it asserts on code rather than prose). The froxel pass is
additionally lazy: nothing is allocated or compiled until `Volumetric`.

`scattering_anisotropy` defaults to 0.3 rather than 0: real haze and smoke are
strongly forward-scattering, and it's what makes a beam pointed toward the
camera flare instead of reading as a flat grey cone.

**Until the froxel pass exists, `Volumetric` degrades to `Analytic`** — same
medium, integrated more cheaply — rather than rendering no haze at all. Pinned
by `volumetric_currently_degrades_to_analytic`, which is the reminder to flip
it.

## Stages

**Status: WORKING.** Beams render — a luminous cone in the air above a slotted
occluder, with a dark shaft below where a bar blocks the light, and lit pools +
shadow stripes on the floor. Browser-verified 2026-07-26.

Four bugs on the way, three of which only a browser could find:

1. A `TextureDescriptor` defaults to **2D**, so every 3D view of the volume (and
   of the effects dummy) was invalid, taking the whole Effects bind group with
   it. Naga validates the *shader*; nothing native validates texture/view/layout
   agreement.
2. The volume inherited the light-culling **far plane (~10 km)**. 32 exponential
   slices over that make the far ones kilometres thick, saturating to solid haze
   and washing the frame flat. Hence `volumetric_distance` (default 80 m).
3. **The inject bind group had the scatter volume in two usages at once** —
   sampled at slot 2 (only because the layout is shared with `integrate`) and
   storage-write at slot 3. WebGPU's synchronization-scope rule is per
   SUBRESOURCE and keys off what the bind group DECLARES, not what the shader
   body reads. The code carried a confident comment asserting the opposite;
   inject now binds a 1×1×1 dummy at slot 2.
4. Not a renderer bug at all: the **MCP server binary was stale**, and since it
   deserializes and re-serializes each command, it silently dropped every field
   it didn't know — so `atmosphere_mode` never reached the editor while
   `density`/`color` did. Two "the feature does nothing" screenshots came from
   that. The MCP tool docs warn callers that unknown fields are silently
   ignored; the same hazard applies to the SERVER, and restarting the dev task
   after a protocol change is now part of the loop.

### Quality + cost

Blockiness FIXED by filtering the fetch. The composite samples the integrated
volume trilinearly (`textureSampleLevel`) instead of `textureLoad`-ing a froxel
centre, so the hardware interpolates across all three axes for the price of one
sampler. Watch the half-texel offset: froxel values live at CENTRES, so slice
`i` is at `(i + 0.5)/n` — sampling at `i/n` shifts the whole volume half a
froxel toward the origin and the beams lean away from their lights.

Three costs deliberately bounded:

- **Shadow filtering is forced HARD in the volume** (`shadow_force_hard` on the
  shared shadow include, set only by this pass). The plan called for it from the
  start and the first implementation didn't actually do it: a PCSS light runs a
  ~24-tap blocker search plus ~32 PCF taps, and at ~260k froxels on a 1080p
  frame that is millions of texture reads per light for detail the volume
  integral immediately blurs away. Surfaces are untouched.
- **The integrate march early-outs** once transmittance drops below 0.002 — the
  remaining slices contribute nothing the rgba16float target can hold. The tail
  is filled with the saturated value so the composite still reads correctly.
- **Zero-density froxels skip the light walk entirely**, which is most of the
  volume in any scene with a haze layer rather than uniform fog.

`volumetric_temporal` remains unimplemented (the config axis and cache key
exist). With the filtered fetch the beams already read as air, so it's now a
refinement — jittered sampling plus a history blend — rather than the thing
standing between here and shipping.

## Two bugs found from the live editor (2026-07-26)

Reported by David against the dance-off stage: *the haze changes massively with
zoom level*, and *zoomed in, the cone edges are hard straight-edged facets with
banding inside them*. Both were investigated by measurement before any art
tuning, and neither had the cause the reporting suggested.

### (a) The medium had no ambient in-scatter — FIXED

The suspicion was the far plane: `volumetric_distance` is measured from the
camera, so zoomed out the stage falls beyond it and the composite clamps to the
last slice. **That is not what was happening.** Clamping *under*-estimates the
medium (it stops the air early), and at the framings in question it was moving
almost nothing — the backdrop sits ~2 m past a 25 m far plane.

The decisive test was an A/B of the two haze modes on the *identical* medium,
since they are documented as "the same air, integrated differently". Mean luma
over the stage region, orbit radius 25, `density 0.11`:

| mode | mean luma |
|---|---|
| `off` | 22.2 |
| `fog` (analytic) | **50.6** |
| `volumetric` | **14.5** |

A 3.5x disagreement. The analytic path ends in `rgb * T + color * (1 - T)`; the
volumetric path's source term was `scattered *= scattering_color * density`,
where `scattered` is *only* the punctual + directional light walk. So air that
no light reaches in-scattered **nothing** — the medium was a pure absorber, and
the volumetric mode faded distant surfaces to BLACK where the analytic mode
fades them to `color`. Real media in-scatter the room (sky, bounce, walls), not
only the lights the culling grid bins.

That also explains the zoom dependence exactly, and why it felt like more than
fog: close up the ray is short *and* crosses the beams, so in-scatter wins and
the haze is net ADDITIVE (measured 26.4 → 29.1 at radius 8); wide, the ray is
long and mostly outside the spots' 8 m range, so extinction has nothing to
replace it and the haze is net SUBTRACTIVE. The sign of the effect flipped with
camera distance — a 1.7x swing in stage brightness from a camera move alone.

Fixed by making the source term additive:

    scattered = (scattered + volumetric_params.scattering_color) * density

The weight on the ambient term is exactly **1.0**, and that is derived rather
than tuned: with source `S = color * density` and extinction `density`,
`integrate`'s energy-conserving slice term `S/sigma_t * (1 - exp(-sigma_t*d))`
telescopes down the column to `color * (1 - T)` — the analytic term,
identically. `ambient_inscatter_telescopes_to_the_analytic_haze_term` pins the
algebra over exponential slice thicknesses and a range of colours/densities;
`volumetrics_inject_adds_ambient_inscatter` pins the shape in the emitted WGSL
with comments stripped, so the term can't be silently deleted.

Note what the fix also removes: `scattering_color` no longer multiplies the
lights. The medium's scattering albedo is now neutral, so **a beam is the colour
of its light** — a white hazer doesn't turn a red beam blue — and `color` keeps
one meaning in both modes instead of being a tint in one and a fade target in
the other.

Browser-verified: volumetric now measures **53.7** against analytic's 50.6 at
the same framing. The residual 6% is the beams' in-scatter, which is exactly
what the volumetric path is supposed to add on top.

### (b) Froxel Z quantization — CONFIRMED, not yet fixed

Confirmed by changing *only* the slice distribution (`volumetric_distance`
25 → 8, one live uniform, same camera, same everything else): the hard straight
edges on the beam cones moved and softened. They are depth-slice boundaries seen
at a glancing angle, not XY tile edges and not shadow-map aliasing.

The arithmetic says why. The volume borrows the light-culling grid's near plane,
which is the **camera's** near plane (0.1 m), and slices exponentially from
there: `slice(z) = 32 * ln(z / 0.1) / ln(z_far / 0.1)`. At `volumetric_distance
= 25` that puts **half of the 32 slices between 0.1 m and 1.6 m** — empty air in
front of the lens for any framing of a stage. At the authored camera the whole
subject (view_z ~5–9 m) lands in slices 22.7–26.1: **about 3.4 slices for the
entire stage.** Each is ~1 m thick where the beams are ~1.5 m wide.

That distribution is right for its original job and wrong for this one.
Exponential slicing exists to equalize *screen-space* error over a 0.1 m → 10 km
light-binning range; volumetric error is world-space and the range is tens of
metres.

Still to do, and the two are entangled — see below.

### The far plane stopped being an art knob — FIXED

`volumetric_distance` is documented as a cost/quality budget, but it was welded
to the amount of medium: beyond it the composite clamped, so the air simply
ended. Demonstrated above — dropping it 25 → 8 to buy z-resolution visibly
*removed haze*. You could not spend the slice budget where the beams are without
changing the look.

Fixed with the Frostbite split: froxel volume near, **analytic continuation
far**. The composite had everything it needed already — the closed-form height
integral describes the same medium, and past the volume there is no beam detail
left to preserve. Order follows the light (it crosses the far segment first):

    (rgb * T_analytic + color * (1 - T_analytic)) * T_volume + inscatter_volume

with the analytic term over `[volume_far, surface]` **only**. This is not
double-fogging and the code says so at length: the two terms cover disjoint
segments of the ray, and running both over the same one is the
extinguish-the-air-twice bug the mode switch exists to prevent.

Sky pixels stop being pinned to "exactly `volumetric_distance` away" and get
the saturating distance the analytic path already uses.

Shape of the change: the medium math (density profile, its closed-form
integral, the view-ray reconstruction) moved out of `atmosphere.wgsl` into a
shared `helpers/atmosphere_medium.wgsl`, included for **either** haze phase.
`atmosphere.wgsl` keeps only `apply_atmosphere`, so the existing pin that the
analytic term compiles for exactly the analytic phase still holds. Exactly the
precedent `depth.wgsl` set when `load_depth` had two consumers.

Browser-verified as the decoupling it claims to be. Mean luma over the stage,
close framing, sweeping the knob across a 7.5x range:

| `volumetric_distance` | 8 | 25 | 60 |
|---|---|---|---|
| mean luma | 70.3 | 71.6 | 68.4 |

A 4.6% spread, where the same sweep used to change the image obviously. The
residual is honest and expected: the volume contributes *beam* in-scatter that
the analytic tail cannot (the tail has no lights), so moving the boundary moves
a little energy across it.

### (b) is now the visible defect

With the medium in-scattering everywhere rather than only inside beams, the
froxel grid shows up across the whole frame instead of hiding in the dark — the
16 px x 32 slice blocks are plainly visible at a close framing. Worth stating
because it looks like a regression and isn't: the structure was always there,
the old medium was just too dark to show it.

Order of attack, now that the far plane is a free knob:

1. **Range** — `volumetric_distance` can drop to roughly the subject's depth
   extent at no cost to the look, which puts slices on the stage instead of in
   the empty air in front of the lens.
2. **Distribution** — the volume still borrows the *camera's* 0.1 m near plane
   for its exponential anchor. Even at a short range that wastes the near half
   of the budget. The volume wants its own near plane, or a distribution whose
   error metric is world-space rather than screen-space.
3. **Count** — 32 → 64 last, not first. Frostbite and UE both ship 64, and it is
   2x the froxels (~16 MB of rgba16float at 1080p) for 2x the z-resolution, but
   it is the expensive lever and should be spent only if a correctly-ranged,
   correctly-anchored volume still bands.
4. **`volumetric_temporal`** — still unimplemented, and now clearly the finisher
   rather than a nicety: jitter plus a history blend is what turns a
   one-sample-per-froxel volume into smooth air.

## Per-light `volumetric_intensity`

A light's contribution to the medium is an artistic knob, not a physical
consequence: a key light usually shouldn't fog the room, and a beam fixture
should blaze in the air even if it barely lights the floor. Default `1.0`,
`0.0` = light the surfaces only.

The ripple, in order: `scene/src/light.rs` (all three `LightConfig` variants,
`#[serde(default)]` so existing scenes round-trip) → `scene-loader` →
`renderer/src/lights.rs` + the packed GPU light struct (check for a free padding
word before growing the stride) → `editor/state.rs` + the light panel →
MCP `set_light_*` **and its description** (the parity test asserts tools and
docs stay in sync).

This is pure plumbing with no GPU passes, it's independently testable, and
nothing else in the sequence depends on it landing first — a good standalone
first commit.

**Data model: DONE.** The field exists on `LightConfig` and `Light`, defaults to
1.0 everywhere, maps through scene-loader, and packs into the word at offset 60
that used to be pure padding — so the 64-byte stride and every bind group are
unchanged. Tests pin the offset and the stride, not just the round-trip.

**Setter surface: DONE.** `LightParamKind::VolumetricIntensity` rides the
existing `SetLightParam` command, so it inherits its undo, its coalescing and
its readback for free — and, because that enum is the ANIMATION track-param
enum, the knob is animatable as a side effect. That's the point rather than an
accident: animating a beam's presence in the air is exactly what a light show
wants, and it means a cue can bloom a beam into the haze without touching its
surface lighting.

The ripple landed together: renderer `LightParam` + its read/apply arms, both
1:1 `light_param` lowerings, the `add_track.rs` "Volumetric" row, the
`ReadbackTarget::LightParam` readback, the inspector row, and MCP
`set_light_volumetric_intensity` with its description + parity matrix + MCP.md.

Note the asymmetry with `Range`/`InnerAngle`/`OuterAngle`: those have
"wrong light kind → no-op / Null" arms. This one has none, because every light
kind participates in the medium. The readback never returns Null and the
add-track row is unconditional.

## Gates

Per the repo's working rules: `task lint` + `cargo test --all-features` green on
every commit; `wgsl_validation` pins for each new shader variant (present when
on, absent when off, valid across the msaa × reverse-z × temporal matrix);
browser-verified before the pass is called done; no player perf regression when
volumetrics is off (it compiles to nothing — same standard `atmosphere` is held
to).

Test scene `examples/test-scenes/volumetrics/` — DONE. `author.js` +
`project/` + `bundle/` + `verify.md`, no `golden.png` (authored on a portrait
window; goldens follow the viewport aspect).

The occluder is THREE separated bars, not one slotted slab, because the gaps are
the test: floor stripes only prove the shadow map works. What the scene is for is
the beam being occluded IN THE MEDIUM — lit air above, dark shafts below the
bars, lit air in the gaps.

`verify.md` is an A/B/C over the haze mode, run before it was written: volumetric
(cone present), fog (cone GONE, floor identical), off (no haze, crisp horizon).
B is the load-bearing state — it isolates "the volume is lighting the air" from
"the scene has haze in it", which a single screenshot cannot.

## Non-goals

Multiple scattering; per-light phase `g`; volumetric shadows cast BY the medium
onto surfaces; participating media with varying albedo per region (one global
medium, as in Phase 1).


## Publishing 0.27 — needs a human decision (2026-07-26)

The dance-off light show is gated on publishing, but `task publish -- 0.27.0`
does considerably more than publish, and all of it from the `volumetrics`
branch, which is **21 commits ahead of `main` and unmerged**:

1. bump + commit + annotated tag `v0.27.0`
2. **publish 14 crates to crates.io** — irreversible; yank is the only undo
3. **deploy BOTH frontends to Cloudflare Pages** — this replaces the live
   `scene.awsm.fun` editor
4. **push the branch and the tag** to origin, which fires the cargo-dist
   MCP-binary release on CI

Steps 2–4 are outward-facing and hard to reverse, and releasing off an unmerged
feature branch is a call about the project's release process, not a mechanical
step. Left for a human.

**Verified so far:** `task bump -- 0.27.0` is clean and idempotent, and
`crates-publish-dry-run` packages + verifies `awsm-renderer-core@0.27.0`
successfully. It then stops at crate 2 — dependency-ordered publishing means
`awsm-renderer-materials` needs `awsm-renderer-core = "^0.27.0"` to exist on the
index, which a dry run never uploads. That is a limitation of dry-running a
multi-crate workspace, not a packaging fault. The bump was reverted; the tree is
back at 0.26.0.

**Unblocking the light show without publishing:** an *uncommitted*
`[patch.crates-io]` in LOCKSTEP-GAMES pointing at this checkout (the games plan
already sanctions this for iteration). Note also that authoring the SCENE does
NOT need the pin — `scene.toml` carries the settings regardless; the pin only
decides whether the *game* renders them. So the art direction can be authored
and exported now and will light up when 0.27 lands.
