# verify: dynamic-material-attributes

drive:
  1. Replay `examples/test-scenes/dynamic-material-attributes/author.js` (a custom
     material: `add_custom_material` → layout with ONE uniform `ambient` →
     `set_custom_material_shader_includes {includes:['vertex_color']}` →
     `set_custom_material_fragment_inputs {inputs:['normals','vertex_color']}` →
     shade WGSL reading `material_vertex_color(input, 0u)` → `register_material`;
     assert `material_diagnostics {ok:true, registered:true}`. Then ONE instancer
     over the box mesh, the custom material set as the instancer's single
     `material` via `patch_kind {instancer: {material: {asset: ..}}}` (an
     instancer has no variant palette — `add_material_variant` on it errors),
     and `set_instancer_transforms` with a rainbow `per_instance_colors`
     array of 12).
     (Or `load_project_from_url {base_url: http://localhost:9084/dynamic-material-attributes/project}`
     — note the bundle carries no per-instance colors table unless re-driven; prefer replay.)
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`;
     `set_camera_orbit {yaw:0.18, pitch:0.55, radius:17.5, look_at:[0,0.4,0]}`;
     `wait_render_settled`; screenshot (state `rainbow`).

  Twelve box instances (a 3×4 grid) from ONE instancer sharing ONE custom
  material with ONE uniform. Each instance's color arrives as PER-INSTANCE
  ATTRIBUTE data (`per_instance_colors` → vertex-color channel 0), read by the
  shader via `material_vertex_color(input, 0u)`. This is the attribute path —
  contrast `dynamic-materials` (per-instance UNIFORM override) and
  `dynamic-material-textures` (texture slot).

expect:
  - A 3×4 grid of 12 boxes, each a DISTINCT color spanning a full rainbow
    (index 0→11 sweeps magenta → purple → blue → cyan → green → lime → yellow →
    orange) — the per-instance colors, driven by attribute data. No two boxes
    share a color.
  - The material has only ONE uniform (`ambient`), identical for every instance,
    so it CANNOT be the source of the divergence — the rainbow proves the color
    comes from the per-instance vertex-color channel.
  - A subtle directional shading gradient across each box's faces (the `diff`
    term), i.e. lit, not flat.
  - Status: custom material registered (2 materials / 2 buckets, incl. the floor),
    one instancer node (not 12 scene nodes), no black/pink error surface.

fail:
  - All 12 boxes the SAME color ⇒ `material_vertex_color` not reading the
    per-instance channel (attribute path broken — colors came from the uniform,
    or `per_instance_colors` not uploaded, or the `vertex_color` include/input
    dropped).
  - Boxes black/pink ⇒ material failed to register (naga compose error), or
    `material_vertex_color` unresolved (needs the `vertex_color` include).
  - Only one box (instancer not expanding the 12 transforms).
