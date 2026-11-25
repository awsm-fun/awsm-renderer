# Release notes 0.46

## 0.46.0

0.46 adds temporal anti-aliasing (TAA) and temporal upscaling. The camera is
jittered by a fraction of a pixel each frame, and a resolve pass blends the
frame with the reprojected history of earlier frames. Below a render scale of
1 the same pass upscales: the scene is shaded at 67%, 58% or 50% of the width
and height, and the output is rebuilt at full resolution.

The reference is [`docs/TAA.md`](TAA.md): modes, API, how it works, what apps
must do, known limits and every measurement.

Measured on the `taa-thin` test scene (M1 Max, Chrome; mean error against a
64-sample reference, 0-255, lower is better):

| | Error |
|---|--:|
| No AA | 6.274 |
| SMAA | 5.268 |
| MSAA 4× | 5.525 |
| TAA native | 0.560 |
| TAA 0.67 | 1.254 |
| TAA 0.5 | 2.679 |

In Dreidel Derby (1920×1435; mean error against a 64-sample reference, and
GPU frame time, M1 Max, Chrome):

| | Title error | Mid-round error | In motion | Mid-round frame | Title frame |
|---|--:|--:|--:|--:|--:|
| No AA | 1.172 | 2.014 | 2.01 | 10.69 ms | 13.99 ms |
| SMAA | 1.127 | 1.750 | 1.75 | | |
| MSAA 4× | 1.322 | 1.918 | 1.93 | 14.97 ms | 19.38 ms |
| TAA native | 0.311 | 0.337 | 0.75 | 13.12 ms | 16.56 ms |
| TAA 0.67 | 1.351 | | | 8.62 ms | 9.50 ms |

TAA native is closer to the reference than every other mode, still and in
motion, and 1.9 ms (mid-round) and 2.8 ms (title) cheaper than MSAA 4×. It
costs about 2.5 ms more than no AA: about 2 ms of that is the resolve, a
fixed cost per output pixel. TAA 0.67 is cheaper than no AA.

The resolve costs 1.5-1.8 ms at 1920×1435 output at the default disocclusion
level (1.2-1.3 at the cheapest). With TAA off, pixels are identical to 0.45
and frame time is unchanged (within ±0.107 ms on the test scenes). Details:
`docs/TAA.md` §7.6.

The default stays MSAA 4× for now.

## New

- **TAA** (`AntiAliasing::taa(TaaConfig)`): smooths geometric edges and also
  the shading, specular and sub-pixel aliasing MSAA cannot reach. Motion
  vectors come from a fifth geometry target with the previous frame's vertex
  evaluation (camera, object, instance, skin, morph and vertex-hook motion),
  or, on devices with 32 bytes per sample, from the prep pass plus a small
  overlay pass that redraws only the meshes that deformed this frame, so
  they get exact vectors there too. The resolve averages in the display's
  own space (exposure and tonemapper applied), which is what makes a
  converged image match a supersampled one.
- **Disocclusion levels** (`TaaConfig::disocclusion`): `Depth`,
  `DepthAndMotion`, `DepthMotionAndId` (the default). Each adds a test that
  catches history the one before cannot: the contact line under a moving
  object, then contact between two objects and on moving surfaces. A level
  that is off costs nothing. Costs and a recommended-configuration table:
  `docs/TAA.md` §1.
- **Bloom and depth of field under TAA.** The resolve averages in display
  space, so a very bright thin highlight loses its energy above white
  there. Bloom therefore reads this frame's HDR render
  (`TaaConfig::bloom_source`, default `CurrentFrame`), and depth of field
  runs before the resolve, at render size (`TaaConfig::dof_order`, default
  `BeforeResolve`; the order Unreal's temporal upscalers and AMD's
  FidelityFX guidance use when upscaling). Both keep the old behaviour as
  an option. In Dreidel Derby they take the title screen from 1.020 to
  0.311.
- **New objects** (`TaaTuning::reactive_new_objects`, default 1.0): an
  opaque mesh in its first drawn frame, shown again after being hidden, or
  jumping into place within a few frames of that, starts from a history of
  itself instead of fading in over what was behind it.
  `reset_motion_history(key)` marks the subtree the same way: call it when
  an object teleports.
