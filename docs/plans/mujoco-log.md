# MuJoCo implementation log

Running notes for the overnight `/loop` driving `docs/plans/mujoco.md`. Newest entry
at the bottom. Deleted at Phase 6 along with the plan.

## BLOCKED

*(nothing — no blockers as of the latest entry)*

---

## 2026-08-07 — Phase 1, increment 1: `mujoco-sys` bindings crate

**Landed.** `packages/crates/mujoco-sys` (`awsm-renderer-mujoco-sys`): runtime-loaded
FFI to the official MuJoCo library, plus a safe `Library`/`Model` wrapper and slice
accessors over the `mjModel` fields the exporter needs (geom type/group/bodyid/
dataid/matid/size/pos/quat/rgba, body parent/pos/quat, material colour params) and
`mj_id2name`/`mj_name2id` lookups.

**Verified (native — no browser surface in this increment).**
`MUJOCO_DIR=~/.local/share/mujoco/3.11.0 cargo test -p awsm-renderer-mujoco-sys` →
4/4 pass against the real MuJoCo 3.11.0 dylib: the DeepMind humanoid compiles
(nbody 17, ngeom 20, nmesh 0), body 0 is `world`, geom 0 is the `floor` plane,
every geom quaternion is unit and every size is finite/positive/plausible, and
`flex/flag.xml` reads materials in range. `cargo check --workspace` and
`cargo clippy -p awsm-renderer-mujoco-sys --all-targets` clean.

