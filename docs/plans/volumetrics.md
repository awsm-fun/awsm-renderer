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

Two facts do most of the work, and both were checked in the source rather than
assumed:

1. **The shadow bind group is standalone and reusable.**
   `shared_wgsl/shadow/bind_groups.wgsl` is a self-contained group (slot 3 in
   the opaque pass; entries defined once in
   `shared::material::bind_group::shadow_bind_group_layout_entries`) exposing
   `sample_shadow_descriptor(desc, world_pos, world_normal)`. The light-injection
   stage binds the SAME group and calls the SAME function — no new shadow
   plumbing, and no risk of the volume disagreeing with the surfaces about where
   the shadows are.
   - **Use the HARD path per froxel.** `sample_shadow_descriptor` also carries
     PCSS/Vogel soft filtering (`shadow_tap_count`, blocker search). At froxel
     rates that is unaffordable and pointless — the volume integral blurs the
     result anyway. Inject with a single tap.
   - Note the existing caveat in that file: the transparent pass can't bind the
     shadow group because `maxBindGroups=4` is already spent there. A compute
     pass has its own budget, so this pass can afford group 0 = volume + params,
     group 1 = lights/froxel storage, group 2 = shadows.

2. **The light-culling grid is already per-froxel.** `froxel_walk.wgsl` is
   documented as "the SINGLE SOURCE OF TRUTH for how a pixel enumerates the
   lights", with `froxel_base_for_pixel(pixel_xy, view_z)` over 16px tiles and
   an exponential z-slice mapping. The injection stage calls it with the
   *froxel centre* pixel and the slice's view depth, so the volume enumerates
   lights in exactly the order the surfaces do — including the directional
   prefix, which is flat and not froxel-binned.

## Stages

A froxel volume (`rgba16float`, RGB = in-scattered radiance, A = extinction),
sized ~(viewport/8) × 64 slices over the same exponential depth mapping the
light culling uses.

1. **Media injection** — write extinction + albedo per froxel from the
   atmosphere config (the same height-profile function Phase 1 uses, evaluated
   at the froxel centre rather than integrated along a ray). This is the stage
   that makes the medium *3D* instead of a per-pixel function of depth.
2. **Light injection** — for each froxel, walk `froxel_walk` and accumulate
   `light_colour · attenuation · shadow · phase(cos θ) · volumetric_intensity`.
   Henyey-Greenstein for the phase term (one `g` per scene is enough; a per-
   light `g` is a later refinement, not an MVP knob).
3. **Temporal reprojection** (optional, structural) — the froxel volume is
   heavily undersampled, so a jittered slice offset plus a reprojected history
   blend is what turns it from banded to smooth. Gate it like `ssr.temporal`:
   off by default, structural, and only worth compiling when asked for.
4. **Front-to-back integration** — a single pass down the slices accumulating
   transmittance, writing the integrated scattering per froxel.
5. **Apply in EFFECTS** — sample the integrated volume at the pixel's froxel and
   composite. **This REPLACES the analytic fog term, it does not stack with
   it**: the volume already carries the same medium's extinction, so running
   both double-counts the air and the scene goes twice as murky as authored.
   Concretely: the `atmosphere` arm in `effects_wgsl/compute.wgsl` becomes a
   three-way choice — off / analytic / volumetric — on the cache key, not two
   independent booleans.

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

Test scene `examples/test-scenes/volumetrics/`: a spot fixture aimed through a
slotted occluder into a hazy room, so the shafts have hard edges to be right or
wrong about. Write `verify.md` against what the capture ACTUALLY shows — the
`shadows-all` lesson (`shadows-all-verify-vs-golden.md`).

## Non-goals

Multiple scattering; per-light phase `g`; volumetric shadows cast BY the medium
onto surfaces; participating media with varying albedo per region (one global
medium, as in Phase 1).
