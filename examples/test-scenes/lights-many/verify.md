# verify: lights-many

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/lights-many/project}`; wait ~4s; `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:0.6, pitch:0.7, radius:20, look_at:[0,0,0]}`; `wait_render_settled`; screenshot (state `light-grid`).

  36 colored point lights in a 6×6 grid (spacing 3, range 2.6 — tighter than
  spacing so each reads as a DISTINCT pool) over a 24×24 floor + 3×3 pillar
  grid. The seeded key directional is deleted, so punctual lights are the only
  dynamic illumination (IBL ambient remains). This scene is the froxel
  (clustered) reverse-Z culling lock.

expect:
  - Per-light colored pools on the floor in ROW order, cycling
    red / green / blue / yellow / magenta / cyan across the six rows — every
    light visibly present as its own colored disc, none missing.
  - Pillars lit on their light-facing sides (colored gradients up the near
    faces), self-shadowed / dark on the away sides.
  - Adjacent pools of different colors blend at their overlaps (e.g. blue↔yellow
    band) but each color's centers stay distinct — not one uniform wash.
  - Interactive frame rate (status bar shows the scene, 36 lights, ~12k tris).

fail:
  - The floor unlit / uniformly grey (ALL punctual lights culled — the exact
    froxel reverse-Z regression this scene guards: tile unproject anchored at
    NDC z=0 → NaN side planes → every light dropped).
  - Only some rows lit, or pools the wrong color / all one color (light indexing
    or culling wrong).
  - Pillars uniformly bright on all sides (no directional falloff) or fully black
    (no light reaching them).
