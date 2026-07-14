# verify: decals

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/decals/project}`; wait ~3.5s; `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}` (the green decal-VOLUME wireframe still shows — it is volume viz, not a gizmo).
  3. `set_camera_orbit {yaw:0.85, pitch:0.5, radius:8, look_at:[0.2,0.4,0.2]}`; `wait_render_settled`; screenshot (state `projected`).
  4. Move check: `set_transform` the `decal-checker` node by a small translation,
     re-settle, screenshot — the projected texture must MOVE with the volume.

  The AlphaBlendModeTest label sheet projected DOWN through a rotated decal
  volume that straddles a box edge, over floor + box. Needs the `decals`
  feature (the editor build has it).

expect:
  - The label texture WRAPS both the FLOOR and the BOX inside the (green
    wireframe) decal volume — the stripe/label content appears projected onto
    both surfaces, continuing across the box edge.
  - Alpha cutouts respected: the decal shows only where the sheet has opaque
    texels (transparent regions let the underlying surface through).
  - NOTHING projected on the skybox or outside the volume bounds.
  - (move) translating the decal node moves the projected texture on the geometry.

fail:
  - No decal visible (feature off / projection broken) — plain floor + box only.
  - Decal bleeding onto the skybox or beyond the volume.
  - Alpha ignored (a solid rectangle projected instead of the cutout label).
  - Decal frozen when the node moves (projection not following the volume).
