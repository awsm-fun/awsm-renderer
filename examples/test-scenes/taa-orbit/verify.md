# verify: taa-orbit (TAA harness scene — numbers, not eyes)

Static textured shapes around `pivot` on a checker floor; the harness moves
the authored `camera`: still 0–29, orbit 30°/s about `pivot` 30–149, strafe
right at 1 px/frame (at the pivot depth) 150–239, still 240–299.

drive:
  `task test-scenes` (:9084) and the TAA harness (examples/taa-tests,
  `trunk serve --port 9093`, capture store on :9095; see its README).
  Re-baked bundles: serve uncached (`python3 examples/taa-tests/capture-server.py
  examples/test-scenes 9099`) and add `&bundles=http://localhost:9099`.
  Run: `?mode=vectors&scenes=taa-orbit` (camera vectors, analytic), `?mode=gate&gates=pan,stop&scenes=taa-orbit` (phase 2+).
  Look: `?mode=baseline-record&set=scene-preview&scenes=taa-orbit&aa=msaa&frames=0,60,150`
  stores frames under the capture store's `scene-preview/`.

expect:
- k0: six coloured checker shapes around the centre, nothing clipped by the frame.
- Determinism (`?mode=determinism&scenes=taa-orbit`) bit-exact; analytic vectors ≤ 0.02 px.

fail:
- shapes leaving the frame during the orbit; a renamed `pivot`/`camera` node
  (the script falls back to the origin / first camera).
