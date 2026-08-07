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
