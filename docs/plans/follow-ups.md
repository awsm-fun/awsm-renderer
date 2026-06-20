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
   bone's Transform track to the rig-glb joint key FIRST → `update_animations` drives it.
   **The prime suspect (index-space mismatch) is now REFUTED by code (2026-06-20):** `node_flat_indices` is
   built (editor `gltf.rs:287-296`) via `awsm_glb_export::scene_node_flat_indices` — the SAME DFS flatten the
   clean rig glb uses — and is explicitly "the index the player's loader will assign that joint" (there's a
   unit test, `flat_indices_follow_depth_first_not_source_order`). The player keys
   `node_index_to_transform` by `node.index()` of the clean rig glb it loads (`renderer-gltf/populate.rs:128`).
   So `j.index` and the player's lookup are the **same clean-glb DFS space** — they match by construction. That
   makes **"the bind-pose comment is stale" the likely answer** (the `skin_joints` wiring + index mapping look
   correct + tested); a live run is the only thing left to confirm it, vs. some subtler break (e.g. joints not
   surviving export, or clip bone-targets not exported).
   **LIVE VERIFICATION (2026-06-20, via the `editor_dispatch_json` wasm seam):**
   - ✅ **Player path RENDERS the skinned Fox correctly.** Dispatched `LoadPlayerBundle` (`{cmd:
     "load_player_bundle"}`) — bakes the project + reloads via `populate_awsm_scene`. Fox renders, no errors,
     project name → `round-trip.awsm`. So the player-path skin loads + binds + draws fine.
   - ✅ **The cold-reload skinned path ANIMATES.** Dispatched `ReloadProjectInMemory` (clears
     `skin_joints`/templates/bake-cache to model a cold load, restores + re-materializes) → played the clip →
     the Fox visibly deformed (head lowered into frame across t=0.04→2.05). Same `skin_joints` binding logic.
   - ⚠️ **The one thing still not directly seen:** the player-BUNDLE skin deforming under a driven clock.
     `LoadPlayerBundle` is a *static* authored-vs-runtime render comparison by design — it empties the editor
     scene + clip library, so the bundle's clips live in `r.animations` with no editor-side driver, and the
     transport / `SampleClipTimeseries` query only reach editor clips. The render loop pins (doesn't advance)
     the bundle. So nothing in the editor ticks `update_animations` for the bundle.
   **Verdict: very likely WORKING; the bind-pose comment is almost certainly stale.** Index wiring proven +
   player render verified + the equivalent cold-reload path animates. To close the last gap autonomously I'd
   add a small driver — e.g. a `#[wasm_bindgen] editor_tick_animation(dt_ms)` test seam (calls
   `renderer.update_animations(dt)`), so after `LoadPlayerBundle` I can tick + screenshot the bundle deforming.
   Pending David's go-ahead on adding that seam (vs. he confirms in ~30s himself).
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

4. ✅ **DONE — dead `docs/plans/todo.md §N` cross-references stripped** (David's call). Deleting `todo.md` had
   left 29 dangling `docs/plans/todo.md §N` pointers across renderer / glb-export / renderer-gltf / scene-loader
   / model-tests / editor comments; removed the pointers, kept the explanatory prose (the architecture they
   referenced is landed + in the crate `//!` docs). Comment-only; tests + lint + both frontends green.
   `grep -rn "docs/plans/todo.md" packages/` now returns none.

## Out of scope (tracked elsewhere)

- **Multithreading** — `docs/plans/multithreading.md`. Explicitly out of scope; do NOT change `commit_load`'s
  structure for it here.
- See also `docs/plans/nanite.md` and `docs/plans/upstream-improvements.md`.
