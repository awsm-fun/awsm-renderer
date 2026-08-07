# MuJoCo implementation log

Running notes for the overnight `/loop` driving `docs/plans/mujoco.md`. Newest entry
at the bottom. Deleted at Phase 6 along with the plan.

## BLOCKED

*(nothing — no blockers as of the latest entry)*

## OPEN (not blocked on you — scoped out with reasons)

- **Flex deformation streaming.** The flex SURFACE ships and renders at its bind
  pose (flag, bunny, floppy all verified). Making it *move* needs one of two
  multi-increment pieces of renderer work, and I judged neither worth starting
  unattended when Phases 5 and 6 were still open:
  1. A **dynamic-vertex mesh path** in the renderer — the plan's original
     wording. The geometry pipeline explodes positions into a visibility stream,
     derives tangents, AABBs and BVH state at commit, so per-frame vertex
     upload means re-running a chunk of the hottest code in the engine.
  2. **Skinning**, which needs no new renderer capability at all: each flex
     vertex is rigidly attached to exactly one body, so one joint per vertex at
     weight 1 reproduces the deformation exactly through the existing GPU
     skinning path, driven by an ordinary body-pose channel. The catch is the
     editor's MuJoCo import mints plain mesh assets, so it would need a skinned
     path; and interpolated flexes (bunny) have no vertex bodies at all and
     would stay static.
  My recommendation is (2) — it is the smaller surface and reuses machinery that
  already works. Everything the exporter emits serves either route unchanged.

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

---

## 2026-08-07 — Phase 3, increment 1: the pose sink (ON-DEVICE VERIFIED)

**Landed.** `scene_loader::mujoco` — the renderer's entire contract with an
external simulator, and the whole of the MuJoCo runtime surface in this repo. No
networking, no transport, no timing source, no MuJoCo code:

- `MujocoInstance` — the instance root, the model fingerprint, and a
  `Vec<Option<TransformKey>>` indexed **by geom id** and sized to the model's
  full geom count. An array, not a map, because it is indexed once per geom per
  frame; unrendered geoms are `None` and the id space stays the model's, so a
  producer never re-indexes a frame.
- `resolve_instances` — walks the loaded tree and fills `LoadedScene::mujoco`.
  Derived, never stored, same as everywhere else in this feature.
- `apply_geom_poses` — writes one frame. Translation and rotation only, with
  **scale preserved** by read-modify-write (an ellipsoid geom is a unit sphere
  scaled per axis; overwriting scale would flatten it on frame one). A mis-sized
  frame is an error, never a truncation.

Plus two editor test seams (`editor_apply_mujoco_poses`,
`editor_mujoco_instances`) mirroring `editor_tick_animation`: after a bundle
reload the scene lives only in the renderer, exactly as it does for a player.

**What the browser showed.** Imported the humanoid **with no clip at all**, ran
`load_player_bundle` (editor tree empty, 0 clips — so nothing but the sink could
move anything), then fed all **58 frames of the recorded fixture capture**
straight into `apply_geom_poses`:

- **58/58 frames applied, zero errors**; the humanoid went from standing to
  collapsed on the floor. Screenshots before and after.
- The torso's MuJoCo z walks 1.282 → 1.246 → 0.899 → 0.348 → 0.261 → 0.262
  across the fed frames.
- Both guards fired on purpose: a 100-float frame → *"pose frame has 100 floats,
  this instance needs 140 (7 per geom x geom count)"*; instance index 3 → *"no
  sim instance 3 (the last bundle reload resolved 1)"*.
- Console clean.

This is the same fixture the `mujoco-capture` scene bakes into a clip, now taking
the OTHER path — proving both consumers of one capture agree.

**Next:** the templates-repo `physics-mujoco` template migrating onto the sink
(its Phase-0 client-side mirror is throwaway by design), plus collider components
(universal core + `mujoco` extension block), which the plan lists as a Phase 3
prereq.

---

## 2026-08-07 — Phase 3, increment 2: collider physics params (ON-DEVICE VERIFIED)

**Landed.** The plan's Phase-3 prereq: `collider::PhysicsParams` — a universal
core (sliding friction, restitution, collision layer/mask, optional density)
plus an optional `MujocoPhysics` extension block (torsional + rolling friction,
`condim`, `solref`/`solimp`, `margin`/`gap`, `priority`) — on
`EditorNode::physics`, with `SetPhysicsParams` and the `set_physics_params` MCP
tool.

