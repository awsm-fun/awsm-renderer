# verify: ssr-arena

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/ssr-arena/project}`; wait ~7s (heavy scene: ~65 nodes / ~77k tris, generated KTX2 skybox+specular). `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:2.9, pitch:0.28, radius:32, look_at:[0,2,0]}` (the closeup golden framing); `wait_render_settled`; screenshot (state `arena-close`).
  4. Optionally the overview: `set_camera_orbit {yaw:0.55, pitch:0.42, radius:88, look_at:[0,10,0]}`.

  A jetpack-knockout arena recreation: hex-panel polished floor disc with a red
  danger band; 8 rainbow neon wall rings (blue→violet); neon ribs, platforms,
  5 launch pads (glowing ring cores) + a center emblem. Post: khronos_neutral
  tonemap + bloom tuned so only neon cores bloom. This is the arena SSR case.

expect:
  - The polished hex floor MIRRORS the rainbow neon wall rings — the colored
    ring bands continue as reflections in the floor, coherent (not a flat black
    floor).
  - Launch pads read as glowing concentric ring targets (red / green / magenta /
    orange cores) with bloomed centers; their glow also reflects in the floor.
  - The wall rings form a clean rainbow gradient (red danger band, then
    green/cyan/blue/violet up the wall); neon cores bloom, mid-tones do not
    (high bloom threshold).
  - The platform/occluder over the floor stays a SOFT lit maroon/yellow — visible
    as geometry, NOT rendered black (occluder handling).
  - Hex-panel floor tessellation reads through the reflections.

fail:
  - Floor black / no reflections of the wall rings (SSR not sampling the arena).
  - The occluder platform rendering solid black (occlusion/thickness leak).
  - Whole image blown out (bloom threshold too low) or no bloom on the neon cores.
  - Wall rings the wrong colors / not a gradient, or the scene failing to load
    the generated KTX2 environment (flat/untextured skybox).
