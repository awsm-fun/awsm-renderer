# todo — camera API consolidation + outstanding branch work

**Branch:** `more-textures` (15 commits ahead of `main`, PR #197 open, v0.24.0).
**State:** `task lint` clean, `cargo test --workspace --all-features` green (61
test binaries). Nothing here is blocking a red build — this is design debt and
unfinished features.

Read this whole file before starting. Task A is a **redesign**, not a patch: it
exists because the camera API was grown one question at a time and now has ~7
overlapping ways to do the same thing. Do not add an eighth.

---

## Task A — camera API consolidation (the big one)

### The problem

The renderer's camera surface accreted. Every one of these currently exists and
overlaps:

| # | API | Where |
|---|---|---|
| 1 | `CameraMatrices { .. }` public-field struct literal | `renderer/src/camera.rs` |
| 2 | `CameraMatrices::perspective(convention, eye, target, up, fov, aspect, near, far)` | same |
| 3 | `CameraMatrices::orthographic(convention, eye, target, up, l, r, b, t, near, far)` | same |
| 4 | `CameraMatrices::from_view(convention, view, pos, projection_params, aspect, near, far)` | same |
| 5 | `AwsmRenderer::update_camera(CameraMatrices)` | same |
| 6 | `AwsmRenderer::set_camera(view, pos, projection, near, far)` | same |
| 7 | `AwsmRenderer::set_perspective_camera(eye, target, up, fov, near, far)` | same |
| 8 | `AwsmRenderer::set_orthographic_camera(eye, target, up, half_height, near, far)` | same |
| 9 | `FreeCamera::matrices()` with its OWN perspective/orthographic sub-types, each with `projection_matrix(convention)` | `web-shared/src/util/free_camera.rs` |
| 10 | model-tests' own `Camera` + `projection/{perspective,orthographic}.rs`, each with `projection_matrix(convention)` | `frontend/model-tests/src/pages/app/scene/camera*` |
| 11 | editor `scene_camera_matrices()` building from a node transform | `frontend/editor/src/engine/render_loop.rs` |

Plus `DepthConvention::{perspective, perspective_finite, orthographic}`
(`renderer/src/depth_convention.rs`) and `CameraProjectionParams`
(`renderer/src/cameras.rs`), which are the two pieces that should have been the
foundation all along.

The ortho half-width derivation (`half_width = half_height * aspect`) is
currently written out in at least three places.

### The design to land

1. **ONE camera module** in the renderer crate exposing helpers for **view**
   matrices and **projection** matrices.
2. **View and projection fully decoupled.** An orbit camera must compose with
   EITHER projection; switching perspective↔orthographic must touch only the
   projection, never the view. Today `free_camera` and model-tests each bundle
   the two together, which is why they both needed bespoke plumbing.
3. **A simple way to set the active camera on `AwsmRenderer`** — one obvious
   call, not three.
4. **Players constructing their own view matrix is FIRST CLASS**, not a
   fallback. It is normal for a game to own its view matrix (its own controller,
   physics-driven, replay, VR) while still using the built-in projection
   helpers. That path must be as short and as safe as the built-in one — in
   particular it must not force the caller to hand-manage the depth convention.
5. **Reverse-Z is the DEFAULT but NOT hardcoded.** `RendererFeatures::default()`
   already sets `reverse_z: true` (an explicit `impl Default`, deliberately not
   derived — keep that). The helpers must keep taking / deriving the convention
   so forward-Z stays selectable; nothing should bake `reverse_z: true` into a
   matrix builder.
6. **Delete the redundant paths.** Collapse the table above. Removing public API
   is fine — no backwards-compatibility constraint on this branch, and migrating
   every consumer is part of the task.
7. **Update EVERY consumer** onto the one system: the 9 `examples/multithreaded`
   demos, `examples/render-worker`, `examples/player-tests`, the editor (both
   the free camera and scene-camera-node paths, including live UI parameter
   edits for fov / near / far / ortho half-height), and the model-tests viewer.

### Invariants that must survive the redesign

These were each a real bug on this branch. Do not regress them.

- **Projection and `reverse_z` cannot disagree.** A mismatch inverts every depth
  test; the symptom (geometry occluded backwards) points nowhere near the
  camera. `CameraBuffer::update` currently logs a one-shot error on mismatch
  (`renderer/src/camera.rs`) — keep an equivalent guard, or make the mismatch
  unrepresentable, which is better.
- **Aspect ratio must come from the live surface**, not from startup constants.
  `player-tests/harness.rs` used to pin it to `CANVAS_WIDTH/HEIGHT`, which is
  wrong the moment the canvas resizes.
- **Reverse-Z ortho is the near/far SWAP** (`DepthConvention::orthographic`).
  Hand-rolled `Mat4::orthographic_rh` gets this wrong.
- **Never unproject NDC z=0 or z=1 and divide by w.** Under infinite-far
  reverse-Z the far plane is at infinity and `w == 0` exactly → `±Inf` → `NaN`.
  This bug was found in THREE places on this branch (`sample_skybox`,
  `material_classify::view_space_depth`, `transform_controller::ray_from_screen`).
  `compute_view_frustum_rays` in `camera.rs` documents the hazard;
  `light_culling` avoids it correctly via `NEAR_NDC_Z`.
- `transform_controller::ray_from_screen` builds its ray in VIEW space and
  rotates it out, specifically to avoid both `w == 0` and world-space
  cancellation far from the origin. It has 4 tests including a sweep over every
  projection `DepthConvention` builds. If the camera module changes shape,
  those tests must keep passing.

### Naming note

`eye / target / up` is a `look_at` convenience, not how a scene camera is
modelled. glam names the middle argument `center`, which reads like a viewport
centre but is the point being looked AT. Whatever the final API, make this
unambiguous.

---

## Task B — `dynamic-material-attributes` is a false-positive test

`examples/test-scenes/dynamic-material-attributes` claims to verify a **custom
WGSL material on an instancer**, reading per-instance colours via
`material_vertex_color(input, 0u)` (see its `verify.md`).

It has never tested that. Its `author.js` calls `add_material_variant` on the
instancer node, which is impossible:

- `InstancerDef` (`crates/scene/src/instances.rs`) has `mesh` / `transforms` /
  `per_instance_colors` / `shadow` / `lod` — **no material field**.
- `NodeKind::material_variants_mut()` returns `Some` only for
  `Mesh` / `SkinnedMesh` / `ClusterMesh`.
- the loader has an explicit branch:
  `NodeKind::Instancer(_) => instancer_default_material(..)`
  (`crates/scene-loader/src/lib.rs`).

The command failed **silently** until `editor_dispatch_json` was fixed on this
branch to report errors. The committed `project.toml` proves it never bound: the
box node has `material_variants = []`. And the golden still looks correct,
because `instancer_default_material` renders `per_instance_colors` anyway — so
the scene passes visually while testing a different code path from the one it
documents.

Isolated: `instancing-stress` also uses an instancer but puts its
`add_material_variant` on the floor **mesh**, so it authors fine.

**Suggested fix** (decided, not yet implemented): give `InstancerDef` a
`material` field. The resolution machinery already exists —
`instancer_default_material` builds a `MaterialInstance` with a nil asset and
calls `resolve_material(.., custom_shaders)`; pointing it at a real asset makes
custom shaders resolve normally. `PatchKind` merges raw JSON into the node kind
and re-deserializes, so `patch_kind {instancer: {material: ..}}` is the setter
for free — no new protocol command. Then fix `author.js`, regenerate the scene's
`project/` + `bundle/` + `golden.png`, and make `add_material_variant` on an
instancer fail with an actionable message.

No backwards-compat constraint — migrate the committed projects.

---

## Task C — `SetActiveCamera` + the two nanite goldens

`active_camera: Mutable<Option<NodeId>>` already exists in the editor
(`controller/state.rs`); `None` = editor camera, `Some(id)` = a scene camera
node. The viewport UI sets it and `render_loop` reads it. **There is no
`EditorCommand` for it**, so it cannot be driven headlessly.

Add `SetActiveCamera { camera: Option<NodeId> }` to the editor protocol, wire it
to that field.

Then `examples/test-scenes/lod-nanite` and `lod-nanite-open` — the only two
goldens on this branch still carrying the old flat-skybox bug (27 of 30 were
regenerated). They could not be regenerated because:

- their `author.js` is a stub returning
  `"see recipe comment — authoring requires a prior export bake URL"`; the real
  recipe needs `export_player_bundle` then `import_nanite_asset` against the MCP
  server's `/bundle/<handle>/` URLs;
- `load_project_from_url` against their committed `project/` DOES work (verified
  — 4 and 2 nodes), but **`project.toml` does not persist the editor camera**, so
  the authored framing is unrecoverable and a capture comes out at a far,
  coarse-LOD framing instead of the close one the golden shows;
- camera radius is the variable under test there ("watertight cut at every
  radius"), so inventing a framing changes what the golden proves.

Fix: add a **camera NODE** to both scenes (camera nodes DO persist in
`project.toml`), point the viewport at it via `SetActiveCamera`, and capture
through it. That makes the framing reproducible permanently.

Golden regeneration recipe that worked for the other 27 is in
`examples/test-scenes/README.md`; capture via `editor_query_scene_png` and POST
to the MCP server's `/png/<id>` side-channel, then curl it down.

---

## Task D — camera frustum gizmo

A camera node currently has no viewport indication of where it points. Add a
gizmo showing the camera's orientation, ideally the full frustum, toggleable in
Settings exactly like light gizmos.

Follow the existing pattern end to end — `SetViewOptions.light_gizmos` →
`settings.light_gizmos` → `engine::light_icons::per_frame_update` — and add a
`camera_gizmos` sibling. `light_icons` is the closest working reference
(pickable, re-anchored and screen-scaled per frame).

---

## Dev environment (needed for Task C, and for browser-verifying A/D)

All paths in this file are repo-relative; run from the repo root.

- `task mcp-dev` brings up everything: editor on **:9085**, MCP server on
  **:9186**, media on **:9082**, and the sibling `../test-assets` repo on
  **:9083**. It is the ONLY dev task — never run `editor:dev` alongside it.
- `task test-scenes` serves `examples/test-scenes` on **:9084**, which is how a
  scene's `author.js` is fetched for replay.
- Long-running dev servers get killed periodically in agent sessions; just
  restart. Probe ports before assuming a failure is real, and watch for a STALE
  process still holding :9085 and serving an old bundle — that cost an hour on
  this branch.
- Golden regeneration flow that worked for 27 scenes: fetch
  `http://localhost:9084/<scene>/author.js`, `eval` it in the editor page via
  Chrome DevTools MCP `evaluate_script`, `wait_render_settled`, capture with
  `editor_query_scene_png`, POST the bytes to `http://127.0.0.1:9186/png/<id>`,
  then `curl` them down to `examples/test-scenes/<scene>/golden.png`. Only
  `golden.png` changes for a render-only fix — `project/` and `bundle/` are
  scene DATA and stay untouched.
- The environment assets the `env-bc6h-spheres` scene loads live in the sibling
  `test-assets` repo under `cyber_bc6h/` (already committed and CDN-synced).

## Working rules

- `task lint` (fmt + clippy `-D warnings`) and
  `cargo test --workspace --all-features` green at every commit. Never weaken a
  test to make it pass.
- **Prove a regression test fails on the bug.** Reintroduce the defect, watch the
  test fail with the expected message, restore. Two tests on this branch passed
  against broken code because they only covered one depth convention — the same
  blind spot as the bugs they guarded.
- Shader / WGSL edits are runtime-only: browser-verify them. `task mcp-dev`,
  editor on :9085. Renderer `tracing` output goes to the BROWSER console.
- Instrument before theorising. The gizmo bug survived two confident wrong
  diagnoses (device-pixel-ratio, then selection plumbing) and was solved in
  minutes by printing the actual ray: `ro=(-inf,-inf,-inf) rd=(NaN,NaN,NaN)`.
- Delete this file when everything in it has shipped (see `docs/plans/README.md`
  — plans live here only while there is work left).
