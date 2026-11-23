# verify: taa-vertex-hook (TAA harness scene — numbers, not eyes)

`wave`: a 3 × 1.5 m plane (96 × 24 segments) at 0.5 m whose ONLY motion is a
time-driven custom vertex hook (`wgsl_vertex`: y += 0.15·sin(4π·x − 4·t), with
the analytic normal). Still camera.

drive:
  `task test-scenes` (:9084) and the TAA harness (examples/taa-tests,
  `trunk serve --port 9093`, capture store on :9095; see its README).
  Re-baked bundles: serve uncached (`python3 examples/taa-tests/capture-server.py
  examples/test-scenes 9099`) and add `&bundles=http://localhost:9099`.
  Run: `?mode=vectors&scenes=taa-vertex-hook` (exact facet model), `?mode=aa-toggle` (none → TAA → MSAA → TAA with commit_load).
  Look: `?mode=baseline-record&set=scene-preview&scenes=taa-vertex-hook&aa=msaa&frames=0,60,150`
  stores frames under the capture store's `scene-preview/`.

expect:
- an orange plane with travelling crests; frames 20 and 35 differ inside it;
  every AA toggle leaves it displaced (aa-toggle 5/5).

fail:
- a flat plane (hook not compiled into the active geometry variant).