Decisions (in the plan): a **separate optional node field**, not a payload on
`NodeKind::Collider` (widening that variant would break every saved project and
baked bundle for a field absent on almost every node); a **whole-value replace**
rather than a patch, because the fields interact — MuJoCo's torsional and rolling
friction are inert below `condim` 4 and 6, which is the usual reason a value
appears to do nothing; and **omitted sub-fields fall back to MuJoCo's own
defaults**, not zero. Layer/mask default to "everything" so an author who never
touches them gets collisions rather than silence.

The doc comments carry the two facts a user will otherwise learn the hard way:
MuJoCo has NO restitution parameter (bounce lives in solref/solimp, so the
universal value maps only approximately), and engines combine pairwise friction
differently (MuJoCo max-modulo-priority, Rapier average, Box2D geometric mean),
so identical authored values do not give identical contact behaviour.

**What the browser showed.** Inserted a collision box, set a full params block
via `set_physics_params`, then ran `reload_project_in_memory` (the editor's own
save→load self-test — serialize, drop session caches, load back). The
`[nodes.physics]` and `[nodes.physics.mujoco]` blocks come back **byte-identical**,
with the sub-fields I did NOT author (`torsional_friction`, `rolling_friction`,
`solref`, `solimp`) filled in from MuJoCo's defaults exactly as designed.
Console clean.

**Parity checklist for the new field**: one field on the shared `EditorNode` so
both formats carry it; the bundle bake guarded by
`bake::tests::collider_physics_params_survive_the_bundle_bake`; save/load
verified in the browser as above; `node_sync` N/A (not a per-mesh material
override); MCP covered by the dedicated tool + parity row.

Threading it through took the same four places the `mujoco` field did
(`EditorNode`, `NodeSpec`, both conversions, the reactive `Node`) — the note in
the plan paid for itself.

**Next:** the templates-repo `physics-mujoco` template migrating onto the pose
sink, which is the last piece of Phase 3 and the plan's end-to-end oracle.

---

## 2026-08-07 — Phase 3, increment 3: the template migrates onto the sink — **PHASE 3 COMPLETE**

**Landed** (templates repo, commit `b6d4042` — its own git repo, renderer crates
pinned to local paths).

The Phase-0 spike built its own renderer-side mirror: read `mjModel` off the
wasm module, mint a meshgen primitive + material + node per geom. The plan
called that throwaway, and the pipeline it stood in for now exists. So:

- the robot is **authored content** — exported by `awsm-renderer-mujoco-export`,
  imported in the editor, shipped in `media/bundle` like any other geometry;
- the render worker builds **nothing**. It takes the instance the loader already
  resolved (`LoadedScene::mujoco`) and each frame hands the seqlock'd snapshot to
  `scene_loader::mujoco::apply_geom_poses`;
- `render_thread.rs` goes **693 → 460 lines**, and every geom-to-node decision
  now happens once at import instead of on every page load.

**The SAB pose block is now literally a stream frame.** The worker writes
quaternions in MuJoCo's `[w,x,y,z]` order rather than glam's `[x,y,z,w]`, so
nothing is reshaped on either side of the SharedArrayBuffer — this worker could
dump its block verbatim as a capture file and the editor would bake it into a
clip. That is the format doing its job.

`link_sim` refuses to bind when the sim's geom count disagrees with the scene's
instance, for the same reason the capture import checks fingerprints.

**What the browser showed.** HUD: *"mujoco: model ready — 20 geoms, dt 5.0 ms"*
→ *"sim linked (20/20 geoms) — running"* → *"first frames rendered — ready"*.
The humanoid is **collapsed on the ground under live physics**, from a bundle
whose authored pose is standing — so only the sink could have moved it. Console
clean, zero errors.

(The machine was under heavy build contention: WebGPU device acquisition alone
took 31 s and renderer init 81 s. Slow, not broken — it did reach `running`.)

**Phase 3 is complete**: pose sink, collider physics params, and the reference
template driving real physics through the real pipeline.

**Next: Phase 4** — sites, spatial tendons, skins, heightfields, and flex
deformables.

---

## 2026-08-07 — Phase 4, increment 1: sites (ON-DEVICE VERIFIED)

