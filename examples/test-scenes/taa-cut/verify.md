# verify: taa-cut (TAA harness scene — numbers, not eyes)

Three authored cameras `cam-a`, `cam-b`, `cam-c`; the harness cuts between
them every 2 s (frames 120, 240, with reset_temporal_history) and teleports
`teleporter` (0.8 m magenta box) between three spots every second (60, 120, …).

drive:
  `task test-scenes` (:9084) and the TAA harness (examples/taa-tests,
  `trunk serve --port 9093`, capture store on :9095; see its README).
  Re-baked bundles: serve uncached (`python3 examples/taa-tests/capture-server.py
  examples/test-scenes 9099`) and add `&bundles=http://localhost:9099`.
  Run: `?mode=gate&gates=cut&scenes=taa-cut` (phase 2+); `?mode=vectors&scenes=taa-cut`.
  Look: `?mode=baseline-record&set=scene-preview&scenes=taa-cut&aa=msaa&frames=0,60,150`
  stores frames under the capture store's `scene-preview/`.

expect:
- three clearly different viewpoints at k0, k130, k250; the magenta box jumps
  each second.

fail:
- a camera missing (the script silently keeps the previous one).
