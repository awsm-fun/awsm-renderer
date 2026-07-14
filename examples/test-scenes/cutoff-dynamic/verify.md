# verify: cutoff-dynamic

drive:
  1. Replay `examples/test-scenes/cutoff-dynamic/author.js` (custom-WGSL material
     needs authoring: `add_custom_material` → layout → shade WGSL → alpha WGSL →
     `set_custom_material_alpha_mode {mode:{mask:{cutoff:0.5}}}` → double-sided →
     `register_material`; assert `material_diagnostics {ok:true, registered:true}`).
     (Or `load_project_from_url {base_url: http://localhost:9084/cutoff-dynamic/project}`.)
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`;
     `set_camera_orbit {yaw:0.25, pitch:0.2, radius:8, look_at:[0,1.6,0]}`;
     `wait_render_settled`; screenshot (state `cutout`).

  An upright orange panel whose alpha is a CUSTOM WGSL fragment
  (`custom_alpha_dynamic` returns an f32) computing a 5×5 grid of circular discs;
  Mask{cutoff 0.5} alpha-tests it → the discs are punched out as holes.

expect:
  - A grid (5×5) of HARD-EDGED circular HOLES through the panel — the sky/floor
    is visible through each hole, crisp on/off edges (NOT a soft fade). Driven by
    the custom alpha WGSL, no texture involved.
  - The panel body is opaque orange (tint uniform 0.9,0.35,0.15), lit by the
    custom shade fragment.
  - Double-sided: the back face shows through the holes (no culled black holes).
  - The panel casts a HOLE-PUNCHED shadow on the floor — the circular cutouts
    appear as light discs in the shadow (masked alpha respected in the shadow pass).
  - Status: custom material registered (2 materials / 2 buckets), no black/pink
    error surface.

fail:
  - Soft/anti-aliased alpha fade instead of hard holes (Mask treated as Blend, or
    cutoff not applied).
  - No holes / solid panel (alpha WGSL not compiled into the masked variant).
  - Solid rectangular shadow (cutout alpha not sampled in the shadow pass).
  - Back-face culled black holes (double-sided flag lost).
  - Material fails to register (naga compose error) → black/pink panel.
