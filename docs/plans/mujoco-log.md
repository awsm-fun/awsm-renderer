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
