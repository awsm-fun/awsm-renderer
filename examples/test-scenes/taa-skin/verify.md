# verify: taa-skin (TAA harness scene — numbers, not eyes)

The Fox (glTF sample) walking in place via its own `Walk` clip, beside the
AnimatedMorphCube whose two morph weights the harness script sets each frame
(the player does not apply morph clips; pre-existing). Placement goes through
the skinned-mesh NODES `fox` / `morph-cube` (the player ignores the
skeleton-root group's transform; pre-existing), so the editor preview differs
from the player: verify in the harness only.

drive:
  `task test-scenes` (:9084) and the TAA harness (examples/taa-tests,
  `trunk serve --port 9093`, capture store on :9095; see its README).
  Re-baked bundles: serve uncached (`python3 examples/taa-tests/capture-server.py
  examples/test-scenes 9099`) and add `&bundles=http://localhost:9099`.
  Run: `?mode=vectors&scenes=taa-skin` (skin + morph vectors; SS reprojection + per-object scale sweep).
  Look: `?mode=baseline-record&set=scene-preview&scenes=taa-skin&aa=msaa&frames=0,60,150`
  stores frames under the capture store's `scene-preview/`.

expect:
- Fox upright, walking (legs move between frames 0 / 20 / 40); the cube visibly
  deforms (TAA-INFO morph-weights line shows weights changing).

fail:
- Fox giant / lying flat (placement on the rig group instead of the mesh node),
  cube static (morph weights 0).
