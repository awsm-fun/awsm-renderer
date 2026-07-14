# verify: instancing-stress

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/instancing-stress/project}`; wait ~4s; `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:0.6, pitch:0.5, radius:55, look_at:[0,0,0]}`; `wait_render_settled`; screenshot (state `city`).
  4. Orbit interactively (change yaw over several frames) — the grid must stay
     interactive (this is also the Layer-B `instancing` frame-budget check).

  The axis-5 explicit instancer NodeKind: ONE instancer node (`city-3000`)
  owning 3000 box instances (city-height grid, per-instance colors) over a
  floor. 3000 instances must NOT become 3000 scene nodes.

expect:
  - A dense city-block grid of ~3000 colored boxes (pastel per-instance colors),
    varied heights, filling the view.
  - The Outliner shows only a HANDFUL of nodes (Directional Light, Plane, Box,
    `city-3000`) — the 3000 instances ride one instancer node, not 3000 nodes.
    Status bar: ~4 nodes / 2 meshes / 1 material / 1 bucket.
  - Renders + orbits at interactive rate (no multi-second hitching).

fail:
  - The Outliner exploding to thousands of nodes (instances materialized as scene
    nodes).
  - Only one box / a partial grid (instancer transforms not applied).
  - All boxes one color (per-instance colors not fed).
  - Frame time blowing the budget / non-interactive (see Layer-B `instancing`).
