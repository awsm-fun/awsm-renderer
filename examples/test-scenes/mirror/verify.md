# verify: mirror

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/mirror/project}`; wait ~4s; `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:0.1, pitch:0.12, radius:12, look_at:[0,1.0,-1.2]}` (low + grazing so the reflections dominate the floor); `wait_render_settled`; screenshot (state `mirror`).
  4. Optionally orbit yaw ±0.15 to check the contact lines from a second grazing angle.

  A flat SILVER metallic floor (metallic 1.0, roughness 0.0 → reflection spread
  0 → the spatially-deterministic mirror trace) under emissive probes at varied
  heights: a white sphere, a red box, a THIN torus (thin-geometry acceptance
  case) and a floor-TOUCHING sphere (contact case). SSR full-res, bloom OFF.

expect:
  - Each object's reflection is PIXEL-IDENTICAL IN SHAPE to the object: white
    sphere → clean white circle, red box → clean red rectangle, thin torus →
    continuous torus ring (both reflected across the floor plane).
  - The contact lines where object meets its reflection (sphere/box/torus) are
    clean — no serration/zippering along the mirror line.
  - Reflection interiors are smooth — no stipple / noise / dithered speckle.
  - The thin torus ring reflects UNBROKEN — no dashed gaps through the ring
    (the thin-geometry acceptance case).

fail:
  - Serration / zippering at any contact line.
  - Stipple, noise, or dashed gaps inside a reflection (especially the torus).
  - A reflection wrong-shaped, offset, doubled, or missing.
  - Anything beyond normal 1px rasterization aliasing on the mirror.
