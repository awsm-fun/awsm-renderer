# verify: dynamic-material-textures

drive:
  1. Replay `examples/test-scenes/dynamic-material-textures/author.js` (imports the
     Duck albedo `DuckCM.png`, then a custom material: `add_custom_material` →
     layout `textures:[{name:'tex',ty:'texture_2d<f32>',color_kind:'albedo'}]` →
     `set_custom_material_shader_includes {includes:['textures']}` →
     `set_custom_material_fragment_inputs {inputs:['uv']}` → shade WGSL sampling
     `material_sample_tex(input.material, material_uv(input, 0u))` →
     `register_material`; assert `material_diagnostics {ok:true, registered:true}`;
     then `set_material_texture {node, slot:'tex', texture}` binds the instance).
     (Or `load_project_from_url {base_url: http://localhost:9084/dynamic-material-textures/project}`.)
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`;
     `set_camera_orbit {yaw:0.2, pitch:0.15, radius:7, look_at:[0,1.7,0]}`;
     `wait_render_settled`; screenshot (state `textured`).

  An upright panel whose custom-WGSL material SAMPLES A BOUND TEXTURE via the
  dynamic-material texture-slot path — the generated `material_sample_tex`
  sampler reading the mesh's own UV0 through the `textures` include. Distinct
  from `dynamic-materials` (procedural, no texture) and from a builtin
  `base_color_texture` (this is the custom-material slot path).

expect:
  - The Duck albedo renders across the panel: a YELLOW/gold body, a BLACK eye
    with a WHITE highlight (lower-left), and an ORANGE beak patch (upper-left) —
    sharp, not blurred, and in correct sRGB color (the `albedo` color_kind
    applies the sRGB decode; a data-map decode would wash the yellows pale/dark).
  - A soft wrap-diffuse gradient over the duck (the shade term), not flat unlit.
  - The panel is a single opaque quad, double-sided (no culled black back-face).
  - Status: custom material registered (1 material / 1 bucket), no black/pink
    error surface.

fail:
  - A flat untextured panel (tint only) — texture slot not bound / not sampled.
  - Wrong colors: washed-out or over-dark yellows ⇒ the texture uploaded in the
    wrong color space (color_kind not honored → sRGB/linear mismatch).
  - Garbled / mis-scaled duck ⇒ `material_uv(input, 0u)` not reading UV0 (the
    `textures` include or the `uv` fragment input dropped).
  - Black/pink panel ⇒ material failed to register (naga compose error), or
    `material_sample_tex` unresolved (the generated sampler needs the slot in the
    layout).