**Landed.** Sites end to end: `mujoco-sys` accessors (`site_*` model fields plus
`site_xpos`/`site_xmat`), a `sites` table in the sidecar, the exporter filling
it from `mj_forward` at `qpos0`, a `MujocoSite` component, importer nodes, and a
**site channel** in the pose sink (`apply_site_poses`).

**Decision**: sites are their own component and their own channel, never a flag
on `MujocoGeom`. MuJoCo indexes sites separately, so a shared id space would put
a site's pose in a geom's slot — precisely the silent failure the geom-id binding
exists to prevent. Both channels go through one shared `write_poses`, so they
cannot drift in behaviour. The matrix→quaternion conversion was factored out of
`geom_world_quat` so geoms and sites share it.

**What the browser showed.** Imported MuJoCo's `tendon_arm/arm26.xml` (11 sites,
5 geoms): the editor renders the red capsule arm segments, the translucent purple
shoulder/elbow cylinders, and **all 11 site spheres** — white and green, at their
MuJoCo world poses along the arm, exactly where the muscle attachment points are.
The outliner carries the MJCF names (`s0`, `x0`, `s1`…`s8`, `x1`). The saved TOML
has 11 `[nodes.children.mujoco.site]` blocks with `site_id` 0–10 **beside** 5
`[…mujoco.geom]` blocks — separate id spaces, as designed. Console clean.

**A harness bug that cost real time tonight, now understood.** Several of my
"wait for the build" loops never terminated: `until ! pgrep -f "rustc.*awsm..."`
matches the shell running *that very command*, so pgrep always finds itself. It
looked like the machine was compiling for twenty minutes when nothing was. Wait
on artifact mtimes or a marker file, never on a pgrep pattern that appears in the
waiting command.

Also: with the template page (:9000) running its sim and the editor both holding
WebGPU contexts, the editor wedged before first paint — bindings answered but the
boot overlay never cleared and screenshots showed it. Closing the template page
fixed it immediately. Two live WebGPU pages on this machine is one too many.

**Next in Phase 4:** heightfields (exporter-side, self-contained), then spatial
tendons, skins, and flex.

---

## 2026-08-07 — Phase 4, increment 2: heightfields (ON-DEVICE VERIFIED)

**Landed.** Heightfields are **baked to meshes at export**, which is what the
plan asked for and the right call for a deeper reason: MuJoCo's grid is static
after compile, so there is nothing dynamic to preserve — and baking here means
the editor, the scene format and the pose sink never learn what a heightfield is.
An hfield geom arrives downstream as an ordinary mesh geom.

The sidecar still records `type: "hfield"` — it stays a faithful record of the
compiled model — with `mesh` pointing at the baked entry, appended after the real
meshes. The bake emits the top surface plus a skirt down to the model's base
depth, on its own vertices, so the terrain reads as solid at a grazing angle and
the rim fold stays sharp instead of smearing normals.

**What was verified.** Exported `google_barkour_vb/scene_hfield_mjx.xml`. Reading
the baked mesh straight back out of the GLB: **69,616 vertices, 256 distinct
elevation levels** (exactly what an 8-bit PNG heightfield yields, so the data was
read correctly, not zeroed), z spanning **−0.100 → +0.050** — precisely the
model's `size` z-scale (0.05) and base depth (0.1). In the editor it renders as
undulating ground with the Barkour robot standing on it; 37 nodes, console clean.

**Next in Phase 4:** spatial tendons, then skins, then flex.

---

## 2026-08-08 — Phase 4, increment 3a: spatial tendons, static half (ON-DEVICE VERIFIED)

**Landed.** The sidecar gained a `tendons` table, the scene gained a
`MujocoComponent::TendonSegment`, and the importer mints a **preallocated pool**
of unit-cylinder segments per drawable tendon.

