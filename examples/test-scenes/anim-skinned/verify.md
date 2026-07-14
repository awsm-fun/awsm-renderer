# verify: anim-skinned

drive (persisted-pose path — fast):
  1. `load_project_from_url {base_url: http://localhost:9084/anim-skinned/project}` (saved project carries rig.glb + bake side files — the skinned persistence path); wait ~4s; `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false, skeleton_viz:false}`.
  3. `set_camera_orbit {yaw:0.5, pitch:0.25, radius:4, look_at:[0,0.9,0]}`; `wait_render_settled`; screenshot (state `persisted-stride`).
  Note: the saved project restores the joint TRS at the authored frame (~0.9s of
  the ~1.96s walk clip). `set_frame_time` does NOT re-scrub a *loaded* project —
  the clip's tracks target import-minted node ids that change on save/load, so
  the restored pose is what renders. Use the replay path below to prove live
  clip evaluation.

drive (live-clip path — replay author.js, needs media server :9082):
  1. Replay `examples/test-scenes/anim-skinned/author.js` (fresh
     `import_model_from_url` CesiumMan → `set_current_clip` → `set_frame_time`).
  2. Screenshot at `set_frame_time {seconds:0.9}` (state `stride-a`), then at
     `set_frame_time {seconds:0.3}` (state `stride-b`) — the two leg splits must
     differ, proving the clip evaluates per frame_time.

expect:
  - A clear WALKING pose: legs split fore/aft (one leg forward, one trailing),
    arms swung — NOT a symmetric T-pose or A-pose.
  - The mesh deforms smoothly with the skeleton: limbs bend at joints, no
    "candy-wrapper" pinch/collapse at any joint (elbow/knee/hip), no exploded or
    detached vertices.
  - Textured with the blue/green CesiumMan pattern (base color texture bound —
    survived the rig.glb roundtrip).
  - Casts a soft contact shadow on the floor.
  - (live path) `stride-a` and `stride-b` poses differ.

fail:
  - A rigid T-pose / A-pose (persisted pose lost, or clip not evaluating on replay).
  - Candy-wrapper collapse or pinching at a joint (skinning weights/matrices wrong).
  - Untextured / white body (texture dropped on the rig roundtrip — cf. memory
    `skinned-saveload-rig-glb-roundtrip`).
  - (live path) identical pose at t=0.3 and t=0.9 (frame_time not driving the clip).
