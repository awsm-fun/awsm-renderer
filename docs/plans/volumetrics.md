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
their own, because they describe **the same medium** — `color` is the
scattering tint, `density` the extinction, `base_height`/`height_falloff` the
profile. Only the integration changes. So the switch is one three-way `mode`,
not an enable plus a style flag:

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

### Known artifact — blockiness

The beams stair-step visibly at the froxel grid: 16 px columns, 32 slices,
nearest-neighbour `textureLoad`, no temporal. Expected at this stage and exactly
what the remaining work addresses — the temporal reprojection stage (already a
config axis, `volumetric_temporal`) plus a filtered fetch. Do NOT ship the
dance-off light show before that lands; the artifact reads as broken.

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