The one design call worth recording: a tendon's waypoint count changes at
runtime as it wraps around geometry, and a pose stream can only write transforms
— it can't create nodes. So the pool is sized from the *compiled model*
(`max_waypoints = 2 * tendon_num`, MuJoCo's own rule for sizing `wrap_xpos`) and
the unused tail simply starts hidden. Sizing it from the initial pose instead
would have looked fine on arm26 today and run out of segments the first time a
tendon wrapped: `BF` routes through 7 waypoints at rest but is allowed 10.
Segments are cylinders rather than the capsules MuJoCo draws, because length
lives in the node's Z scale and only a cylinder scales along its axis without
distorting.

**A real bug the tests caught before the browser did.** My first pass assumed
every tendon is drawable. MuJoCo's own `humanoid.xml` proved otherwise: its
hamstrings are FIXED tendons — joint-coupling constraints with no path through
space — and they exported with a pool but zero waypoints, which would have drawn
two stray cables at the origin. Now told apart by `wrap_type` (the compiled
truth) rather than by a runtime `ten_wrapnum` of 0, and exported with
`max_waypoints: 0` while keeping their slot, since the index *is* the tendon id.

**What the browser showed.** `tendon_arm/arm26.xml` imported: 6 tendons → 38
segment nodes (`SF 0`…`BE 8`), 14 of them hidden spares, and the viewport shows
the muscle cables running the length of the arm and visibly bending as they pass
through the site spheres. Then a genuine save/load roundtrip: `save_project` →
served over a CORS-enabled static server → `load_project_from_url` → re-save.
`tendon_capacity = [6, 6, 6, 6, 10, 10]`, 38 `tendon_segment` blocks and 14
`visible = false` all came back identical, and the reloaded scene renders the
same. Separately, `humanoid.xml` imported 0 segment nodes with
`tendon_capacity = [0, 0]` — the fixed-tendon fix confirmed on-device, not just
in a test. Console clean.

**Two harness notes.** `load_project_from_url` silently does nothing if the
served project has no CORS headers (plain `python3 -m http.server` won't do), and
the failure looks exactly like a successful no-op load — I only caught it by
renaming a node in the served `project.toml` and checking the marker came back.
Also, appending a cache-buster to `base_url` corrupts the fetch path; serve on a
fresh port instead.

**Next in Phase 4:** the tendon waypoint stream channel +
`apply_tendon_waypoints` in the sink (increment 3b), then skins, then flex.

---

## 2026-08-08 — Phase 4, increment 3b: the tendon stream channel (ON-DEVICE VERIFIED)

**Landed.** `apply_tendon_waypoints` in the pose sink, plus `TendonSlots` on the
resolved instance and an `editor_apply_mujoco_tendons` test seam. Tendons are now
drivable end to end.

Three design calls, all recorded in the plan. The frame is
`[live_count, then the full capacity as xyz triples]` per tendon — fixed size
even though the count varies, so a producer can publish it into a preallocated
SAB, which is the same reason the segment pool is preallocated. A count above the
pool is an error rather than a clamp, because it means the producer and the
imported model disagree about which model this is. And segment visibility is
**edge-triggered** — `set_mesh_hidden` bumps the TLAS revision and re-syncs the
spatial index, so re-asserting it every frame for every segment would churn the
BVH for nothing; that is why this one channel takes `&mut` on the instance while
geoms and sites take `&`.

**What the browser showed**, through the real player path (editor → 
`export_player_bundle` → `load_player_bundle` → drive):

- Replaying the model's own qpos0 waypoints reproduces the imported placement
  exactly — the editor and the sink share `segment_transform`, and an unchanged
  frame is the proof.
- Bowing the waypoints outward plucks all six cables off the arm, endpoints
  still pinned at their site spheres.
- Filling every tendon to its full capacity makes the hidden spares appear: the
  three-chord bow becomes a smooth many-segment arc. This is the whole point of
  the pool, and it works.
- Shrinking back to two waypoints collapses each tendon to a single chord and
  re-hides the rest.
- `WrongLength` and `TooManyWaypoints` both fire with useful messages.

Console clean. The bundle carried all 38 `tendon_segment` blocks and
`tendon_capacity`, and the loader resolved them back to a `tendon_frame_len` of
138 — so the `export_player_bundle` parity box is re-checked for tendons.

Also fixed in passing: the `WrongLength` message hardcoded a geom-channel
explanation, which was wrong the moment a second channel existed.

**Next in Phase 4:** skins (mjSkin → skinned meshes), then flex/deformables.

---

## 2026-08-08 — Phase 4: flex surfaces, and skins dropped on evidence (ON-DEVICE VERIFIED)

**Skins are gone, and I checked rather than assumed.** Before building anything I
probed `nskin` across MuJoCo 3.11's shipped models and the whole menagerie: it is
**0 everywhere**. The single model in the tree with a `<skin>` element,
`plugin/elasticity/belt.xml`, does not even compile — it needs an elasticity
plugin dylib we do not load. `mjSkin` is the legacy deformable; **flex** is the
one that exists, with 30-odd demo models in `model/flex/`. So I reordered: flex
now, skins recorded as dropped with the evidence. Both are noted in the plan.

**Landed.** The exporter half of flex: a sidecar `flexes` table, the surface
baked into the geometry GLB, a `MujocoFlex` component, and importer nodes.

Design calls, all in the plan. The surface goes into the GLB with a synthetic
`meshes` entry appended after the meshes and heightfields — the heightfield trick
again — so a flex is just another mesh asset and the importer needed no
flex-specific plumbing. Only the *surface* is baked: a 2D flex's elements already
are triangles, but a 3D flex's are tetrahedra whose visible boundary is
`flex_shell`, and drawing the tets would fill the inside with invisible faces.
Vertices come from `mjData` in world space, not from `flex_vert` (which is each
vertex in its own body's frame), so the node transform is identity — a deformable
has no rigid frame. Normals are computed at export because MuJoCo ships none.

**One thing the models taught me.** `bunny.xml` failed validation on the first
run with a vertex body id of −1: a flex with interpolation drives its vertices
from a cage of NODES, not bodies. `vertex_bodies` is now all-or-nothing —
populated only when every vertex has a body — because a partial list would let a
consumer skin some vertices and strand the rest at the bind pose. The validator
catching this before the browser did is exactly what it is for.

**What the browser showed.** `flag.xml` imports as a flat 171-vertex /
288-triangle cloth sheet at its qpos0 height. `bunny.xml` imports as a
recognisable, smooth-shaded 2,503-vertex Stanford bunny — a wrong index rebase or
the wrong element/shell choice would give a mess, not a bunny. `body_attached`
reads `true` for the flag and `false` for the bunny. Console clean.

**Next:** the flex vertex stream channel. It needs a renderer decision I have
deliberately not pre-empted: a body-attached flex could be **skinned** to its
vertex bodies (one joint per vertex, weight 1) through the existing GPU skinning
path with no new renderer capability, while an interpolated one can only be
driven by uploading vertex positions, which the renderer has no path for today.
The exported data serves either route.

---

## 2026-08-08 — Phase 4 closed, Phase 5 started: the contacts overlay (ON-DEVICE VERIFIED)

**A judgement call first.** The next flex increment was deformation streaming,
and I went looking for the cheapest honest route before writing any of it. The
renderer's geometry pipeline explodes positions into a visibility stream and
derives tangents/AABB/BVH state at commit, so a per-frame vertex upload means
re-running a chunk of the hottest code in the engine — not something to start
unattended, on a branch, for the item this plan itself ranks last, with Phases 5
and 6 still open. There is a much cheaper route (skin each flex to its vertex
bodies, one joint per vertex at weight 1 — no new renderer capability), but it
needs a skinned path through the editor's MuJoCo import, which is also several
increments. So I parked it, with both routes and my recommendation written up in
the OPEN list at the top of this log, and moved to Phase 5. Nothing about the
exported flex data has to change for either route.

**Landed (templates repo, `ac44524`): the contact-point overlay.** The sim
worker publishes contact positions and normals in the **same seqlock** as the
poses, so the overlay can never draw contacts from a different step than the
bodies they touch. The count varies every step while the block must be
fixed-size, so the region is preallocated with the live count in the header —
the same shape as the renderer's tendon channel, for the same reason. Opt in
with `?contacts`; overlays stay off by default and out of the way of what the
template is actually demonstrating.

One small renderer-side addition made this clean: `MujocoInstance` now exposes
`root_transform`. The spikes are parented under it, so contact positions apply as
raw MuJoCo world coordinates exactly as geom poses do, instead of the template
re-deriving the convention rotation and drifting from it.

**What the browser showed.** The collapsed humanoid reports 10–13 live contacts.
Four red spikes stand along the resting foot (the classic box-foot corner set)
with more at the hip, each oriented along its `+Z` floor normal, and they appear
and disappear as the sim shifts. Console clean.

**Two harness notes.** Trunk does not re-copy `web/` on its own — only a Rust
rebuild triggers the copy, so a worker-only edit needs a manual `cp` into
`dist/`. And my first look showed no spikes at all with everything working
correctly: at 4 mm × 6 cm they were a red hair a couple of pixels wide. The
console instrumentation, not the screenshot, is what proved the data was flowing;
the spikes are now 12 mm × 12 cm.

**Next in Phase 5:** force arrows, joint axes, inertia boxes — same channel, same
pool shape.
