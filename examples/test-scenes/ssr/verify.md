# verify: ssr

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/ssr/project}`; wait ~4s; `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:0.15, pitch:0.35, radius:14, look_at:[0.5,0.9,-2]}`; `wait_render_settled`; screenshot (state `ssr-on`).
  4. `set_post_process {ssr_enabled:false}` (FLAT field — a nested `ssr:{}` is silently ignored); `wait_render_settled`; screenshot (state `ssr-off`).
  5. Optionally `set_post_process {ssr_enabled:true}` again and orbit yaw ±0.2 to grazing to re-check continuity.

  A BLACK glossy dielectric floor (base 0.02, roughness 0.05, metallic 0 — black
  shows the reflection signal) under three emissive RGB columns at staggered
  depths + a rough gold-metal sphere. The graduated plan-004 LinearDda lock.

expect (ssr-on):
  - Each column reflects CONTINUOUSLY straight down into the floor — an
    unbroken vertical red / green / blue streak beneath its column, no
    horizontal banding / stair-stepping (the LinearDda march lock).
  - The rough gold sphere's reflection below it is BLURRED / spread (roughness →
    reflection spread), not a sharp mirror image.
  - Reflections dim with distance from the object contact but stay coherent.
expect (ssr-off):
  - All reflections GONE — the columns and sphere no longer appear in the floor;
    the floor is a flat dark surface. The scene otherwise unchanged (geometry,
    emissive columns still lit).

fail:
  - Horizontal banding / broken dashes in a column's reflection (LinearDda
    regression).
  - The gold sphere reflection sharp instead of blurred.
  - ssr-off looking identical to ssr-on (toggle not wired / nested-field trap),
    or ssr-on showing NO reflections at all.
  - Reflections leaking above the horizon or onto the sky.
