# verify: taa-hud (TAA harness scene — numbers, not eyes)

World shapes on a checker floor plus two HUD meshes (`hud-panel`,
`hud-marker`) placed 1.5 m in front of the authored camera. HUD is a runtime
flag: the harness calls set_mesh_hud on them, keeps them attached to the
camera, and orbits the camera 30°/s over frames 30–149.

drive:
  `task test-scenes` (:9084) and the TAA harness (examples/taa-tests,
  `trunk serve --port 9093`, capture store on :9095; see its README).
  Re-baked bundles: serve uncached (`python3 examples/taa-tests/capture-server.py
  examples/test-scenes 9099`) and add `&bundles=http://localhost:9099`.
  Run: `?mode=gate&gates=hud&scenes=taa-hud` (phase 3, 05 §4.4) once it exists; `?mode=vectors&scenes=taa-hud`.
  Look: `?mode=baseline-record&set=scene-preview&scenes=taa-hud&aa=msaa&frames=0,60,150`
  stores frames under the capture store's `scene-preview/`.

expect:
- the dark panel lower-left and the yellow emissive marker upper-right stay
  fixed on screen while the world orbits.

fail:
- HUD drifting with the world (not re-attached) or depth-fighting the world.
