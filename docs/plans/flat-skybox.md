# Flat skybox — a 2D backdrop for static-camera scenes

Status: designed 2026-07-27, not started. Scoped against the real code.

## Why

A cubemap skybox is the wrong shape for a scene whose camera never moves. It
pays for 360° of resolution to show one fixed ~60° slice, and it pays in the
most expensive place: dance-off's skybox was **33.5 MB** at 2048/face, in a
37 MB bundle. Dropping it to 512/face got that to 2.1 MB, but the shipped
pixels are still ~90% never seen.

Two dead ends worth recording so they are not re-proposed:

- **"Make the unseen faces black so it compresses."** BC6H is a FIXED-RATE
  block format — 1 byte per texel regardless of content. Black faces cost
  exactly what detailed ones do. Measured: zstd -19 over the whole 2.1 MB
  skybox saves 3%.
- **"Turn on KTX2 supercompression."** It only pays for sparse data, which is
  what this feature removes. On the maps that would remain (the IBL cubemaps,
  which by their nature carry radiance in every direction) zstd -19 saves 3.0%
  and 8.0% — ~65 KB total — against adding a zstd decoder to the wasm renderer
  core plus decode time on every load. `env-bake-cli` even asserts
  `"the cubemap loader rejects any supercompression"`, so it is a two-sided
  change. Not worth it.

With a static camera the background is literally a fixed image. Ship it as
one: a 1920×1080 texture is ~150 KB against 2.1 MB, and it is authored the way
an artist actually thinks about a backdrop.

Note this does NOT remove the IBL cubemaps. Specular + irradiance are what
light the scene, they must stay directional, and they stay cubemaps. This
feature decouples "what the camera sees" from "what lights the scene" — which
`EnvironmentConfig` already treats as three independent slots.

## Shape: a structural axis, not a runtime branch

Per David, and it is the right call: the probe and the env rotation are
runtime uniforms because they are **no-ops when identity** — the same shader
runs either way. A flat backdrop is a structurally different sampling path
(`texture_cube` by direction vs `texture_2d` by screen UV). A runtime branch
would compile both paths into every skybox shader and carry a dead binding
whichever way it is set.

