# verify: kitchen-sink

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/kitchen-sink/project}`; wait ~5s (skinned rig + particles + SSR); `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:0.25, pitch:0.45, radius:13, look_at:[0,0.8,0]}`; `wait_render_settled`; screenshot (state `everything`).

  Everything at once — the smoke test + the startup-census scene: PBR variant
  spheres (plastic/metal/emissive), a procedural-checker box, 6 froxel-culled
  colored point lights, a skinned CesiumMan mid-stride, a blended particle
  fountain, and SSR on a dark glossy floor. Pass = it all composes without
  crashing/artifacting.

expect:
  - `red-plastic` sphere (matte red), `gold-metal` sphere (reflects the env),
    `emissive` teal sphere (glowing), `checker-box` (procedural checker texture).
  - Colored point-light pools on the floor (6 froxel-culled lights — distinct
    colored patches).
  - The skinned CesiumMan is posed (mid-stride, not a T-pose), even if partly
    washed by the bright particle/light area.
  - A blended particle fountain (soft bright plume) and SSR reflections of the
    spheres in the dark glossy floor.
  - Status bar shows ~35 nodes; no black/pink error surfaces.

fail:
  - Any element missing (no particles, no lights, no reflections, CesiumMan in
    T-pose or absent).
  - Black/magenta error materials, a crash (blank canvas), or the census
    pipeline/shader counts spiking (cf. Layer-B `startup-census`).
