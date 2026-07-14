# verify: dynamic-materials

drive (replay author.js — the per-node texture overrides need the two textures
imported + bound; load_project also works but replay is the reference path):
  1. Replay `examples/test-scenes/dynamic-materials/author.js` (imports two
     textures, registers ONE custom material with a `tint` uniform + a `tex`
     slot, assigns it to two spheres, then applies per-node overrides: a uniform
     override on the right sphere AND a different bound texture on each).
     (Or `load_project_from_url {base_url: http://localhost:9084/dynamic-materials/project}`;
     wait ~3.5s; `wait_render_settled`.)
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:0.22, pitch:0.3, radius:10.5, look_at:[0,1.0,0]}`;
     `wait_render_settled`; screenshot (state `two-overrides`).
  4. (Optional live-edit) `set_node_material_uniform` on `override-tex`'s tint to
     a new color, re-settle, screenshot — the right sphere re-shades WITHOUT a
     recompile (uniform edit, not a pipeline rebuild).

  ONE registered custom-WGSL material (a `tint: vec3<f32>` uniform + a `tex`
  texture slot, sampled by the custom lambert fragment), assigned to two spheres.
  Two INDEPENDENT per-node override kinds are exercised on the same material:
  - UNIFORM override: `shared-tex` reads the shared default tint (cool,
    `set_material_uniform`); `override-tex` carries a per-instance override (warm,
    `set_node_material_uniform`).
  - TEXTURE override: `set_material_texture {node, slot, texture}` binds the
    Cesium logo to the LEFT sphere and the Duck albedo to the RIGHT — per-node
    (it writes the instance's `texture_overrides` map, not the shared material).

expect:
  - Both spheres shaded by the SAME custom lambert fragment (matte, view-independent
    diffuse falloff — not the built-in PBR look), each casting a soft shadow.
  - LEFT sphere shows the CESIUM LOGO texture (blue/white/green swoosh) under a
    COOL tint; RIGHT sphere shows the DUCK albedo (yellow/gold) under a WARM tint.
    The two spheres diverge in BOTH texture AND tint — two distinct per-node
    overrides on one shared material.
  - Textures read sharp and in correct sRGB color (the `tex` slot's `albedo`
    color_kind applies the sRGB decode).
  - Status bar shows 2 materials / 2 buckets (custom material + floor).
  - (live-edit) the override sphere re-shades to the new tint with no recompile hitch.

fail:
  - Both spheres the SAME texture (per-node texture override not applied — the
    `set_material_texture` / `texture_overrides` path is broken).
  - Both spheres the same tint (per-instance uniform override not applied).
  - The spheres showing built-in PBR shading instead of the custom lambert
    fragment (custom material not restored from the project).
  - Either sphere untextured/black (texture slot unbound or `material_sample_tex`
    unresolved), or washed/dark colors (wrong color space), or a recompile/flicker
    on a live uniform edit.
