# verify: taa-cluster (TAA harness scene, cluster-LOD motion vectors)

drive:
  `?mode=vectors&scenes=taa-cluster` on the TAA harness (examples/taa-tests).
expect:
  - `vectors-lod-switches:taa-cluster` lists frames where the GPU cluster cut
    (read back from the cluster pass's `selected` flags) changes during the dolly;
  - `vectors-lod-switch:*` PASS (the cut differs between k−1 and k), and the
    analytic / G2 / reprojection lines at those frames PASS.
fail:
  - cut signature `None` (cluster path not running: check the features), or any
    vector line failing across a cut change.