The skybox slot is a scene-LOAD property. It changes when a project loads or
an author picks a different backdrop — never per frame. Paying a pipeline
rebuild for that is exactly the trade the codebase already makes for its
structural axes (`trace.wgsl`: *"the structural permutation axes (§5a): each
compiles ONLY into the variant that needs it"*).

## The thing that makes this cheap (verified)

`skybox_tex` at `@group(0) @binding(11)` is sampled by **exactly one shader**:
`material_opaque_wgsl/skybox_primary.wgsl`, via the `sample_skybox` helper.
Nothing else in the opaque group touches it — IBL uses its own
`ibl_filtered_env_tex` / `ibl_irradiance_tex` bindings, and SSR binds the
prefiltered env under its own group.

So the flat variant needs **no new binding**. It re-declares that same slot as
`texture_2d<f32>`:

```wgsl
// bind_groups.wgsl:37
@group(0) @binding(11) var skybox_tex:
    {% if flat_backdrop %}texture_2d<f32>{% else %}texture_cube<f32>{% endif %};
```

and `bind_group.rs:824` picks `TextureViewDimension::D2` instead of `Cube`.

The declaration lives in the shared `bind_groups.wgsl`, so every opaque shader
sees the same type — and since the axis is on the cache key, every pipeline in
a given scene compiles with the same value. Layout and shader stay in step by
construction.

## Work

### 1. Renderer

- `ShaderCacheKeyMaterialOpaque` gains `flat_backdrop: bool`. Thread it into
  the template context (`template.rs`) like the existing axes.
- `bind_groups.wgsl` binding 11: templated type, as above.
- `helpers/skybox.wgsl`: a `{% if flat_backdrop %}` arm that samples by screen
  UV and **skips the entire ray-reconstruction block** — the perspective
  unprojection, the `w == 0` hazard, the inv-view rotation, the env-rotation
  multiply. Genuinely less work per sky pixel, not merely different work.
  (The env rotation is meaningless for a flat backdrop; say so in the comment
  so nobody "fixes" its absence.)
- `bind_group.rs`: `D2` vs `Cube` on entry 11, in BOTH MSAA layout variants.
- `environment.rs`: the renderer holds either a cubemap skybox or a 2D
  backdrop. `set_backdrop(texture)` alongside `set_skybox`, both marking
  `EnvironmentSkyboxCreate` — which already invalidates the bind group — plus
  the pipeline invalidation the new axis needs.
- `wgsl_validation`: pin that the flat variant samples 2D by UV and contains
  no `textureSampleLevel(skybox_tex, …, ray_dir, …)`, and that the cube
  variant is unchanged. Both across the msaa × reverse-z matrix.

### 2. Scene schema — a BREAKING change, deliberately

`EnvSlot` gains a variant:

```rust
EnvSlot::Backdrop { asset_id: AssetId }   // a 2D image, not a cubemap
```

Per David: **no backwards compatibility.** We break the format and re-export
our scenes. Do not add a migration path or a legacy fallback — those are the
things that make a format permanently harder to reason about, and we have
exactly one first-party scene to re-export.

Two consequences to handle honestly rather than paper over:

- `Backdrop` is only meaningful in the `skybox` slot. In `specular` or
  `irradiance` it is a category error — those must stay directional. Reject it
  at load with a clear error rather than silently degrading.
- `ktx_asset_ids()` currently answers "which KTX cubemaps must ship". A
  backdrop is a regular 2D texture asset on the normal texture path, so it
  must NOT be returned there, and it must be picked up by whatever already
  collects mesh/material textures for the bundle. Check the export census
  actually ships it — the failure mode is a bundle that loads with a missing
  backdrop and no error.

### 3. Loader (`scene-loader/src/environment.rs`)

`slot_image` currently returns a `CubemapImage` for every variant. Split the
skybox slot's resolution so `Backdrop` loads a 2D texture and calls
`set_backdrop`, while the other two keep their cubemap path.

### 4. Editor

- The slot picker gains a **Backdrop image…** entry, on the SKYBOX slot only —
  the specular/irradiance pickers must not offer it.
- David: *"The editor UI changes may require a toggle to specify the kind of
  image it is."* The picker already distinguishes what a slot holds; the
  backdrop entry needs a file/asset picker for a 2D image (PNG/WebP/KTX2 2D)
  rather than the `.ktx2` cubemap import. That is the toggle in practice: the
  slot's KIND decides which importer runs, so the type is chosen at pick time
  instead of being guessed from the file.
- `env_sync.rs`: `LiveEnv` already diffs per slot; the backdrop needs its own
  apply arm and to survive the "failed apply stays dirty" rule.

### 5. Round-trip — the part that actually has to be tested

The feature is not done when it renders. It is done when it SURVIVES:

- editor → `save_project` → reload from disk → still a backdrop, same asset;
- `export_player_bundle` → the 2D image is IN the bundle → the player renders
  it identically;
- the existing `verify_roundtrip` self-test (which clears the KTX stash to
  model a cold load) covers the backdrop too.

The bundle path is where this is most likely to break, because the backdrop
leaves the `ktx_asset_ids` census that every env asset has used until now.

## Gates

`task lint` + `cargo test --all-features` green on every commit;
`wgsl_validation` pins as above; browser-verify BOTH variants (a cubemap scene
must be untouched — this axis changes a shared binding's type, so "the normal
path still works" is the primary regression risk); no player perf regression.

## Expected result

dance-off's bundle is 3.9 MB today (2.10 skybox + 0.52 specular + ~1.2
geometry). The backdrop takes the skybox to ~0.15 MB → **~2.0 MB**. Real, but
no longer urgent — the 37 MB emergency was already solved by re-baking. The
better justification now is the one David gave: it is the right shape for
static-camera games, and we will want it repeatedly.
