# MuJoCo — remaining gaps

The feature is complete and verified by hand. It is **not release-hardened**.
This doc is only the gaps; how the feature works lives in `docs/mujoco.md` and
should stay there.

Branch `mujoco`, never pushed, `main` untouched. Templates work is in
`../templates/physics-mujoco`, its own repo, also unpushed.

## Already verified — do not redo

Deformable flexes went in after the permanent doc was written, so this is the
part most likely to be re-litigated:

- A flex deforms by **linear blend skinning exactly** — zero error at nanometre
  resolution against MuJoCo's `flexvert_xpos` across a real simulation
  (`packages/tools/mujoco-export-cli/tests/flex_skin.rs`). Body-attached flexes
  are one influence per vertex; trilinear ones are eight, and the weights come
  straight out of `flex_vert0`, which already holds each vertex's normalized
  coordinates inside its cage cell.
- The exported GLB reproduces MuJoCo to 0.0001 mm after 500 steps, read back
  through the real extractor.
- On-device, all four of MuJoCo's `model/flex/` demos import as skinned meshes:
  **1047 joints bound (171 + 484 + 384 + 8)**, console clean.
- Round-trips: save → load (rig glb persists), `export_player_bundle` (carries
  `JOINTS_0/1` + `WEIGHTS_0/1`), `load_player_bundle`, and `apply_body_poses`
  deforming the bunny inside that loaded bundle.

Commits: `e02e47be` `161ab883` `47ce3900` `93654308` `dcb43f7e` `b3005cd2`
`73fd3922` `6a875af5` `ab9cbb48`.

## Gap 1 — the reference template does not drive flexes (highest value)

`physics-mujoco` publishes geom poses only. Nothing in the worked example a
player copies demonstrates a deformable, which matters because this plan's
position has always been that **the template is the integration test**.

What it needs:

- `protocol.rs`: a body region — `nbody × 7`, same shape as the pose region.
  Sizes follow `pose_block_bytes`, which already takes `nbody`.
- `web/workers/mujoco-worker.js`: publish `data.xpos` + `data.xquat` per body
  inside the existing seqlock. `matToPose` already generalises — it takes a
  target array and offset.
- `render_thread.rs`: call `awsm_renderer_scene_loader::mujoco::apply_body_poses`
  next to `apply_geom_poses`. `SimLink` needs the body region view + scratch.
- **The template's bundle has no flex in it.** `media/mujoco/humanoid.xml` drives
  the scene, and the humanoid has no deformables. Either add a second scene
  (`model/flex/flag.xml` is the clearest — a cloth visibly waving is the whole
  point) or swap the demo. This is the real work in this gap; the wiring above is
  small.

Closing this also closes Gap 2, which is why it goes first.

## Gap 2 — no automated coverage for flexes

Everything in "already verified" was done by hand in one session. Nothing would
catch a regression. That is the wrong risk profile for this specific feature,
because **both bugs found tonight produced silently wrong geometry, not errors**:

- `reexport_clean` dropped `JOINTS_1`/`WEIGHTS_1`, so a trilinear flex kept four
  of eight cage corners with weights summing to ~0.5 and collapsed toward them.
- A skinned mesh's world AABB described its bind pose, so the flag was frustum
  culled and simply never drew.

What exists: `flex_skin.rs` covers the export maths and the artifact. What does
not: the editor import → bundle → sink chain, which is where both bugs lived.

Suggested: a `examples/test-scenes/` scene with a flex plus a golden, following
`mujoco-capture`'s shape (checked-in fixtures, no simulator in the loop). A
golden image would have caught both bugs immediately.

## Gap 3 — file the parry3d issue

`parry3d-0.28.0/src/partitioning/bvh/bvh_binned_build.rs:60` computes
`(k1 * (centroid - min)) as usize` and indexes an 8-bin array with it, with no
clamp. `k1`'s `(1.0 - 1e-5)` factor is the only guard.

Measured: for any well-formed leaf set the value tops out at exactly `7.999920`
— coordinates to 1e7 with extents to 1e-4 all hold, and a zero extent yields
`NaN`, which casts to bin 0. So an out-of-bounds index means malformed input,
not drifting arithmetic. A one-line `.min(NUM_BINS - 1)` makes it unreachable.

We saw the panic once (`index out of bounds: the len is 8 but the index is 8`),
never reproduced. `dde3bf98` added a `debug_assert` at `to_parry_aabb` — the
single funnel into parry — so next time the malformed box gets named instead of
parry taking the blame.

## Gap 4 — two decisions to ratify

- **Body-attached flexes cost ~5× GLB size** (poncho 23 → 125 KB) because one
  joint node per vertex is inherently redundant when the mapping is the identity
  and glTF has no cheaper way to say it. Absolute sizes stay small. Interpolated
  flexes — the ones that matter at runtime — cost 2× and save far more: eight
  joint matrices per frame instead of 2503 vertex positions. Nobody has decided
  this is acceptable.
- **Quadratic cages are refused loudly** rather than approximated: they would
  need 27 influences (seven joint sets). `quadratic.xml` and
  `bunny_quadratic.xml` exist in `model/flex/`, so the case is real but
  unexercised.

## Harness gotchas worth keeping

- `load_player_bundle` resolves no instance on a **fresh page** — it needs a
  project loaded first. This is the editor's test seam, not the player path (a
  non-flex bundle fails identically), but it will waste an hour if rediscovered
  as a suspected flex bug.
- The editor's trunk watcher frequently produces a new wasm *before* a naive
  baseline snapshot is taken, so "no rebuild" loops lie. Compare the wasm's mtime
  against the source's instead.
- For the templates repo use `task dev`; hand-rolled RUSTFLAGS break the threaded
  build (`-Z build-std` recompiles `std` with atomics) and trunk then serves a
  stale `dist/` while every rebuild fails silently.