**Decisions taken** (recorded in the plan under "Decisions taken during
implementation"): dlopen instead of link-time binding, so the crate is a normal
workspace member that builds with no MuJoCo installed; checked-in bindgen output
(no libclang build requirement); version lock enforced at load against
`mjVERSION_HEADER`.

**Bug caught by verifying rather than compiling.** The first test run went 4/4
green while *nothing had actually loaded*: `EXPECTED_VERSION` was hand-written as
`311` but `mj_version()` returns `3011000`, so every test hit the version-mismatch
path and the helper's blanket skip-on-error turned that into a pass. Fixed twice
over — `EXPECTED_VERSION` now derives from the generated header constant, and the
test helper panics instead of skipping whenever `MUJOCO_DIR` is set (skipping is
only for machines with no install at all).

**Environment set up** (both outside the repo, nothing committed):
`~/.local/share/mujoco/3.11.0` (official macOS release: `mujoco.framework` +
`model/`) and `~/.local/share/mujoco_menagerie` (shallow clone, for the Phase 1
Unitree exit criterion).

**Next:** `packages/tools/mujoco-export-cli` — start with the sidecar
(`mujoco.json`) half over primitives only, dumping the humanoid's geom/material
tables, before touching the GLB/mesh path.

---

## 2026-08-07 — Phase 1, increment 2: sidecar schema + exporter's sidecar half

**Landed.** Two crates:

- `packages/crates/mujoco-format` (`awsm-renderer-mujoco-format`) — the frozen
  `mujoco.json` schema. Pure serde, no MuJoCo dependency, so the wasm-side editor
  and scene-loader can read it and the native exporter can write it. Enums are
  documented strings, unknown fields are ignored, absent ones default, and
  `Sidecar::validate()` checks the index references (`geom.body`, `geom.material`,
  `geom.mesh`, `body.parent`) that JSON cannot express — the exact silent-failure
  the plan's parity checklist is about.
- `packages/tools/mujoco-export-cli` (`awsm-renderer-mujoco-export`) — compiles a
  model with MuJoCo's own compiler and writes `<name>.mujoco.json`: source
  fingerprint (filename + SHA-256 + MuJoCo version), bodies, geoms, materials,
  mesh names. GLB half not yet started.

**Verified — no browser surface in this increment** (a native CLI; nothing
user-visible reaches the browser until the editor import command lands, which is
where the on-device criterion starts to apply). What was actually run:

- Exported the **Unitree Go2** from a menagerie checkout: 14 bodies, 56 geoms,
  16 meshes, 4 materials (`metal`/`black`/`white`/`gray`). The group split the
  plan calls out is real and correct — **33 visual mesh geoms in group 2, 23
  collision primitives (box/sphere/cylinder) in group 3**, nothing else.
  Every visual geom resolves to a mesh AND a material; every collision geom has
  neither.
- Exported the DeepMind humanoid: 17 bodies, 20 geoms, 0 meshes, floor is a
  `plane` on body 0, all quaternions unit.
- 8 tests green (`cargo test -p awsm-renderer-mujoco-export-cli
  -p awsm-renderer-mujoco-format` with `MUJOCO_DIR` set), `cargo check
  --workspace` and clippy clean.

**Found by reading real models** (recorded under Phase 1 in the plan):

- MuJoCo **re-frames mesh geoms at compile time** — a mesh geom's pos/quat are
  relative to the mesh's recentred inertial frame, not the authored frame. Go2's
  visual geoms all carry non-identity pos/quat because of it. Self-consistent
  only if the GLB's vertices come from `mjModel.mesh_vert` (post-compile) and
  never from the source OBJ/STL. Directly constrains the next increment.
- `geom_dataid` is overloaded (mesh id vs heightfield id) — only dereference it
  against the geom type.
- Collision geoms have no material, so material-less must fall back to
  `geom_rgba`, not error.

**Next:** the GLB half of the exporter — `mjModel.mesh_vert`/`mesh_face`/
`mesh_normal`/`mesh_texcoord` through the existing `glb-export` crate, one glTF
mesh per MuJoCo mesh, verified by loading the emitted GLB back and comparing
counts (and eventually by the editor rendering it).

---

## 2026-08-07 — Phase 1, increment 3: geometry GLB (ON-DEVICE VERIFIED)

**Landed.** The exporter's GLB half. The GLB is a flat geometry *library* — one
root node per MuJoCo mesh, mesh order, identity transform, **no materials** —
because geoms are instances of meshes (many geoms share one) and materials get
minted in our system at import. MuJoCo's OBJ-style separate index arrays are
de-indexed on export ((pos, normal, uv) triple → one glTF vertex), UV V flipped
so the GLB is a plain correct glTF any viewer opens right, and face indices are
bounds-checked per corner because "mesh-relative not absolute" is a convention
read off the library, not a header guarantee.

**What the browser showed.** Exported `unitree_go2/go2.xml` → 196,748 triangles
across 16 meshes, served over HTTP, imported into the editor at :9085 via
`editor_dispatch_json {"cmd":"import_model_from_url"}` (headless drive, no MCP
pairing, no tab eviction). Result:

- All **16 nodes** present, named exactly as the sidecar's `Mesh::node` records
  them (`mesh_0_base_0` … `mesh_15_foot`) — the sidecar↔GLB binding holds
  through a real import.
- Screenshot: the Go2 chassis shell renders with the **"GO2" logo legibly
  embossed** on the top surface, correct smooth shading, sensor housing and
  panel gaps all visible. Real geometry, correct normals, correct scale.
- All 16 parts pile up at the origin, which is *correct* for a geometry library
  and is the visual signature of the recentring described below.
- **Console clean**: zero errors. Only pre-existing benign warnings (trunk
  preload `integrity`, a wasm-bindgen init deprecation, and one first-frame
  "pipeline not compiled at line_pass" notice from the renderer) — nothing from
  this work.

**Verified the framing claim instead of assuming it.** Last iteration I wrote
that MuJoCo recentres mesh vertices; this iteration measured it. Every one of
Go2's 16 meshes has its centroid within **13 mm** of its own origin, while the
geoms that use them carry offsets up to **0.33 m**. So the compiler really does
recentre each mesh and fold the difference into `geom_pos`/`geom_quat` — which
is why the GLB's vertices must come from `mjModel` and never from the source
OBJ. Now a real test (`meshes_are_recentred_and_geoms_carry_the_offset`) rather
than a comment.

**Noted, not chased:** the editor HUD reported 19.2k triangles for a GLB the
gltf reader measures at 196,748 — presumably the editor's LOD on import
reporting the drawn level. Not an export defect (the GLB content is verified
independently), but worth confirming when the MuJoCo import command lands.

**Also:** the CLI crate became lib + bin so tests drive the export directly
rather than through `#[path]` includes.

**Next:** the editor-side import — a `mujoco` component in editor-protocol +
scene formats, and an import command that reads sidecar + GLB and mints geom
nodes at their sidecar poses under an instance root. That is where the pipeline
parity checklist starts applying, and where Phase 1's real exit criterion (the
Go2 rendering as a robot, groups honoured, materials mapped) gets met.

---