- **Temporal upscaling**: `TaaConfig::render_scale` `0.5..=1.0`, presets
  `TaaPreset::{Native, Quality (0.67), Balanced (0.58), Performance (0.5)}`,
  optional RCAS sharpening (`sharpen`, 0.25 in the upscaling presets).
  `set_taa_render_scale` changes the scale at runtime without a pipeline
  compile or a history reset: the lever for a resolution governor.
- **Reactive mask**: transparents, particles, lines and materials whose
  colour changes over time distrust their history instead of trailing.
  Particles, lines and other transparents each have their own weight
  (`reactive_particles`, `reactive_lines`, `reactive_transparent`).
  Materials get `MaterialTemporal::{Auto, Stable, Reactive}`
  (`set_material_temporal`, `set_custom_material_temporal`, and `temporal`
  in scene files).
- **HUD under TAA**: HUD meshes with a transparent pipeline are drawn after
  the resolve at output size, unjittered; opaque-path HUD pixels bypass the
  history.
- **Debug views** (`set_taa_debug_view`): velocity, reprojection error,
  history length, rejection, reactive, confidence, frozen jitter, history
  only, current only.
- **Scene schema**: `post_process.anti_aliasing.taa` (render scale,
  sharpening, every tuning field). The editor and MCP `set_anti_aliasing`
  expose the mode, preset, sharpening, tuning and debug views. The model
  tester has a single anti-aliasing selector (None / MSAA 4× / SMAA / TAA
  native / TAA Quality / TAA Performance) and an `?aa=` URL override.
- **Test harness**: `examples/taa-tests` and the `taa-*` test scenes measure
  vectors, trails, stops, cuts, pans, convergence, flicker, reactive content,
  the HUD and upscaling against a jittered reference.

## API additions

- `awsm_renderer::taa`: `TaaConfig`, `TaaPreset`, `TaaTuning`,
  `TaaDebugView`, `JitterOverride`, `history_cap`, `still_gamma`,
  `texture_lod_bias`, the jitter helpers.
- `AntiAliasing::{none, msaa4, smaa, taa}` constructors; `AntiAliasing.taa`.
- `awsm_renderer::taa::{TaaDisocclusion, TaaBloomSource, TaaDofOrder}`;
  `TaaConfig::{bloom_source, dof_order}`; `TaaTuning::{reactive_particles,
  reactive_lines, reactive_new_objects, edge_release}`; `AwsmRenderer::set_mesh_taa_particles`
  (flag particle meshes an app builds itself; the scene loader flags its
  emitters), `taa_debug_variant_ready`, `taa_fallback_overlay`,
  `set_taa_fallback_storage_limit_override`.
- `AwsmRenderer`: `set_taa_render_scale`, `taa_render_scale`,
  `reset_temporal_history`, `reset_motion_history`, `set_taa_debug_view`,
  `taa_debug_view`, `taa_velocity_path`, `set_taa_velocity_path_override`,
  `taa_fallback_reason`, `set_material_temporal`,
  `set_custom_material_temporal`, `set_jitter_override`, and the test hooks
  `debug_read_velocity`, `debug_read_depth`, `debug_read_reactive`,
  `set_debug_motion_history_check`, `debug_verify_motion_history`,
  `set_taa_debug_repeat`.
- `Transforms::reset_history`, `Skins::reset_history`, morph
  `reset_history`.
- `RawMeshData::into_geometry_source` is public, to declare raw geometry into
  an open load (`register_geometry` + `add_mesh`) and let the next
  `commit_load` upload it.
- Errors: `TaaCombinedWithOtherAa`, `TaaRenderScale`, `TaaNotEnabled`.
- GPU pass timings `Taa` and `TaaRcas`.

## Breaking

Struct literals that list every field need the new one:

- `AntiAliasing { msaa_sample_count, smaa, mipmap }` →
  `AntiAliasing { msaa_sample_count, smaa, mipmap, taa: None }` (or use
  `AntiAliasing::msaa4()` / `none()` / `smaa()`).
- `AntiAliasingConfig { msaa, smaa }` (scene crate) →
  `AntiAliasingConfig { msaa, smaa, taa: None }`.
