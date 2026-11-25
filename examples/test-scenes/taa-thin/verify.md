# verify: taa-thin (TAA harness scene — numbers, not eyes)

Thin-wire fence (1.5 cm posts), alpha-masked leaf cards, a fine checker floor to
a grazing angle (moiré), a clearcoat box `spinner`. Static 0–119 (convergence,
flicker), spinner turns 180°/s 120–209, camera pans 0.5 px/frame 210–299.

drive:
  `task test-scenes` (:9084) and the TAA harness (examples/taa-tests,
  `trunk serve --port 9093`, capture store on :9095; see its README).
  Re-baked bundles: serve uncached (`python3 examples/taa-tests/capture-server.py
  examples/test-scenes 9099`) and add `&bundles=http://localhost:9099`.
  Run: `?mode=gate&gates=converged,flicker&scenes=taa-thin` (phase 2+), `?mode=vectors&scenes=taa-thin`.
  Look: `?mode=baseline-record&set=scene-preview&scenes=taa-thin&aa=msaa&frames=0,60,150`
  stores frames under the capture store's `scene-preview/`.

expect:
- posts ~1.5 px wide, leaf cards with holes, checker shimmering in the distance
  under no-AA; spinner a red clearcoat cube right of the fence.

fail:
- leaf cards opaque squares (mask cutoff / texture import failed), spinner moving
  during the static phase.