## 2026-08-07 — Phase 1, increment 4: the `mujoco` node component (ON-DEVICE VERIFIED)

**Landed.** `EditorNode::mujoco: Option<MujocoComponent>` — an enum, `Instance`
(source fingerprint, model name, `geom_count`, `visible_groups`) or `Geom`
(`geom_id`, `group`, `kind`, `body`). A field on the node rather than a table on
the scene, so it rides along through copy/delete/reparent and the plan's
"scene-level sim instance list" is derived rather than stored. No stored
geom_id→node_id map either: each geom knows its own id and the binding is
resolved by walking the subtree, so there is no second copy to drift.

Because `EditorProject` (authoring) and `Scene` (bundle) are both built from
`EditorNode`, one field covers both formats.

**The bug this iteration existed to find.** After the schema landed and every
round-trip test passed, the browser round-trip returned the component **empty**.
The editor does not serialize `EditorNode` directly — its reactive
`engine::scene::Node` converts through `NodeSpec`, which has its own field list.
A per-node field has to be added in *four* places (`EditorNode`, `NodeSpec`, both
conversions, and the reactive `Node`), and nothing but a browser round-trip
catches missing one. Exactly the silent-failure class the parity checklist names.
Recorded in the plan so the next per-node field doesn't repeat it.

**What the browser showed** (after the fix): a hand-written `project.toml`
carrying an instance + a child geom, loaded into the editor at :9085 via
`load_project_from_url`, then read back with `editor_project_toml` — every field
present and correct: `[nodes.mujoco.instance]` with `model_name`/`geom_count`/
`visible_groups`, `[nodes.mujoco.instance.source]` with the real Go2
filename/sha256/mujoco_version, and `[nodes.children.mujoco.geom]` with
`geom_id = 12`, `group = 2`, `kind = "capsule"`, `body = 3`.

**Parity checklist**: three of five boxes green, one ruled N/A after checking
(`node_sync`'s effective-def merge is `MaterialInstance` → `MaterialDef` only,
nothing per-mesh here), one still open (MCP coverage, nothing to cover until the
import command). Notes on how each was verified are in the plan.

**Next:** the editor import command — read sidecar + GLB, mint an instance root
with the convention rotation and a geom node per visible geom at its sidecar
pose, minting materials from the sidecar's Phong terms. That is Phase 1's real
exit criterion: the Go2 standing there as a robot.

---

## 2026-08-07 — Phase 1, increment 5: the editor import command (ON-DEVICE VERIFIED)

**Landed.** `EditorCommand::ImportMujocoFromUrl { sidecar_url }` +
`controller/mujoco_import.rs` + the `import_mujoco_from_url` MCP tool (parity
matrix row + wire-tag guard). Fetches the sidecar, validates it, resolves and
loads the GLB the sidecar names, and mints:

- a **sim instance root** — user-placeable, carrying the model fingerprint and
  the Z-up→Y-up convention rotation (`from_rotation_x(-π/2)`, matching the Phase
  0 template);
- one node per geom in MuJoCo's visible groups (0–2), each stamped with its geom
  id, `locked` because the stream owns its transform;
- one mesh **asset** per MuJoCo mesh, shared by every geom that uses it.

**What the browser showed.** `import_mujoco_from_url` on the exported Go2 →
**the robot standing on four legs** in the editor viewport: body on top, four
legs down to the feet, at the model's real qpos0 pose. 34 nodes / 33 meshes,
outliner shows the geom-id gaps (0,1,2,3,4,8,9,11,…) that prove collision group
3 was skipped, every geom node locked. **Console clean — zero errors.** Geoms
render magenta because materials are deliberately the next increment.

**The bug the browser found.** The first render was a jumble: I was applying
`geom.pos`/`geom.quat` as node locals, but those are **body-relative**, and the
geom nodes are flat. Composing the body chain would have given the *joint-zero*
pose, which is not `qpos0` for any robot with a reference configuration. Fixed
at the source instead: the exporter now runs `mj_makeData` + `mj_forward` and
records each geom's **world** pose at `qpos0` in the sidecar. That is also
exactly the shape a stream frame carries, so the simulation's first frame will
continue from the initial render rather than jumping.

**Two harness traps worth remembering** (both cost a cycle):
- Waiting on `! pgrep rustc` races trunk — it exits before the build starts.
  Wait for the build to *start*, then finish.
