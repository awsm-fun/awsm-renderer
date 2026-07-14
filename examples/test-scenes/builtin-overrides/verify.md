# verify: builtin-overrides

drive (replay author.js — the per-node texture override needs the Cesium logo
imported + bound; load_project also works):
  1. Replay `examples/test-scenes/builtin-overrides/author.js` (imports the
     Cesium logo, ONE shared PBR material asset, four sphere variants; the first
     sphere gets a per-node `set_builtin_texture {slot:'base_color'}` override).
     (Or `load_project_from_url {base_url: http://localhost:9084/builtin-overrides/project}`;
     wait ~3.5s; `wait_render_settled`.)
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:0.0, pitch:0.55, radius:11.5, look_at:[0,0.5,0]}`; `wait_render_settled`; screenshot (state `overrides-grid`).

  ONE shared built-in PBR material asset; four spheres each carry their own
  material VARIANT. Three override scalar params (base_color / metallic /
  roughness / emissive — the builtin per-node uniform-override path); the FOURTH
  override kind is a per-node TEXTURE: the top-left sphere binds the Cesium logo
  to its inline material's `base_color` slot via `set_builtin_texture`, while the
  others (same asset) stay flat-tuned. The floor shares the same asset.

expect:
  - Four visibly DIFFERENT tunings of the one material in a 2×2 grid:
    - `textured-logo` (top-left) — the CESIUM LOGO (blue/white/green swoosh)
      wrapped on the sphere: a per-node base_color TEXTURE override, on a sphere
      that shares the same PBR asset as the flat-tuned ones.
    - `metal-gold` (top-right) — gold metallic (dark body + a bright warm
      specular highlight).
    - `rough-clay` (bottom-left) — cream/beige matte (high roughness, no sharp
      highlight).
    - `emissive-teal` (bottom-right) — glowing cyan/teal (emissive override,
      reads brighter than lit).
  - Status bar shows **1 material / 1 bucket** — proving they share ONE asset and
    the differences (including the texture) are per-node overrides, not separate
    materials; the builtin base_color texture rides the same PBR pipeline (no
    extra bucket).
  - The logo reads sharp and in correct sRGB color (base_color texture is sRGB).
  - Soft contact shadows under each sphere; no grid/gizmos.

fail:
  - The top-left sphere flat/untextured (the per-node `set_builtin_texture`
    override didn't apply) or its logo washed/dark (wrong color space).
  - Any two spheres looking identical (an override didn't apply).
  - Status showing 2+ materials or 2+ buckets (overrides minted separate assets,
    or the texture forked a separate pipeline bucket).
  - The emissive sphere not glowing, or the gold sphere flat/non-metallic.
