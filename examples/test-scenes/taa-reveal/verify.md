# verify: taa-reveal (TAA harness scene — numbers, not eyes)

A 3 × 2.5 × 0.2 m dark wall (`wall`) in front of a fine checker backdrop; the
harness slides it 3 m right over frames 30–89, holds, slides back 120–179, holds.

drive:
  `task test-scenes` (:9084) and the TAA harness (examples/taa-tests,
  `trunk serve --port 9093`, capture store on :9095; see its README).
  Re-baked bundles: serve uncached (`python3 examples/taa-tests/capture-server.py
  examples/test-scenes 9099`) and add `&bundles=http://localhost:9099`.
  Run: `?mode=vectors&scenes=taa-reveal`; `?mode=gate&gates=trail&scenes=taa-reveal` (phase 2+).
  Look: `?mode=baseline-record&set=scene-preview&scenes=taa-reveal&aa=msaa&frames=0,60,150`
  stores frames under the capture store's `scene-preview/`.

expect:
- k0 the wall covers the left-centre of the backdrop; k100 the backdrop is fully
  revealed; k200 the wall is back.

fail:
- `wall` resized/renamed (the trail-band hull assumes the 3 × 2.5 × 0.2 box).