- Reloading the page with `ignoreCache` does NOT bust the cache for the
  editor's own `fetch` of the sidecar. A stale pre-`world_pos` JSON came back
  and every geom landed at the origin. Cache-bust the asset URL, not the page.

**Parity checklist is now fully green** — the last box (MCP coverage) closed
with the dedicated tool; `packages/mcp/tests/parity.rs` caught the missing
matrix row exactly as designed.

**Next:** materials — mint a library material per sidecar material (MuJoCo's
Phong terms → our PBR) and assign per geom, so the Go2 renders in its real
metal/black/white/gray instead of magenta.

---

## 2026-08-07 — Phase 1, increment 6: materials — **PHASE 1 EXIT CRITERION MET**

**Landed.** The import now mints one library material per sidecar material and
assigns it per geom. Materials go into the assignable library (not inline per
node), exactly as the glTF importer does, so editing "metal" once repaints every
metal part. Geoms with no material get one minted from their own `geom_rgba`,
deduped by colour, so nothing is ever left on the magenta unassigned sentinel.

MuJoCo Phong → our PBR, documented as an approximation: `rgba` → base colour
(alpha < 1 turns on blending), `roughness = 1 - shininess`, `metallic =
reflectance`, `emissive = rgba * emission`. **`specular` is deliberately
dropped** — a dielectric's specular intensity is fixed in a metallic-roughness
model, and every menagerie part defaults to `specular = 0.5`, so folding it in
would make the whole robot read as half-metal.

**What the browser showed.** The **Unitree Go2 rendering as the actual robot**:
white shell with the "GO2" logo legible on top, black feet, black sensor grille
on the head, standing on four legs at its qpos0 pose. Library shows
metal/black/white/gray; HUD reads 34 nodes / 33 meshes / 4 materials / 4
buckets. **Console clean** — zero errors, only the same three pre-existing
warnings (trunk preload `integrity`, a wasm-bindgen init deprecation, one
first-frame pipeline-compile notice).

That is Phase 1's stated exit criterion, on-device: *"a menagerie Unitree model
renders correctly (groups honored, materials mapped) in its initial pose;
instance root placeable; model fingerprint recorded; parity checklist green."*
All five hold.

**Still open inside Phase 1** (recorded in the plan, not blockers for the
criterion above): primitive geom kinds — the DeepMind humanoid is all capsules
and currently imports as empty nodes with correct geom ids but no geometry — and
re-import-in-place preserving user material overrides.

**Next:** primitive geoms, so the humanoid renders too. Our `PrimitiveShape` has
no capsule or ellipsoid, so the plan is: box/sphere/cylinder/plane map directly
(with the axis fix — MuJoCo's cylinder/capsule run along local Z, our primitives
along Y), ellipsoid = a sphere with per-axis node scale, capsule = the Z-axis
capsule builder the Phase 0 template already wrote, ported to a captured mesh.

---

## 2026-08-07 — Phase 1, increment 7: primitive geoms (ON-DEVICE VERIFIED)

**Landed.** `controller/mujoco_primitive.rs` — MuJoCo's primitive shapes as
meshes, ported from the Phase-0 template (which worked them out against the live
wasm build): plane, sphere, capsule, ellipsoid, cylinder, box.

Decisions: they become **captured meshes, not `PrimitiveShape` recipes** —
capsule and ellipsoid have no `PrimitiveShape` equivalent at all, and sim-bound
geometry is stream-owned rather than edited, so one uniform path beats a split
that would only make some geoms editable. Shapes dedupe by (kind, size), so the
humanoid's 16 capsules mint a handful of assets. Ellipsoid is the exception to
"geometry carries the size": a unit sphere with per-axis **node scale**, safe
because a pose frame writes translation and rotation only.

MuJoCo's axis conventions are honoured in the geometry rather than worked around:
capsules/cylinders run along local Z (ours along Y) so those meshes are rotated
+90° about X — a proper rotation, so winding and normals stay valid. Planes have
a +Z normal and are usually authored infinite (`0` half-extent), which falls back
to a finite 10 m quad.

**What the browser showed.** The DeepMind humanoid imports and renders as a
humanoid: sphere head and hands, capsule torso/waist/limbs, both feet, arms out
at its qpos0 pose, standing on its own MuJoCo ground plane (visible as the floor
with a horizon). Outliner carries the real MJCF names — `torso`, `waist_upper`,
`thigh_right`, `foot1_left`, … — 21 nodes / 20 meshes / 2 materials. **Console
clean, zero errors.**

