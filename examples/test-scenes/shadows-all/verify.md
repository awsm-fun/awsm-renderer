# verify: shadows-all

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/shadows-all/project}`; wait ~4s; `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:0.5, pitch:0.6, radius:13, look_at:[0,0.3,0]}`; `wait_render_settled`; screenshot (state `all-shadows`).
  4. Optionally orbit yaw ±0.3 and re-screenshot to inspect the lowered-box contact from a second angle.

  Three shadow-casting lights over one receiver floor: a seeded DIRECTIONAL
  (cascades), a SPOT straight down (warm pool), and a POINT/cube (bluish, low
  near the sphere). Casters: tall-box, sphere, thin-bar, lowered-box.

expect:
  - `tall-box` (tan, left) casts a solid directional shadow; its base meets the
    floor with a CONTACT-TIGHT shadow — the shadow touches the box, no bright
    gap between box and its shadow (no Peter-Pan).
  - `sphere` (blue, center) casts a round soft contact shadow directly beneath it.
  - `thin-bar` (salmon, right) casts a long thin shadow — a sliver, tracking the
    bar; not a fat blob.
  - `lowered-box` (green, foreground) sits with its bottom slightly under the
    floor level and shows a CONTACT-TIGHT shadow with NO donut/hole/ring under
    it (the PR#169 world-referenced depth-bias lock).
  - A warm/bright SPOT pool (cone-shaped light patch) illuminates the floor near
    the lowered-box and thin-bar; a cooler bluish POINT contribution near the
    sphere's far side. Shadows sit inside the lit pools.

fail:
  - A Peter-Pan gap: any caster's shadow detached from the object base by a
    bright band.
  - A donut/ring/hole under `lowered-box` (constant-NDC bias ballooning under
    perspective — the exact regression this scene guards).
  - Shadow acne / z-fighting speckle on the floor, or a caster with NO shadow.
  - The whole floor uniformly dark (shadow map not resolving) or uniformly bright
    (no shadows cast at all).
