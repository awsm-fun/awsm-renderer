Draft: not released.

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
| No AA | 6.078 |
| SMAA | 5.269 |
| MSAA 4× | 5.309 |
| TAA native | 1.449 |
| TAA 0.67 | 2.991 |
| TAA 0.5 | 5.759 |

In Dreidel Derby (1920×1435, mid-round), TAA native has half of MSAA's error
against the same kind of reference (0.873 against 1.864); on its title screen
it is only narrowly better (1.217 against 1.310).

GPU frame time at 1920×1435 (M1 Max, Chrome):

| | No AA | MSAA 4× | TAA native | TAA 0.67 | TAA 0.5 |
|---|--:|--:|--:|--:|--:|
| Dreidel Derby mid-round (unpaced) | 10.74 ms | 14.77 ms | 12.22 ms | 7.30 ms | 6.16 ms |
| `kitchen-sink` | 4.81 ms | 5.71 ms | 5.98 ms | 4.92 ms | 4.56 ms |
| `taa-thin` | 3.71 ms | 6.04 ms | 4.97 ms | 3.34 ms | 2.71 ms |

The resolve costs 1.03-1.17 ms at 1920×1435 output. With TAA off, frame time
is unchanged against 0.45 (within ±0.114 ms on the test scenes; Dreidel Derby
no AA 10.64 against 10.67 ms). Details and the remaining gaps:
`docs/TAA.md` §7.6.

The default stays MSAA 4×: not every verification gate is met.

## New

- **TAA** (`AntiAliasing::taa(TaaConfig)`): smooths geometric edges and also
  the shading, specular and sub-pixel aliasing MSAA cannot reach. Motion
  vectors come from a fifth geometry target with the previous frame's vertex
  evaluation (camera, object, instance, skin, morph and vertex-hook motion),
  or, on devices with 32 bytes per sample, from the prep pass (camera and
  rigid motion; deforming meshes are made reactive there).
- **Temporal upscaling**: `TaaConfig::render_scale` `0.5..=1.0`, presets
  `TaaPreset::{Native, Quality (0.67), Balanced (0.58), Performance (0.5)}`,
  optional RCAS sharpening (`sharpen`, 0.25 in the upscaling presets).
  `set_taa_render_scale` changes the scale at runtime without a pipeline
  compile or a history reset: the lever for a resolution governor.
- **Reactive mask**: transparents, particles, lines and materials whose
  colour changes over time distrust their history instead of trailing.
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

See `docs/TAA.md` §6. In short: trails at the silhouette of moving objects
on the first uncovered frames, and over particles and transparents; contact
pixels; soft opaque-path HUD below scale 1; thin detail lost at 0.5 (73% of
isolated sub-pixel features); materials that read the frame clock are always
reactive unless marked `temporal: stable`; the 32-byte fallback does not see
skin, morph, vertex-hook or per-instance motion. Several verification gates
are not met; the list is in `docs/plans/taa-upscaling/05-verification.md`,
"Results".

## Outstanding before release

- The Android device check (`docs/checklist-android.md`): the 32 B/sample
  fallback has only been forced on an M1, not run on a device that needs it.
- The frame-cost targets that are not met: the resolve at 1.03-1.17 ms
  against 0.6 ms, and Dreidel Derby mid-round TAA native at 12.1-12.2 ms
  against 11.5 ms. TAA 0.67 meets its 7.5 ms target only when the game draws
  on every refresh (7.30 ms); at the game's own 60 fps pacing on a 120 Hz
  display the GPU clocks down between frames and it reads 8.43 ms.
- The verification gates listed as not met in
  `docs/plans/taa-upscaling/05-verification.md`, Results.