One false alarm worth recording: an early camera angle made the arms look like a
detached second figure floating above the body. Checking the exported world
poses (arms at z≈1.25–1.34, torso 1.28, head 1.47) showed the data was right;
re-framing the camera showed a perfectly normal humanoid. Read the numbers before
believing a bad angle.

**Next:** Phase 1's last follow-on — re-import in place, preserving user material
overrides — then Phase 2 (capture-to-clip bake).

---

## 2026-08-07 — Phase 1, increment 8: re-import in place — **PHASE 1 COMPLETE**

**Landed.** `ImportMujocoFromUrl` grew an optional `reimport: NodeId`.

**Explicit, not inferred.** Omitted, the command always ADDS an instance;
given, it updates that one in place. Matching on model identity instead would
have broken the plan's own `multi` scenario — two instances of the same robot in
one composed world — by silently turning the second import into an update.

A re-import refreshes everything the *model* owns (poses, geometry, fingerprint,
the geom table — including restoring geoms whose nodes were deleted) and
preserves everything the *user* owns (the root's id, name and placement; each
surviving geom's material palette, matched by `geom_id`). It builds the fresh
subtree and transplants it rather than diffing two live trees, so there is no
second construction path free to drift from the real one.

**What the browser showed.** Import Go2 → move the instance to `[3,0,-2]` →
delete a geom node → add and select a "user-override" material variant on
another geom → **re-import twice**. Result: one root, same node id, placement
still `[3,0,-2]`, 33 children again (the deleted geom restored), the
user-override variant still present *and still selected*.

**A wart the browser caught.** The first run reported **8 materials** where there
should be 4 — every re-import was minting a fresh metal/black/white/gray and
stranding the old set. Fixed by deduping library materials on name AND
definition; matching the definition too means a user who has edited "metal" keeps
their edit (the incoming def no longer matches, so the model's version arrives
beside theirs rather than overwriting it). Two consecutive re-imports now hold
the library at 4.

Console clean throughout.

**Phase 1 is complete**: exporter (sidecar + GLB), the `mujoco` node component
with the parity checklist green, the editor import command + MCP tool, materials,
primitive geoms, and re-import. Both oracles render on-device — the Unitree Go2
as the real robot, the DeepMind humanoid as a humanoid.

**Next: Phase 2** — the capture format and the capture→animation-clip bake.

---

## 2026-08-07 — Phase 2, increment 1: capture format + reference recorder

**Landed.** Two halves of the same seam:

- `mujoco-format::capture` — the frozen capture schema. Deliberately the same
  shape as the live stream, dumped verbatim: a flat `7 * geom_count` `f32` array
  per frame (`[px,py,pz,qw,qx,qy,qz]`, indexed by geom id) plus a time in
  seconds. A harness that can feed the pose sink records a capture by writing
  what it was already sending. `geom_count` is stated once at the top so a
  truncated frame is caught rather than shifting every geom after it.
- `awsm-renderer-mujoco-record` — the reference producer, a second binary in the
  exporter crate. Steps a model with MuJoCo's own integrator and writes the
  capture. Explicitly in scope per the plan's settled decisions ("streamer adds
  `mj_step` + mjData reads"); still a native tool, no MuJoCo in the renderer.

**Verified against real physics — native, no browser surface in this increment**
(the capture is a file format; the browser part arrives with the bake, next).
What was actually run: recorded the DeepMind humanoid for 3 s at 30 fps, and

- the ragdoll **actually falls** — torso z 1.282 m standing → 0.266 m at 1.57 s →
  settled at 0.262 m. That is the check that the recorder steps physics rather
  than dumping one pose 86 times;
- every quaternion in every frame stays unit to **6.9e-8** (worst case across
  20 geoms x 86 frames), which is what validates the `geom_xmat` → quaternion
  conversion across a real range of orientations, not just at rest;
- **two runs are byte-identical** (sha256 equal). Sampling on step counts rather
  than an accumulated float clock is what buys that, and it is the whole reason
  checked-in fixture captures are worth checking in;
- **frame 0 matches the sidecar's `world_pos` to 1e-5**, so a capture and the
  imported initial pose agree and the first simulated frame will not jump.

**Next:** the bake — capture → native animation clips (T+R track per bound geom
node, keyframe-reduced), the editor command to import a capture, and then the
on-device scrub/play that is Phase 2's real exit criterion.

---

## 2026-08-07 — Phase 2, increment 2: the capture→clip bake

**Landed.** `scene::mujoco::bake` — a capture plus a geom_id→node map in, a
`StoredAnimation` out. Pure data both ways, so it is natively unit-testable and
the editor command on top will be a thin wrapper. After it runs there is no
MuJoCo left in the result: translation and rotation tracks on scene nodes,
indistinguishable from a hand-authored or glTF-imported clip.

Decisions (all now in the plan):

- **Static geoms get no tracks at all** — a robot's world is mostly floors and
  fixtures, and keying those flat every frame would bloat the bundle and fight
  anything else animating them.
- **Quaternions are made continuous before keying.** A simulator may emit `q`
  then `-q` (the same rotation); interpolating across that flip takes the long
  way and a limb visibly spins a full turn between two near-identical keys. One
  dot product per frame prevents it. The "did it move?" test runs *after* the
  flip, so a pure sign change reads as no motion.
- **Reduction measures against the last KEPT key**, not the original neighbours,
  so error cannot accumulate across a long run of drops.
- **Track order is sorted**, not HashMap order, so two bakes of one capture are
  identical — otherwise golden comparison would be defeated by iteration order.

**Verified — native, no browser surface yet** (the bake is a pure function; the
browser part is the editor command, next). Beyond 12 unit tests on synthetic
captures, the real end-to-end run on a recorded 3 s humanoid fall:

- the baked torso track **still falls** — first key z > 1.0 m, last < 0.5 m;
- **reduction keeps 2470 of 7638 keys (32%)** on real physics data;
- the humanoid's floor plane contributes **no track**;
- the clip **round-trips through TOML** as ordinary animation data, which is the
  concrete proof it is a normal clip and not a special case.

**A test I had to correct rather than the code.** My first sign-flip test fed a
capture whose only change *was* the flip, then asserted a rotation track existed
— but `q` and `-q` are the same rotation, so emitting nothing is right. Rewrote
it to rotate for real (0°→60°→120°, last frame negated) and added a second test
pinning the other half: a pure sign flip must NOT resurrect a static geom.

**Next:** the editor command — import a capture, resolve the binding against a
sim instance (rejecting a fingerprint mismatch loudly), bake, and add the clip to
the library. Then Phase 2's real exit criterion: scrubbing that clip in the
browser and watching the humanoid fall.

---

## 2026-08-07 — Phase 2, increment 3: capture import + bake in the editor (ON-DEVICE VERIFIED)

**Landed.** `EditorCommand::ImportMujocoCapture` + the `import_mujoco_capture`
MCP tool (parity row + wire tag). Fetches a capture, checks its model
fingerprint against the target instance, resolves the geom→node binding from the
live tree, bakes, and adds the result to the clip library as an ordinary clip.

**What the browser showed.** Imported the humanoid, then baked a 4 s / 267-frame
capture into a clip named "ragdoll fall" (3.99 s, tracks reduced to 45–98 keys
each from 267 frames). Scrubbing the playhead drives the scene:

| t | torso world Y |
|---|---|
| 0.0 | 1.282 |
| 0.8 | 1.040 |
| 1.6 | 0.271 |
| 3.9 | 0.261 |

Screenshots confirm it visually — the humanoid **standing upright with arms out
at t=0**, and **collapsed in a heap on the floor at t=3.9**, head sphere resting
on the ground. That is a recorded MuJoCo simulation replayed from a baked
animation clip, scrubbed in the editor. Phase 2's scrub/play criterion, met.

Note the world Y values equal the capture's MuJoCo z — the convention root is
doing its job, and the clip needed no conversion at all.

**The fingerprint guard was exercised, not just written.** Baking a Go2 capture
onto the humanoid instance is refused with `this capture is of a different
model: it fingerprints go2.xml (50adb09a4365), but the instance was imported
from humanoid.xml (acc9167550bf)` and adds no clip. The one console error in the
session is that deliberate rejection.

**Remaining in Phase 2:** the player bundle playing the baked clip, and a
browser-test-suite scene + goldens replaying a checked-in fixture capture.

---

## 2026-08-07 — Phase 2, increment 4: the player bundle plays the baked clip (ON-DEVICE VERIFIED)

**No new code this iteration** — this was the bundle round-trip verification the
plan's parity checklist and Phase 2 both ask for.

**What the browser showed.** Composed a scene (humanoid instance + the baked
"ragdoll fall" clip, 38 tracks / 3.99 s), then ran `load_player_bundle`: bake the
project to an in-memory player bundle, reset to empty, reload through
`populate_awsm_scene` — the actual runtime path a player uses.

- The editor tree goes **empty (0 objects)** and the HUD reads 0 nodes / 0 meshes
  — the scene now exists only inside the renderer, exactly a player's situation.
- The renderer reports `clip_groups: 1`, `per_clip: [{channels: 38, name:
  "ragdoll fall"}]`, `resolved_channels: 38`. **All 38 baked channels resolved
  through the player loader**, so the clip survived project → scene.toml →
  populate.
- Ticking the renderer's animation clock 39 × 100 ms (`editor_tick_animation`,
  the same call a game makes each frame) **plays the fall**: standing humanoid
  before, collapsed on the floor after, shadowed by the bundle's directional
  light. With no editor scene left, nothing else could be driving it.

