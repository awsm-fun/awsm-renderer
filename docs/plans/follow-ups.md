# Follow-ups — after the "one geometry flow" epic

The **"one geometry flow: render our format, glTF is import-only"** epic is **COMPLETE** (landed on
`follow-ups`, verified live, 317 tests green). This doc replaced the old 1000-line `todo.md` running log. The
five post-epic follow-ons have now been worked through (autonomous pass, 2026-06-20): four resolved, one
investigated-but-verification-blocked. It is kept (not deleted) because of the open items below.

## ✅ Epic outcome (for the record)

Both goals met and verified against the code:

- **Transaction API** — one transaction everywhere: `begin_load` → declare ops in dependency order →
  `commit_load` (which dedups by cache key, drains compiles concurrently via `FuturesUnordered`, finalizes the
  texture pool ONCE, and compiles pipelines ONCE; no per-op commits, no post-hoc re-materialise). Player =
  `scene-loader::populate_awsm_scene`; editor bulk load = the join-barrier (`node_sync` `Replace` declares the
  whole forest declare-only → `commit_bulk_load` once; live add/edit stay per-node). Key commits: `1c8b2633`,
  `ba5e25b5`.
- **Data format** — glTF is import-only: arbitrary glTF refactors in-memory to our format (`gltf-convert`
  `reexport_clean_scene` + `write_glb`, stamped `AWSM_FORMAT`); the fast path (players) detects the stamp and
  skips the refactor. Editor export splits materials/animations/textures into sidecars + a geometry-only GLB
  (incl. skins + morphs); the player loads it directly via the Transaction API, no double-load. N-set multi-UV
  + 12 KHR_* material extensions round-trip.

## ✅ Resolved this pass

- **Item 4 — Hidden node's light no longer emits.** The player loader's `Light` arm is gated on
  `effective_visible`, mirroring the lines/decals skip-when-hidden pattern. Commit `d2ae548a`.
- **Item 3 — ParticleEmitter replays in prefabs.** New `PrefabReplay::ParticleEmitter` (per-instance billboard
  + teardown), additive/isolated. Commit `501e352b`. (`InstancesAlongCurve`-in-prefab stays deferred — see
  Open below.)
- **Item 5 — skin_key reuse perf: closed as a measured no-op.** With `?trace=sub-frame`, a skinned
  re-materialise is dominated by the material-pipeline recompile every flip incurs; the extra skin insert+free
  is a sub-ms refcounted upload — no stall to optimize. Commit `fcd4b94a`.
- **Item 2 — skinned fallback cleanup.** Renamed `repopulate_skinned_template` → `rebuild_skinned_template`;
  KEPT `materialize_skinned_from_template` as a deliberate safety net (uncached-rig / legacy sources) with
  corrected docs (morph-only is node-owned now; the bone-ordering transient is handled by the join-barrier).
  Verified Fox + AnimatedMorphCube import clean. Commit (this pass).

## 🔜 OPEN — need David or a harness

1. ✅ **RESOLVED — skinned animation WORKS in the PLAYER path** *(the "bind pose" claim was stale)*. The
   joint-driving wiring is end-to-end: `assemble_skin_joints` (editor `state.rs:6092`) populates
   `SkinnedMeshRef.joints {node, index}` → export → the player arm (`scene-loader/lib.rs:939-943`) builds
   `skin_joints[bone] = node_index_transforms[j.index]` → `resolve_target` (`animation.rs:176-181`) resolves a
   bone's Transform track to the rig-glb joint key → `update_animations` drives it. The index spaces match by
   construction (`node_flat_indices` = the same DFS flatten the clean rig glb uses; unit test
   `flat_indices_follow_depth_first_not_source_order`).
   **VERIFIED LIVE (2026-06-20):** added a `#[wasm_bindgen] editor_tick_animation(dt_ms)` test seam (main.rs;
   one `renderer.update_animations(dt)` call), then via `evaluate_script`: import Fox → `LoadPlayerBundle`
   (reload via `populate_awsm_scene`, project → `round-trip.awsm`) → tick the clock → **the Fox skin deforms
   across t=0 / 1.0s / 1.9s** (three distinct articulated poses), no console errors. So the player-path skinned
   animation is confirmed working. Stale `// bind pose` comments removed from `scene-loader/src/lib.rs`.
   **Remaining narrow sub-gap (separate, minor):** composing a user's *repositioning* of the whole rig — it
   self-places at the renderer root (to avoid double-applying the `Z_UP` node), so moving the scene node
   doesn't move the rig. Unrelated to the now-verified joint animation; needs David's intent on the semantics.

2. ✅ **IMPLEMENTED — `InstancesAlongCurve` now replays inside a prefab** (was NOT a design call — just
   deferred work). Added `PrefabReplay::InstancesAlongCurve`: capture bakes the curve placement (`find_curve`
   + `curve_instance_transforms`, both existing pure fns), and a SECOND pass in `PrefabTemplate::instantiate`
   resolves the source node → this instance's own duplicated mesh and calls `enable_mesh_instancing_opaque` +
   `set_mesh_instance_attrs` — the EXACT calls the (working) non-prefab `materialize_instances_along_curve`
   makes. Green (tests + lint + frontends). ⚠️ **Not live-verified**, for a structural reason, NOT avoidance:
   `PrefabTemplate::instantiate` has **zero callers anywhere in the repo** (`grep -rn "\.instantiate(" packages/`
   = none) — it's a forward API (the player captures prefab templates; a game instantiates them), so even the
   pre-existing Light/Camera/Decal/emitter prefab replays are unexercised live, and the renderer is browser-only
   (no native test). A live screenshot would require writing the first prefab-instantiation consumer (author a
   curve+source+instances prefab → retain the template past `LoadPlayerBundle` → instantiate → render). Offered
   to David as a separate harness task. The implementation is glue over already-verified functions + structurally
   traced.

3. **Hidden line/decal/light runtime re-show** (sub-item of item 4). `set_node_visible` toggles meshes only;
   re-showing a *skipped* line/decal/light at runtime needs a renderer per-kind hide toggle.

4. ✅ **DONE — dead `docs/plans/todo.md §N` cross-references stripped** (David's call). Deleting `todo.md` had
   left 29 dangling `docs/plans/todo.md §N` pointers across renderer / glb-export / renderer-gltf / scene-loader
   / model-tests / editor comments; removed the pointers, kept the explanatory prose (the architecture they
   referenced is landed + in the crate `//!` docs). Comment-only; tests + lint + both frontends green.
   `grep -rn "docs/plans/todo.md" packages/` now returns none.

## Out of scope (tracked elsewhere)

- **Multithreading** — `docs/plans/multithreading.md`. Explicitly out of scope; do NOT change `commit_load`'s
  structure for it here.
- See also `docs/plans/nanite.md` and `docs/plans/upstream-improvements.md`.