- Also: `MaterialDefinition` (custom material definitions, scene crate)
  gained `temporal: Option<MaterialTemporal>`, so a full literal needs
  `temporal: None`. `MaterialDef` gained the same field; literals using
  `..Default::default()` are unaffected.

Bug fixes that change behaviour outside TAA:

- Decals are no longer cut off after the render size shrinks in a session
  (a smaller viewport, or TAA's render scale). Present since 0.45.
- The editor no longer wedges (nothing draws, renderer commands hang) when a
  node is deleted while its observer waits for the renderer.
- The "point-light shadow cube pool exhausted" warning is logged once per
  overflow, not every frame.

For code that reaches into render internals:

- `RenderContext.viewport_size` is replaced by `render_size` and
  `post_size` (`FrameSizes { surface, render, post }`). Without TAA,
  `post_size == render_size`.
- The camera uniform grew from 560 to 656 bytes in every mode
  (`view_projection_unjittered`, `jitter`, `prev_position`, appended at the
  end; no existing offset moved).

## Behaviour notes

- The scene loader applies the scene's authored anti-aliasing, including
  `taa`, before its commit, replacing the renderer's mode. A scene without an
  AA block gets MSAA 4×, as in 0.45. An authored TAA that cannot be set up
  falls back to MSAA 4× with a warning. Apps that want their own mode copy it
  into the scene config before loading, or call `set_anti_aliasing` +
  `commit_load` after.
- `set_render_scale` with any value other than 1 is rejected while TAA is on
  (`TaaCombinedWithOtherAa`): supersampling and TAA are exclusive; use
  `TaaConfig::render_scale` instead.
- Apps must call `reset_temporal_history` on camera cuts and
  `reset_motion_history` on teleports under TAA. The renderer resets on AA
  and output-size changes, scene load and teardown, and when its camera-jump
  guard fires.
- Custom-vertex materials now recompile after an AA change in every mode. In
  0.45 such a mesh rendered undisplaced after an MSAA flip.
- Switching anti-aliasing while a load is open (meshes declared, before
  `commit_load`) no longer aborts half-way with `MeshNotFound` and a
  per-frame "Effects" bind-group validation error when the scene has
  transparent meshes. The model tester hit this when starting in a non-MSAA
  mode; 0.45 had the same bug.
- With TAA off, output is pixel-identical to 0.45 (36/36 baseline frames) and
  the TAA-off geometry shader sources are byte-identical. The only cost with
  TAA off is the 96 bytes added to the camera uniform.
- If a TAA pipeline fails to compile on a device, the renderer switches to
  MSAA 4× once and reports why (`taa_fallback_reason`).

## Known limits

See `docs/TAA.md` §6. In short: a pixel's first uncovered frame behind a
moving silhouette is one sample, for any history method; an object that
rests over a pixel and is then nudged can leave a one-pixel edge for a few
frames (`edge_release` removes it at a small flicker cost); a 1-2 pixel
highlight moving with an object can sit half a pixel off for two frames;
the glow around a sub-pixel bright source can pulse slightly at the jitter
period in a still image (bloom reads the jittered frame); at render scales
below 1 depth of field is computed at render size (whole-frame error about
3% higher than with DoF after the resolve, bokeh energy, flicker and cost
better); with these defaults a HUD drawn in the transparent pass is not blurred by depth of
field and does not bloom under TAA;
glass, water and lines have no motion vectors and lag behind a moving
background; moving textured surfaces are smooth and slightly soft where MSAA
is crisp and stair-stepped; opaque-path HUD is soft below scale 1; instances
of one mesh share an object id, so contact between them is judged by depth
and motion only.

## Default, and what comes next

The default anti-aliasing stays MSAA 4× in 0.46: TAA is opt-in
(`AntiAliasing::taa(..)`, or `taa` in a scene's AA block). A final TAA round
is planned in `docs/plans/taa-final-round/`. It covers:

- a temporally stable bloom source;
- new-object handling for transparents and appended instances;
- the depth-of-field order below scale 1;
- full re-verification;
- making TAA native the default for new scenes.

The Android device check of the 32 B/sample path (`docs/checklist-android.md`)
is still open; that path has so far only been forced on an M1.