Console clean apart from one deliberate rejection from my own test script
(I passed the default Directional Light as the instance id; the guard said "that
node is not a MuJoCo sim instance" — right answer, my mistake).

**Harness notes** (both cost time; the MCP memory is updated):
- The running MCP server binary predates the new mujoco tools, so `tools/list`
  doesn't have them — the stale-tool-list trap. Rather than restart the server
  (which can tear down the whole `task mcp-dev` group), I drove the MuJoCo
  commands through `wasmBindings.editor_dispatch_json` and used MCP only for
  `load_player_bundle`, which the old binary already has.
- Attaching the editor to MCP is now just
  `http://localhost:9085/?mcp=http://127.0.0.1:9186` (no pair code since the
  single-session refactor). The reconstructed `/tmp/mcp.py` needed one fix: the
  initialize response's FIRST SSE event is a bare `data: ` with an empty payload,
  which a naive parser tries to json-decode.

**Remaining in Phase 2:** a browser-test-suite scene + goldens replaying a
checked-in fixture capture, deterministic with no sim in CI.

---

## 2026-08-07 — Phase 2, increment 5: browser-test-suite scene — **PHASE 2 COMPLETE**

**Landed.** `examples/test-scenes/mujoco-capture/` — the whole MuJoCo pipeline as
a permanent suite scene, with **no simulator in the loop**:

- `fixtures/` — a checked-in sidecar + a 2 s / 58-frame capture (112 KiB
  together). Replaying the same JSON must produce the same pose, so CI never
  runs physics.
- `author.js` — `new_project` → `import_mujoco_from_url` → `import_mujoco_capture`
  → `set_playhead {t: 1.2}` → pinned camera, grid/gizmos off.
- `project/`, `bundle/`, `golden.png`, `verify.md`, plus a row in the suite
  README.

**What the browser showed.** Authored through the real MCP pipeline
(`save_project` → `export_player_bundle` → `screenshot_scene`, artifacts copied
from the temp dirs). The golden is the humanoid **frozen mid-collapse at
t=1.2 s** — doubled forward, head near the ground, on the model's own flat
ground plane, softly shadowed. `verify.md` spells out the four ways this scene
can fail (standing pose = clip not driving; heap at the origin = lost world
poses; a limb spun a full turn = hemisphere flip survived the bake; lying on its
side = convention rotation missing or doubled).

**A real defect the artifact bake exposed.** The saved `project.toml` came out
at 1021 KiB. Reading it showed why: my bake wrote `value` into both
`in_tangent` and `out_tangent`, which is meaningless for a linear key — the
editor's own `new_keyframe` writes ZEROES — and tripled the stored floats.
Fixed; the same scene now saves 792 KiB (−22%). Worth noting the browser step
found this, not the unit tests: every test still passed either way.

Scene size (2.0 MB) is in line with the suite (existing project.tomls run
1.2–4.6 MiB).

**Phase 2 is complete**: capture format, reference recorder, the bake, the
editor command + MCP tool, scrub/play on-device, the player-bundle round-trip,
and now a deterministic suite scene with a golden.

**Next: Phase 3** — the pose sink in scene-loader (binding resolution + the
player-API sink), then the `physics-mujoco` reference template migrates onto it.
