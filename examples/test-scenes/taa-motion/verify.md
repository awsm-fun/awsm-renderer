# verify: taa-motion (TAA harness scene — numbers, not eyes)

drive:
  1. `task test-scenes` (:9084), the capture store
     (`python3 examples/taa-tests/capture-server.py <dir>` on :9095) and the
     harness (`cd examples/taa-tests && trunk serve --port 9093`).
  2. Open `http://localhost:9093/?mode=determinism&scenes=taa-motion&aa=none,msaa&frames=0,45,150,240`
     and wait for `#hud` = `taa-tests: COMPLETE …`.
  3. Reference frames: `?mode=reference&scenes=taa-motion&k=29,90,180`.
  4. The 05 §4 gates (trail, stop, motion-vector) run against this scene from
     the gate modes as they land (see examples/taa-tests/README.md).

expect:
  - Frame 0: the orange checker cube left of centre on a black/white checker
    floor, seen straight on from slightly above; nothing moves for frames 0–29.
  - Frames 30–149: the cube slides right (~3.3 px/frame) while turning;
    150–209 still; 210–269 it slides back left fast (~8.7 px/frame).
  - Determinism: every capture bit-identical across runs (max error 0).

fail:
  - Any non-zero determinism error, the cube leaving the frame, or the cube
    not where the scenario puts it (a renamed / resized `cube` node breaks the
    analytic coverage mask the trail test uses).
