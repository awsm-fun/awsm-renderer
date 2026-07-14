# verify: transparent

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/transparent/project}`; wait ~3.5s; `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:0.35, pitch:0.25, radius:9, look_at:[0,1.0,0]}`; `wait_render_settled`; screenshot (state `glass`).
  4. Optionally orbit yaw ±0.3 to check ordering holds (no popping) from another angle.

  An orange OPAQUE box behind three blend-mode glass panes (red/green/blue,
  base_color alpha 0.35) staggered in depth. Transparent-over-opaque ordering.

expect:
  - Through-glass tints compose in DEPTH ORDER: the orange box reads yellow-ish
    where seen through the green pane; where panes overlap they tint each other
    (green+blue → teal/cyan region).
  - The opaque box and floor are unaffected OUTSIDE the panes (correct opaque
    render, glass only tints what's behind it).
  - Panes are translucent (you see through them), soft, no hard edges.
  - Status shows 4 materials / 4 buckets (box + 3 glass).

fail:
  - A pane rendering opaque (alpha ignored) or fully invisible.
  - Wrong compositing order — a near pane's tint missing over a far one, or the
    box not tinted through the glass.
  - Popping / z-fighting flicker as the camera orbits (sort order unstable).
  - The opaque box or floor tinted outside the pane footprints.
