# verify: dynamic-materials

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/dynamic-materials/project}`; wait ~3.5s; `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:0.3, pitch:0.35, radius:8, look_at:[0,0.9,0]}`; `wait_render_settled`; screenshot (state `two-tints`).
  4. (Optional live-edit) `set_node_material_uniform` on `override-tint`'s tint to
     a new color, re-settle, screenshot — the right sphere re-shades WITHOUT a
     recompile (uniform edit, not a pipeline rebuild).

  ONE registered custom-WGSL material with a `tint: vec3<f32>` uniform, assigned
  to two spheres: `shared-tint` reads the shared default (blue,
  `set_material_uniform`); `override-tint` carries a PER-INSTANCE override
  (orange, `set_node_material_uniform`).

expect:
  - Both spheres shaded by the SAME custom lambert fragment (matte, view-independent
    diffuse falloff — not the built-in PBR look), each casting a soft shadow.
  - Left sphere BLUE (shared default tint); right sphere ORANGE (per-instance
    override) — the per-node uniform visibly diverges from the shared default.
  - Status bar shows 2 materials / 2 buckets (custom material + floor).
  - (live-edit) the override sphere re-shades to the new tint with no recompile hitch.

fail:
  - Both spheres the same color (per-instance override not applied — the
    node-material-uniform path is broken).
  - The spheres showing built-in PBR shading instead of the custom lambert
    fragment (custom material not restored from the project).
  - Either sphere untextured/black, or a recompile/flicker on a live uniform edit.
