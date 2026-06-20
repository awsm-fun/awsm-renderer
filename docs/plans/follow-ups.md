# Follow-ups — after the "one geometry flow" epic

The **"one geometry flow: render our format, glTF is import-only"** epic is **COMPLETE** (landed on
`follow-ups`, verified live, 317 tests green). This doc replaces the old 1000-line `todo.md` running log: it
keeps the one-paragraph epic record plus the handful of **deferred follow-ons that are genuinely not done**
(they were tracked in `todo.md` and are separate from the epic's scope).

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

Verified live (editor, no GPUValidationError): SheenChair + Fox import (full bone hierarchy via the bulk
recursion), Fox skinned animation playback, mixed built-in+imported scenes, live edit + undo, material assign
+ custom WGSL, alpha cutoff/blend, multi-UV textures, lighting.

## 🔜 Remaining follow-ons (deferred, NOT part of the epic)

1. **Skinned-mesh skin-correspondence in the PLAYER path** *(most consequential)*. The rig GLB's skin poses at
   **bind pose**; our animation tracks target the scene bone nodes, not the GLB's own joints. Driving the GLB
   skin from our scene-node bones (and composing a user's repositioning of the rig) is the open work. Editor
   skinned playback works (node-owned bones); the player-bundle correspondence is the gap.
   See `packages/crates/scene-loader/src/lib.rs:30-32` and `:896-900`.

2. **`materialize_skinned_from_template` fallback cleanup.** Still the fallback when `raw_mesh_from_rig`
   returns `None` (no cached rig glb / a bone not yet in the bridge / truly-legacy projects). DELETE only after
   confirming those edge cases are covered; also RENAME `repopulate_skinned_template`. Don't stack risk on the
   verified morph-via-rig win — assess separately. (`node_sync.rs` / editor controller.)

3. **Prefab follow-ons.** Inside a `PrefabInstance`, `InstancesAlongCurve` and `ParticleEmitter` contribute
   only their transform (the emitter handle isn't threaded through `PrefabInstance` yet); a **nested** prefab
   child is captured as its own template, never inlined into its parent.
   See `packages/crates/scene-loader/src/lib.rs:230-233`.

4. ✅ **DONE — Hidden node's light no longer emits.** The player loader's `Light` arm is now gated on
   `effective_visible`, mirroring the lines/decals skip-when-hidden pattern, so a node authored
   `visible == false` (incl. via `Group` propagation) skips its light. Runtime re-show of a skipped
   line/decal/light via `set_node_visible` still needs a renderer per-kind hide toggle (unchanged follow-on).
   Verified: gate + lint + both frontends green; correct by symmetry with the existing lines/decals skip.

5. **`skin_key` reuse perf optimization.** Cache + reuse the skin key to avoid per-edit insert+free churn on
   repeated skinned re-materialise. Deferred under "optimise only if measured" (run `?stress` / `?trace`
   first); not a leak — teardown is refcounted and frees correctly today.

## Out of scope (tracked elsewhere)

- **Multithreading** — `docs/plans/multithreading.md`. Was explicitly out of scope for this epic; do NOT
  change `commit_load`'s structure for it here.
- See also `docs/plans/nanite.md` and `docs/plans/upstream-improvements.md`.
