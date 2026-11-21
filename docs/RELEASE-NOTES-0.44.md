# Release notes — 0.44

## 0.44.1

- **Lights lost below a line after a canvas resize.** The light-culling
  tile-lights buffer was sized for the live tile grid while the froxel
  buffers keep their largest size; after shrink → more punctual lights →
  grow, the shaders indexed past its end and every tile row beyond it got no
  punctual lights (a hard dark band across the frame, until reload). It is now
  sized for the allocated grid. Present since 0.43.
- **Editor player preview: pause shows the playhead.** Pausing kept the time
  the clips had reached instead of the playhead position.

## 0.44.0

0.44 is a round of fixes found by building one large scene (≈380 meshes, 190k
triangles, 14 particle emitters, custom WGSL materials, a clip mixer) in the
editor and shipping it as a player bundle. Most of what follows made the
player differ from the editor without saying why.

## Player bundles

- **Double-sided materials work in the player.** Bundle meshes are
  geometry-only glbs; the loader took sidedness from the glb's empty default
  material instead of the material it binds, so every bundle mesh culled back
  faces. `set_mesh_material` now also updates a mesh's sidedness.
- **Particles: `ParticleDriver`.** The loader builds emitters and the host
  ticks them — which no example did and no doc said. `ParticleDriver::new(&loaded)`
  once, `particles.tick(&mut renderer, dt)` every frame. Hidden emitters are
  now built (hidden) and paused; `set_node_visible` shows them, and lights.
- **Blend custom materials load.** A transparent mesh whose custom material had
  no registration produced `unresolved type 'MaterialData'` and failed the whole
  transparent batch. Such a mesh is now skipped; a failed load unregisters what
  it registered; the registration hash covers alpha mode, sidedness, includes
  and hooks.
- **Custom vertex hooks ship.** Exported as `material.vertex.wgsl`
  (`MaterialDefinition::has_vertex_wgsl`) and loaded by the player.
- **`LoadedScene::teardown`** also frees the scene's custom-material
  registrations, node materials and the glb transforms.
- **Hidden lights load switched off**, not skipped: they keep a key, so tracks
  bind, and `set_node_visible` turns them on. New
  `AwsmRenderer::set_light_enabled`.
- **Anti-aliasing is part of the scene.** `PostProcessConfig::anti_aliasing
  { msaa, smaa }`, applied by the loader (`anti_aliasing_to_renderer`).
- **Camera depth of field without a clip.** `CameraConfig::{aperture,
  focus_distance}`.

## Texture pool budget

The array limit was checked against the padded (power-of-two) count and ignored
the 18 sampled textures the opaque pass binds itself: 17 real arrays padded to 32,
+18 fixed = 50 > 48, a WebGPU validation error and a black frame. The effective
ceiling was 16 arrays; it is now `limit − 18` (30 on a 48 device), padding is
clamped, the check runs before any GPU state changes, empty trailing arrays are
released on remove, and `AwsmRenderer::texture_pool_summary()` reports it. KTX2
bundles (one array per size × transcode format) were the usual way to hit it.

## Rendering

- **Custom MASK materials under MSAA** had no shading kernel (`cs_shade` skipped
  every non-Opaque custom), so they rendered black. Fixed.
- **Custom vertex hooks read the wrong material block** (the mesh-meta offset),
  so every uniform read zero — in geometry, transparent and both shadow passes.
- **Custom materials receive shadows**: `material_pixel_light_shadow(input, i)`
  alongside the froxel light walk, reading the prep pass's visibility buffer.
  `OpaqueShadingInput::receive_shadows`.
- **Depth of field** gathers a near-field layer (scatter-as-gather), rotates its
  spirals per pixel and keeps bloom/haze across the blur: defocused foreground
  highlights open into discs instead of stopping at their silhouette.
- **MSAA**: lines and transparent meshes share one render pass; the last pass
  does not store the 4× colour; `cs_shade` shades identical samples of an edge
  pixel once. `last_msaa_edge_pixels` for diagnostics.

## Animation

- A **weight-0 mixer layer** writes nothing. A target no longer driven gets its
  rest written once and is released, so hand-set values hold.
- **Asset-wide uniform tracks** (`AnimationTarget::SharedUniform`) drive every
  instance that does not override the slot, instead of one arbitrary mesh. Per
  instance overrides are recorded (`DynamicMaterial::uniform_overridden`) and
  respected by `set_dynamic_material_uniform_shared`.

## Editor

- **`load_player_bundle` is a player preview**: clips play, particles run, the
  bundle's cameras are selectable (`camera` param or `set_active_camera`), and
  **`exit_player_bundle`** restores the project. The session is no longer
  poisoned (tombstoned material ids, leaked registrations, the 32-bucket cap).
- **Ghost meshes / duplicate lights**: removing a node never cancelled its
  observers, which kept materializing onto dead entries after a reparent or a
  reload. Fixed.
- Hiding a group stops its emitters; `purge_unused_assets` removes unused
  library materials (`restore_material` undoes it); texture-pool overflow is an
  error toast with rollback; export folds identical texture images and warns
  on array budget; `memory_stats` gains `texture_pool`, `lights`,
  `msaa_edge_pixels`; `render_cpu_ms` no longer counts time waiting for vsync.

## MCP

- Raw commands (`dispatch_command`, `dispatch_batch`, `POST /debug`) **reject
  unknown fields** with their path, instead of applying a partial edit.
- `node_look_at` keeps a stable roll within 5° of vertical.
- New tools: `exit_player_bundle`; `load_player_bundle` takes `camera`.
