# verify: alpha-cutoff

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/alpha-cutoff/project}`; wait ~3.5s; `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:2.6, pitch:0.35, radius:11, look_at:[0,1.1,0]}`; `wait_render_settled`; screenshot (state `cutouts`).
  4. Optionally orbit to a more head-on angle (lower yaw) to compare mask-025 vs
     mask-075 coverage directly, and to see back faces (double-sided).

  Masked materials at two cutoff values (`mask-025`, `mask-075`) + a `blend-ref`
  pane, all sharing the glTF AlphaBlendModeTest label sheet (a real alpha
  texture) on thin boxes over a gray floor.

expect:
  - HARD-EDGED stripe cutouts on the masked panes — crisp on/off edges, holes
    where the texture alpha is below the cutoff (background/sky shows through).
  - Coverage DIFFERS between cutoff 0.25 and 0.75: MORE of the sheet survives at
    0.25 (lower cutoff keeps more texels) than at 0.75.
  - The `blend-ref` pane shows SMOOTH translucency (soft alpha gradient), not
    hard edges — distinguishing Blend from Mask.
  - The cutouts also appear in the CAST SHADOW on the floor (striped shadow, not
    a solid rectangle) — masked shadow alpha respected.
  - Back faces render (double-sided) — the sheet is visible from behind too.

fail:
  - Soft/anti-aliased edges on the masked panes (mask treated as blend).
  - mask-025 and mask-075 with identical coverage (cutoff value ignored).
  - Solid rectangular shadows (cutout alpha not sampled in the shadow pass).
  - Back faces culled (single-sided) when the material is double-sided.
