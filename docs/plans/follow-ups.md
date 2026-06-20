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

1. ⏸️ **Skinned skin-correspondence in the PLAYER path** *(most consequential; investigated, NOT changed)*.
   Deep code-read found the joint-driving wiring **already exists end-to-end**, contrary to the "bind pose"
   framing: `assemble_skin_joints` (editor `state.rs:6092`) populates `SkinnedMeshRef.joints {node, index}` →
   export serializes to `scene.toml` → the player SkinnedMesh arm (`scene-loader/lib.rs:939-943`) builds
   `skin_joints[bone] = node_index_transforms[j.index]` → `resolve_target` (`animation.rs:176-181`) resolves a
   bone's Transform track to the rig-glb joint key FIRST → `update_animations` drives it. So the comment is
   either **stale** or the wiring is **present-but-broken**. PRIME SUSPECT: an index-space mismatch between
   `j.index` (`node_flat_indices`, original-gltf order) and the **re-exported clean** rig glb's
   `node_index_transforms` (renumbered at `reexport_clean`).
   **Why blocked:** can't verify autonomously — the only in-repo path running `populate_awsm_scene` WITH a
   driven clock is the editor's `ReloadProjectInMemory` round-trip (`state.rs:1010`), which is MCP-triggered;
   model-tests animates but never uses the player-bundle path; the path needs a GPU (no native test).
   **Fastest check for David:** run the editor round-trip reload on an imported skinned model (Fox) — if it
   animates, the wiring's good and only the stale comment needs deleting; if it poses at bind, the index-space
   mapping is where to look.
   **Then (needs a harness):** a scene-loader integration test (load skinned bundle → `update_animations(dt>0)`
   → assert a joint moved off bind pose) verifies AND localizes the break. A separate narrower sub-gap —
   composing a user's **repositioning** of the self-placing rig glb (rooted at the renderer root to avoid
   double-applying the Z_UP node) — needs David's intent on the semantics.

2. **`InstancesAlongCurve` inside a prefab** (sub-item of item 3). Its instancing references a curve + a
   source-mesh node by id, which the asset-free, per-instance `PrefabTemplate::instantiate` can't resolve
   without the load-time `scene`/`maps`. Genuine design item (per-instance cross-node resolution), niche.
   Documented in `scene-loader/src/lib.rs`.

3. **Hidden line/decal/light runtime re-show** (sub-item of item 4). `set_node_visible` toggles meshes only;
   re-showing a *skipped* line/decal/light at runtime needs a renderer per-kind hide toggle.

4. **~18 dead `docs/plans/todo.md §N` cross-references in code comments** *(created by deleting `todo.md`)*.
   The code treated `todo.md`'s §-sections as a living architecture spec (renderer, glb-export, renderer-gltf,
   and a couple of tests). Deleting `todo.md` left those as dead links. They're harmless (comments only; code
   builds + runs), and the architecture they pointed at is now LANDED + described in the crate `//!` docs — but
   they're a David decision: **strip the dead `(docs/plans/todo.md §N)` pointers, or restore a slimmer
   architecture doc** the code can cross-reference. Not mass-edited autonomously (a no-human sweep of core-file
   comments with inaccurate §-anchors is poor risk/reward). `grep -rn "docs/plans/todo.md" packages/` lists them.

## Out of scope (tracked elsewhere)

- **Multithreading** — `docs/plans/multithreading.md`. Explicitly out of scope; do NOT change `commit_load`'s
  structure for it here.
- See also `docs/plans/nanite.md` and `docs/plans/upstream-improvements.md`.
