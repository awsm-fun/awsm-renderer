# verify: mujoco-flex

A MuJoCo **deformable** through the whole pipeline, with **no simulator in the
loop**. Same shape as `mujoco-capture`, one id space over: a sidecar from
`awsm-renderer-mujoco-export` plus a capture from `awsm-renderer-mujoco-record`,
both checked in under `fixtures/`, so replaying this is deterministic and CI
never runs physics.

The difference from `mujoco-capture` is what drives it. A flex is not moved by
geom poses — it imports as a **skinned mesh** whose joints are the bodies its
cage rides, so the capture carries a **body channel** and the bake keys those
joint nodes. MuJoCo's `flag.xml` is a body-attached flex: 171 bodies, one per
cloth vertex, one influence each.

This scene exists because both flex bugs found so far produced **silently wrong
geometry rather than errors**, and nothing would have caught a regression:

- `reexport_clean` dropped `JOINTS_1`/`WEIGHTS_1`, so a trilinear flex kept four
  of eight cage corners, its weights summed to ~0.5, and the surface collapsed
  toward its joints — visible even at the bind pose;
- a skinned mesh's world AABB described its **bind** pose, so a deformed flag
  left that box and was frustum culled — drawn as nothing at all.

drive (replay author.js — the baked clip targets import-minted node ids, so a
loaded project can't re-drive it; see memory `animation-scenes-need-authorjs-replay`):
  1. Serve the suite: `task test-scenes` (:9084). The fixtures are fetched from
     `http://localhost:9084/mujoco-flex/fixtures/`.
  2. Replay `examples/test-scenes/mujoco-flex/author.js`: `new_project` →
     `import_mujoco_from_url {sidecar_url: .../Flag.mujoco.json}` (poll the
     snapshot until a root named `Flag` has children) → `import_mujoco_capture
     {capture_url: .../Flag.capture.json, instance: <that root>, name: "flag
     flap"}` → a red double-sided cloth material on the flex node →
     `set_playhead {t: 1.1}`.
  3. The camera is set by the script (`yaw 2.5, pitch 0.5, radius 1.35`, looking
     down on the cloth — this wind blows the flag nearly horizontal, so an
     edge-on view shows a sliver and proves nothing). `wait_render_settled`,
     then screenshot.

expect:
  - A red cloth **curled in mid-air**, roughly an S/saddle fold: one lobe
    sweeping down-left, another rising to a point top-centre. Compare against
    `golden.png`.
  - The lit side reads red and the away-facing side reads **near-black**, so the
    fold direction is legible — not just the silhouette. (This two-tone is the
    *editor's* live shading of a double-sided material; the same mesh in the
    exported bundle lights its back faces instead. Both are deterministic; only
    the authored path is what this golden documents.)
  - The outliner: one `Flag` instance root over a `floor` geom node, **171
    `flag joint N` nodes**, and one `flag` node of kind **`skinned_mesh`**.
    Every one of them **locked** (the stream owns those transforms).
  - `snapshot.animation.clips` has one clip, `flag flap`, ~1.98 s, **342 tracks**
    — 171 bodies x (translation + rotation). The static floor geom gets none.

fail:
  - **A flat rectangle.** The cloth is at its bind pose, so the clip is not
    driving the joints — the body channel was lost between capture and bake.
  - **Nothing drawn at all** (an empty floor). The classic AABB bug: the skinned
    mesh's world bounds still describe its bind pose and it got frustum culled.
    Orbit the camera; if the cloth pops in and out, that is this.
  - **A shrunken or crumpled knot** near the middle of the cloth. Weights no
    longer sum to 1 — an influence set was dropped on export, so every vertex is
    dragged a fraction of the way toward its joints.
  - Magenta — the cloth lost its material assignment.
  - The cloth lying flat ON the ground plane, or standing vertically edge-on to
    the camera — the Z-up→Y-up convention rotation on the instance root is
    missing or doubled.

## Also checked in

`bundle/` is the same scene exported as a player bundle, and it is the half of
the chain the golden cannot see. Its rig GLB must still carry `skin`,
`JOINTS_0`/`WEIGHTS_0` and its 171 joint nodes; loading it (`load_player_bundle`)
must resolve a sim instance reporting **171 bound bodies** and a
`body_frame_len` of 1211 (173 bodies x 7). Driving that loaded instance through
`editor_apply_mujoco_bodies` with a frame from `Flag.capture.json` must produce
the same curl as the golden — that is the live-simulator path a player takes,
with the editor standing in for the sim.
