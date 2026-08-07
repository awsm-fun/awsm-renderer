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
