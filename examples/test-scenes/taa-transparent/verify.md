# verify: taa-transparent (TAA harness scene — numbers, not eyes)

No-vector content over a moving background: blended particle fountain, glass
sphere (transmission), UV-flowing noise "water", a green zig-zag fat line;
`backdrop` (4 × 3 × 0.1 m checker box) slides 2 m right and back 30–209.

drive:
  `task test-scenes` (:9084) and the TAA harness (examples/taa-tests,
  `trunk serve --port 9093`, capture store on :9095; see its README).
  Re-baked bundles: serve uncached (`python3 examples/taa-tests/capture-server.py
  examples/test-scenes 9099`) and add `&bundles=http://localhost:9099`.
  Run: `?mode=gate&gates=trail&scenes=taa-transparent` (phase 3 reactive gate, 05 §4.4).
  Look: `?mode=baseline-record&set=scene-preview&scenes=taa-transparent&aa=msaa&frames=0,60,150`
  stores frames under the capture store's `scene-preview/`.

expect:
- k60: backdrop moving behind the glass/water/particles/line; water texture
  scrolls; particles arc out of the fountain.

fail:
- particles absent (emitter not ticked), water static, line missing.
