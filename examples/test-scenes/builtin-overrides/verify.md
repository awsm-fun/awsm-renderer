# verify: builtin-overrides

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/builtin-overrides/project}`; wait ~3.5s; `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:0.0, pitch:0.55, radius:11.5, look_at:[0,0.5,0]}`; `wait_render_settled`; screenshot (state `overrides-grid`).

  ONE shared built-in PBR material asset; four spheres each carry their own
  material VARIANT overriding base_color / metallic / roughness / emissive (the
  builtin per-node uniform-override path). The floor shares the same asset.

expect:
  - Four visibly DIFFERENT tunings of the one material in a 2×2 grid:
    - `plastic-red` — saturated red, plastic (low metallic, mid roughness).
    - `metal-gold` — gold metallic (dark body + a bright warm specular highlight).
    - `rough-clay` — cream/beige matte (high roughness, no sharp highlight).
    - `emissive-teal` — glowing cyan/teal (emissive override, reads brighter than lit).
  - Status bar shows **1 material / 1 bucket** — proving they share ONE asset and
    the differences are per-node overrides, not four separate materials.
  - Soft contact shadows under each sphere; no grid/gizmos.

fail:
  - Any two spheres looking identical (an override didn't apply).
  - Status showing 4+ materials (overrides minted separate assets instead of
    sharing one).
  - The emissive sphere not glowing, or the gold sphere flat/non-metallic.
