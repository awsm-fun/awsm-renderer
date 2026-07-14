# verify: bloom-post

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/bloom-post/project}`; wait ~3.5s; `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:1.1, pitch:0.5, radius:12, look_at:[0,0.8,0]}`; `wait_render_settled`; screenshot (state `bloom`).
  4. Tonemapper re-grade check: `set_post_process {tonemapper:'khronos_neutral'}` (or another), re-settle, screenshot — the overall grade shifts (highlights roll off differently) without changing geometry.

  Three emissive spheres at increasing strength (2 / 5 / 10, red / green / blue)
  in a depth line over a dark floor + a non-emissive gray reference sphere.
  Post: ACES tonemapper, bloom on (threshold 1.0, intensity 1.2, scatter 1.0).

expect:
  - HALO size/strength SCALES with emissive power: `emissive-2` (red) a subtle
    halo, `emissive-5` (green) a stronger halo, `emissive-10` (blue) a blown-out
    bright halo/bleed.
  - The `gray-reference` sphere is matte with NO halo (below the bloom threshold).
  - The floor picks up colored glow around the bright emitters.
  - (tonemapper swap) the image re-grades — highlight rolloff visibly changes.

fail:
  - No bloom at all (all spheres crisp discs) — bloom off / threshold too high.
  - The gray reference glowing (threshold too low / everything blooms).
  - Halos NOT scaling with strength (all same size) — HDR emissive not driving bloom.
  - The whole frame blown white (tonemap/exposure broken), or a tonemapper switch
    doing nothing.
