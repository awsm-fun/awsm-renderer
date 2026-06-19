# Plan: one geometry flow — render our own format; glTF is import-only

**Remaining work.** The "geometry into the load transaction" foundation has landed (see *Already landed*
below). What's left is to collapse to **ONE render path: our own proprietary format** — and make glTF an
**importer**, never something the renderer renders directly. This kills the last two-sources-of-truth
seams (editor skinned content; the glTF-direct render path) so we debug + optimise in one place.

> Scope note: worker-hosting the renderer (main-thread responsiveness / the loading-UI paint nuance) is
> tracked separately in `docs/plans/multithreading.md` and is explicitly OUT of scope here. We are NOT
> changing `commit_load` to add mid-operation yields — the library should not add asynchronous jank for
> an application threading choice.

---

## 0. The north star (David, confirmed)

**The renderer is NOT a "glTF renderer" — it is a glTF *importer* + a renderer of our own proprietary
format.** There is ONE thing the renderer renders: **our format**. glTF only ever enters through import,
which converts it to our format; from then on everything is our format. No code path renders glTF
directly. This holds for EVERYTHING we support — including **model-tests**, which imports a glTF file into
our format **in-memory** and renders that (so even the plain-GLB viewer is consistent with the editor).
Kill the two-sources-of-truth problem everywhere.

**Our format:**
- **Geometry + attribute data → glb** (a clean, geometry-only glb — incl. skins + morphs; the existing
  `awsm_glb_export::reexport_clean` output / rig glb). This is the ONE canonical geometry container;
  decode-it-once is the ONE geometry source.
- **Materials → a separate sidecar** (extracted at import; the editor's material library).
- **Animation → our own clip format** (extracted at import via `extract_animations`, supplemented with
  editor additions like clip mixing).

**TWO DISTINCT STAGES — and only the second touches the GPU (David, confirmed):**
```
STAGE 1  IMPORT (glTF → our format)   — PURE DATA, GPU-FREE, no renderer. Runs ONCE per asset.
         glTF bytes ─► clean glb (geometry/attrs incl. skin+morph, via glb-export)
                     + materials sidecar (extracted)  + clips (extracted)
STAGE 2  MATERIALISE (our format → GPU) — the ONE render path; the ONLY thing that uploads.
         our format ─► decode clean glb → GeometrySource → begin_load → add_mesh → commit_load
                     + bind materials + load clips ─► screen
```
- **Import does NOT push to the GPU.** It produces our-format artifacts (bytes/data) and nothing else —
  no transforms, no skins, no meshes, no uploads. Import ≠ populate; they are different jobs (today
  fused in `populate_gltf`, which decodes glTF AND uploads — split them).
- **Materialise is the single GPU/render operation**, invoked *whenever* a drawable is needed: first
  show, after a project reload, after an edit. **There is no "re-" anything** — each is the same
  materialise reading the retained our-format. The existing `repopulate_skinned_template` is a misnomer
  born of the old session-local design (it "redid" populate on reload); fold it INTO the materialise
  stage and drop the "re"/"populate" naming — it's just *materialise skinned from our-format glb*, used
  uniformly for first-show / reload / re-materialise. NOTHING is done twice.
- **editor**: STAGE 1 on user import; our format is persisted (glb + sidecars) and is what MCP edits +
  what's written to disk. STAGE 2 whenever a node needs a drawable (read the retained our format).
- **model-tests / raw-glb player**: STAGE 1 in-memory, then STAGE 2. (model-tests is still a glTF
  *viewer* — it converts to our format first instead of rendering glTF directly.)

Stage 2's geometry half is the transaction: `begin_load() → add sources (GeometrySource) → commit_load()`,
where a "source" is a `GeometrySource` (geometry + optional skin + optional morph) decoded from our-format
glb. EVERY producer lowers to it.

> ## ⭐ TRANSACTION PRINCIPLE (David, confirmed — applies to ALL loading, do NOT violate)
>
> Loading/materialising ANYTHING is ONE transaction, in this shape:
> 1. **`begin_load`** — start the transaction.
> 2. **add a whole bunch of operations** — glTF, raw meshes, materials, textures, animations, skins, whatever
>    — establishing transforms BEFORE the geometry that references them.
> 3. **`commit_load`** — commit ONCE.
> 4. **the commit does the smart organisation INTERNALLY** — dedup (pack each rep once), run concurrently,
>    resolve geometry, free sources. The caller does NOT pre-organise; it declares, then commits.
>
> **Anti-patterns this forbids (each is a "you did it wrong" smell):**
> - **post-hoc "re-materialisation" passes** that re-run a materialise after the fact to patch ordering /
>   timing races. If a thing isn't ready when you add it, the FIX is to order the transaction correctly
>   (add its dependency first) — NOT to add it broken and re-do it later. (This is the mistake caught on the
>   skinned-reload bone-ordering bug: the right fix is to establish the bone transforms before the skinned
>   geometry within the load, not to re-materialise the skinned node once the bones land.)
> - **per-operation `commit_load`** (a commit per node) instead of one commit for the whole load — it
>   defeats the commit's cross-operation dedup/concurrency. (The editor's reactive per-node materialisation
>   currently does this; consolidating editor LOAD onto one transaction is the aligned direction.)
> - any "smart organisation" (ordering, dedup, batching) done OUTSIDE `commit_load` that the commit should
>   own.
>
> When in doubt: declare everything into the open transaction, in dependency order, then commit once and let
> the commit do the work.

> **Goals this serves:** one way to do things (debug/optimise in one place); fix non-transactional perf;
> NO perf regressions; reduce resource consumption (one decode, no double-geometry, clean up on teardown);
> "scene editor → our format → player/editor" coherently for everything.

## 1. Already landed (do NOT redo — context for the work below)

The load-transaction + geometry-resolution core is implemented, verified, and green
(commits `f8837818..HEAD` on `follow-ups`):

- **The transaction**: `AwsmRenderer::begin_load` → `register_geometry` / `add_mesh` → `commit_load`.
  `commit_load` phase 0 = `resolve_geometry` → `resolve_one` per geometry: union the bound materials'
  kinds, pack each needed rep ONCE via `mesh_pack`, upload one shared per-geometry resource, wire every
  bound mesh, free the source.
- **`GeometrySource`** (`meshes/geometry.rs`) already unifies geometry + `skin_key`/`skin_info` +
  `geometry_morph_key`/`info` + `material_morph_key`/`info`. The deform compute pass runs off `skin_key`
  regardless of producer. (Raw + glTF both produce it today; `add_raw_mesh` passes `skin`/`morph` =
  `None`.)
- **One classifier + one packer**: `geometry_kind(material, is_hud)`; `GeometryReps` (union → distinct
  reps, dedup-tested); `route_renderable` (a both-rep geometry routes by the live material — the free
  opaque↔blend flip). Legacy `Meshes::insert`/`insert_public`, `add_raw_mesh_transparent`,
  `mesh_buffer_geometry_kind`, `GltfGeometryOverride` are all deleted.
- **`awsm-glb-export`** already models **skins** (`ExportSkin`: joints + inverse-bind matrices; per-vertex
  `JOINTS_0`/`WEIGHTS_0`) **and morphs** (`MorphTarget` deltas + default weights) — the geometry-glb
  container exists.
- **Animations** are already extracted into the editor's own clip format
  (`awsm_renderer_gltf::extract::extract_animations` → `ExtractedAnimation`, then editor clips + mixing)
  — decoupled from the glb.
- **Editor precedents to build on**: static imports are captured to an editable `MeshDef` + re-materialise
  via `add_raw_mesh` through `node_sync::apply_kind` (teardown + rebuild); the bundle loader loads a
  **materialless geometry-only glb + a separately-applied material** (`GltfMaterialSource::Single`) — the
  "separated glb + materials" pattern, in production.
- **The skinned source is ALREADY retained + persisted as a glb** (this de-risks the whole epic): at
  import `awsm_glb_export::reexport_clean_scene` builds a **clean rig glb** (skeleton + skin + morph);
  `skinned_bake_cache::store_rig_glb`/`get_rig_glb` hold it (thread-local) and persistence writes it to
  `assets/<id>.rig.glb`; **`gltf::repopulate_skinned_template(source, rig_bytes)` rebuilds the skinned
  renderer template from those bytes** (already used on project reload). So the editor DOES know where to
  re-import skinned geometry from — the rig glb. Static geometry persists in parallel as
  `assets/<id>.mesh.bin` (bitcode `CapturedMesh` / `MeshData`). The geometry-as-glb container the epic
  wants therefore already exists for the skinned case.

**The one remaining special case (what this plan removes):** skinned meshes
(`node_sync::materialize_skinned_mesh`) share the populate-built renderer geometry and re-assign material
via `set_mesh_material` instead of re-materialising. So a skinned mesh flipped to a NEVER-built kind
(opaque↔blend) can't rebuild — it degrades to a graceful `Skip` (vanishes until re-import). Re-skinning
isn't possible either. Fixing this uniformly is the heart of the plan.

## 2. End-state: editor content as an authored, glb-backed source

The editor's job becomes:

1. **Create** content — primitives, imported glbs, MCP/UI edits — over the full surface: geometry, bones,
   skins, morphs, materials, textures, transforms, animations.
2. **Persist** it in our proprietary format = **(a) a glb for GEOMETRY** (a pure geometry container,
   *including skins + morphs*, edited or not) + **(b) materials, separately** + **(c) animations in our
   own clip format** (extracted, then supplemented with editor additions like clip mixing).

Geometry, materials, and animation are SEPARATE concerns in the format — the glb is *only* a geometry
container, so editing a clip or a material never round-trips geometry, and vice-versa. Loading /
re-materialising editor content is then just **(re-)importing the glb through the same source path** +
binding the separate materials + loading the authored clips. Re-materialisation after ANY edit (material
flip, geometry / bone / morph edit, re-skin) = re-import from the authored glb. No second path.

## 3. Phased path

> **Plan refinement (recorded during implementation — code reality vs the §4 assumption).** §4 assumed
> skinned re-import is clean per-node like static. The codebase shows otherwise: skinned geometry is
> **source/template-scoped** — the populate-built skinned meshes are owned by the per-`skin.source`
> template, are SHARED across nodes, and deliberately **survive node teardown** (that is exactly why the
> skinned path uses `set_mesh_material` instead of rebuilding). Material re-fires, by contrast, are
> **per-node** (`rematerialize_for_material` re-sets each node's `kind`). So "repopulate the shared
> template on every per-node flip" (the old Phase 1) would THRASH when nodes share a source (each
> repopulate replaces the template, orphaning the just-built meshes of sibling nodes). **Resolution: merge
> Phase 1 into Phase 2** — make skinned geometry **per-node captured content (including skin)** that
> re-materialises through the SAME teardown+rebuild path as static, with the rig glb as the *extraction
> source* (not a per-flip template rebuild). The skeleton stays persistent (joints are scene nodes;
> animation channels target them — unchanged). This dissolves the source-scoping wrinkle AND fixes the
> flip, in one consolidation. Phases renumbered below.

- ✅ **Phase 1 — Carry skin + morph through the renderer capture/raw path into `GeometrySource`.** DONE
  (commit `b04d5716`). `RawMeshData` gained optional `skin: Option<RawSkin>` (joints + inverse-bind
  matrices + set_count + packed index/weight bytes — the exact shapes `Skins::insert` + the glTF decode
  use) and `morph: Option<RawMorph>` (layout info + weights + values). `add_raw_mesh` inserts them into
  the shared skin/morph stores and attaches `skin_key`/`skin_info` + `geometry_morph_key`/`info` to the
  `GeometrySource`, so a skinned/morphed raw mesh flows through the SAME register → add_mesh →
  resolve_one path the glTF skinned import already uses. `None` ⇒ static, unchanged. All existing
  `RawMeshData` literals gained `..Default::default()`. Gate + lint + both wasm frontends green.
  (No GPU unit test for actual deformation — the renderer is WebGPU-only; correctness rests on the glTF
  path already exercising skin-through-resolve_one + the live editor test in Phase 2.)

> **⚠ BASELINE REGRESSIONS FOUND + FIXED while standing up live verification (this loop).** The
> load-transaction foundation (`a3cbc797`, "already landed") had broken the editor's glTF import — the
> renderer's WebGPU-free lib tests can't catch it, and the path was never live-verified. Two coupled bugs,
> same root cause (`populate_gltf` is now a pure deferred ADD that resolves NOTHING until `commit_load`,
> but the editor never committed before reading renderer state):
> - **Import PANICKED (every model, static + skinned).** `bind_mesh` stamps `world_aabb` at add-time (so a
>   bound-unresolved mesh stays OUT of `collect_renderables`' `world_aabb.is_none()` fallback), but the
>   mesh isn't in `scene_spatial` until resolve. The editor released the renderer lock between populate and
>   the materialise-time commit, so a render frame ran mid-window and the per-frame debug invariant fired
>   (`scene_spatial leaf count (0) diverged from meshes with world_aabb (1)`). **Fixed** (`5de3f75e`): the
>   invariant now counts only RESOLVED meshes (`resource_key().is_ok()`) — debug-only, zero release change.
> - **Import produced NO geometry (every node became an empty Group, 0 tris).** The `AssetTemplate` snapshot
>   reads `keys_by_transform_key` (→ `transform_to_meshes`, populated at resolve) + `mesh_is_skinned` /
>   `geometry_morph_key_for_mesh` (→ the mesh RESOURCE, built at resolve). Pre-commit these are all empty,
>   so `build_editor_subtree` saw zero mesh keys → Group. **Fixed** (`4b97d2f2`): `import_typed` +
>   `repopulate_skinned_template` now `commit_load` after populate, before the snapshot.
> - **Verified live** (chrome-devtools): DamagedHelmet → `NodeKind::Mesh` (1.2k tris, textured, rendered);
>   Fox → `NodeKind::SkinnedMesh` (1.2k tris, fox_material + base-color tex, rendered). Player path
>   (model-tests :9080) renders Fox + materials + shadows, regression-clean. This UNBLOCKS the epic's live
>   verification (import + render now work; Phase 2 can be verified against a working baseline).
> - **BASELINE "deforms + animates" CONFIRMED ✅ (live).** Fox imports with its 3 clips (Survey/Walk/Run);
>   the active Survey clip shows "21 tracks → 21 players · Live" (every bone track resolved to a renderer
>   channel), and playing it visibly DEFORMS the skin (body/hindquarters shift between playhead 1.57s→1.92s
>   — not a rigid node move). So the import→render→deform→animate baseline is fully working — the yardstick
>   for Phase 2 is in place. The earlier "clip targets deleted nodes" toast WAS the transient relower race
>   (bones materialise async after the clip first lowers; `schedule_relower` re-fires and it resolves to
>   21/21). Benign import-race artifact — optionally suppress it during the import burst (low priority).
> - **NOTE for Phase 5 / GPU-free import (§0):** these fixes ADD a `commit_load` at import (the meshes are
>   uploaded then hidden — the current double-geometry the epic removes). That's the correct interim shape
>   for today's populate-then-snapshot flow; the IMPORTER/MATERIALISER split (§0) replaces it with a
>   GPU-free import that snapshots from the clean glb decode, not from a committed populate.

> **▶ PHASE 2 MATERIALISE-REWRITE — CORE LANDED ✅ (material flip re-renders, verified live).**
> The skinned drawable is now NODE-OWNED + rebuilt from the clean rig glb (our-format) — an opaque↔blend
> material flip re-renders instead of vanishing. Commits: `0899350c` (additive `SkinnedMeshRef.rig_node_index`
> — chose interim (2): node_flat_indices captured at import into the field, persisted; node_index stays
> original so drop_skinning/export/bake-cache are untouched — ZERO blast radius), `27d40e00` (the rewrite).
> What landed: `raw_mesh_from_rig` decodes the rig glb (cached via `skinned_bake_cache::get_rig_node_decode`)
> → `RawMeshData{skin}` with joint TransformKeys mapped from `SkinnedMeshRef.joints`, IBMs+index_weights from
> the same decode (rig-glb IBMs == original, per the round-trip proptest → exact bind pose); the skin reads
> the ANIMATED EDITOR BONES directly (no `skin_bridge` hop). `materialize_skinned_mesh` builds it node-owned
> via `add_raw_mesh` with the CURRENT material → `model_meshes` (teardown+`apply_kind` rebuilds on any edit).
> `hide_template_meshes` hides ALL populate meshes (skinned too — no double-render). Legacy template-reuse
> kept as `materialize_skinned_from_template` for morph-only / no-rig-cache nodes.
> VERIFIED LIVE (Fox): imports via the new path + renders; Survey clip plays + visibly DEFORMS (bones +
> geometry move across frames); `fox_material` Opaque→Blend KEEPS RENDERING (was the vanish bug), no
> VisibilityGeometryBufferNotFound / no JointAlreadyExistsButDifferent / no console errors.
>
> **REMAINING Phase 2 follow-ups (next iterations):**
> - **🔴 save→reload restores — BLOCKER: skinned reload renders empty, bone-ORDERING confirmed. The fix is
>   TRANSACTION-ALIGNED ordering, NOT re-materialisation (see the ⭐ TRANSACTION PRINCIPLE in §0).**
>   Tested via the headless in-memory round-trip: `window.wasmBindings.editor_dispatch_json('{"cmd":
>   "reload_project_in_memory"}')` (the `ReloadProjectInMemory` self-test — MCP-only, not in the palette;
>   serialize_inmem captures the rig glb via `rig_glb_files`, clears caches, re-applies). OBSERVED: scene
>   structure restores (fox = SkinnedMesh at origin, 1 mesh/1.2k tris) but the geometry is INVISIBLE,
>   "fox · 0 tris" → fell through to the FALLBACK template path. Per-frame anim `LocalNotFound(TransformKey)`.
>   - **CONFIRMED root cause (commit `1d4b1eca` added diagnostics):** bone-ORDERING. The rig glb input is now
>     ready pre-apply (stored in `restore_skinned_templates` — input availability fixed, the transaction-aligned
>     "declare dependency first" half), so `get_rig_node_decode` succeeds — but the warn fires:
>     `raw_mesh_from_rig: bone node ... (rig joint idx 1) not yet in bridge — falling back`. The SkinnedMesh
>     node materialises BEFORE its bone scene-nodes (under sibling `root`) have their `bridge.nodes` entries,
>     so the joint→TransformKey map can't resolve. At IMPORT the new path fires (bones ready by then — async
>     order differs); on reload it doesn't.
>   - **Fix direction = the transaction model (do it the RIGHT way, NOT a re-materialise hack):** establish
>     the transform hierarchy (bones / all scene-node `TransformKey`s) BEFORE the geometry that references
>     them, within ONE load — i.e. the editor's scene LOAD should be one `begin_load → declare transforms,
>     then geometry/materials/skins → commit_load` transaction (which also fixes the per-node-commit
>     anti-pattern). Concretely: a transforms-first pass over the loaded tree so every bone's `bridge.nodes`
>     entry + renderer `TransformKey` exists before any SkinnedMesh node's geometry is added; THEN one
>     commit. Do NOT add the skinned node broken and re-materialise it once bones land (that's the forbidden
>     post-hoc re-materialisation). The animation `LocalNotFound` is the same ordering race (clip lowers
>     against a not-yet-established bone key) — the same transforms-first ordering resolves it.
>   - HEAD currently regresses skinned reload — fix (the transaction-aligned way) before declaring Phase 2 done.
> - **teardown skin/geometry cleanup (step v)** — repeated edits re-insert the skin/geometry each
>   re-materialise; `skins::remove` + caching `skin_key` per (source,node,prim) to REUSE (DECISION) not yet
>   done → potential leak on repeated flips. Verify with memory_stats / `?stress`, then add cleanup + reuse.
> - **morph-via-rig (step vi)** — morph-only nodes still use the legacy template path; decode morph from the
>   rig glb into `RawMorph` so they go node-owned too, then DELETE the legacy `materialize_skinned_from_template`.
> - **rename** `repopulate_skinned_template` → "materialise skinned from our-format glb" (fold in).
> - **(Original plan retained below for reference.)**
>
> Goal: make a skinned drawable NODE-OWNED + rebuildable so a material opaque↔blend flip re-renders (the
> known vanish bug). The materialiser builds it from the rig glb via `add_raw_mesh(RawMeshData{ skin:
> Some(RawSkin) })` instead of reusing the populate template via `set_mesh_material`.
> - **Bedrock landed this loop:** `ExtractedSkin::packed_index_weights()` (`e8914c6f`) packs a decoded skin
>   into `RawSkin.index_weights` (interleaved u32-idx/f32-weight per influence — matches `convert_skin`).
>   `extract_node_mesh` already returns geometry + `ExtractedSkin` (joint_node_indices, IBMs, joints,
>   weights). So a `RawSkin` is: `{ joints: <editor TransformKeys>, inverse_bind_matrices: <IBMs as Mat4>,
>   set_count: 1, index_weights: skin.packed_index_weights() }`.
> - **THE NODE-INDEX-SPACE PREREQUISITE (confirmed, do FIRST).** `SkinnedMeshRef.joints` is
>   `Vec<SkinJoint{ node: NodeId (editor bone), index: u32 (RIG-GLB flat idx) }>` — so joint→TransformKey
>   mapping is available (bone NodeId → `bridge().nodes[node_id].transform_key`). BUT `skin.node_index` is
>   the ORIGINAL glTF node index (import builds the template/bones from the ORIGINAL), so the rig-glb decode
>   can't locate the skinned MESH node by it. Two ways: (1, DECISION's choice) unify import onto the rig
>   glb so `skin.node_index` IS the rig-glb index — bigger restructure of `import_typed`/`finish_model_import`
>   (materials+anims still from the original; geometry+skeleton from the rig glb); or (2, smaller interim)
>   persist/cache `node_flat_indices` (original→rig-glb) per source so the materialiser translates
>   `skin.node_index` → rig-glb index. Prefer (1) per the DECISION, but (2) de-risks the first behavioural
>   increment. Either way the joint-index space inside the skin (per-vertex JOINTS_0 → `joint_node_indices`
>   order → `RawSkin.joints` TransformKeys) is self-consistent from the single rig-glb decode.
> - **Entanglement (already recorded): (b) needs (d).** `hide_template_meshes` leaves the populate SKINNED
>   mesh VISIBLE today (it IS the rendered geometry). A node-owned `add_raw_mesh(skin)` would DOUBLE the
>   geometry unless populate also stops showing the skinned drawable → hide/remove the template skinned
>   meshes when the node owns its own. Do both in one increment.
> - **Increment sketch (each live-verified — Fox deforms+animates, then flip opaque↔blend, then save/reload):**
>   (i) resolve the node-index space (prereq above); (ii) a helper `materialise_skinned_from_rig(source,
>   rig_glb_node_index, prim, joint_map) -> RawMeshData{skin}` (decode + pack, cache the per-node decode in a
>   thread-local like `skinned_bake_cache`); (iii) rewire `materialize_skinned_mesh` → build node-owned via
>   `add_raw_mesh` with the CURRENT material, push to `model_meshes`, DELETE the `set_mesh_material`-on-template
>   branch; (iv) hide/remove the populate skinned template meshes (no double-geometry); (v) teardown cleans
>   skin/morph/geometry (`skins::remove` exists); (vi) morph-weight anim rebind on rebuild (reuse the static
>   morph approach). Skeleton + skeleton-animation stay persistent (joint TransformKeys unchanged).
> - **Rename:** fold `repopulate_skinned_template` INTO this as "materialise skinned from our-format glb"
>   (drop the "re"/"populate" naming) — used uniformly for first-show / reload / re-materialise.

- **Phase 2 — Capture skinned/morphed imports as per-node editable content; one materialise path.**
  *(Investigated; de-risked plan below. This is the large, high-regression-risk step — see "Risk
  checkpoint".)* The skinned source-of-truth (the rig glb) is already retained + persisted
  (`assets/<id>.rig.glb`), so **NO project-format migration is needed** — do NOT extend the `CapturedMesh`
  / `.mesh.bin` bitcode (positional, non-versioned → would break old saves). Instead:
  - ✅ **(a) DONE (commit `6f1a538c`)** — glb-export `extract_node_mesh` now returns an additive
    `skin: Option<ExtractedSkin>` (per-vertex joints+weights set 0, vertex-aligned; joint node-indices +
    inverse-bind matrices from `node.skin()`). In-memory only; isolated + unit-tested. (Morph capture +
    multi-skin-set are follow-ons within (b) when wiring morphed/multi-set skinned meshes.)
  - **(b) Re-wire `node_sync::materialize_skinned_mesh`** to build a `RawMeshData { skin: Some(RawSkin{..}),
    morph: .. }` per primitive from the rig-glb decode (cache the decoded per-(source,node,prim)
    MeshData-with-skin in a thread-local like `skinned_bake_cache`, to avoid re-decoding per edit) + the
    CURRENT material, and `add_raw_mesh` it PER-NODE (owned by the node → teardown removes it, re-fire
    rebuilds) — DELETE the `set_mesh_material`-on-shared-template branch. The skinned meshes become
    node-owned (added to `model_meshes`) instead of template-owned/survive-teardown.
  - **(c) Map the rig's joint node-indices → the editor's existing skeleton `TransformKey`s** (via the
    template's `node_index_to_transform`) so the `RawSkin.joints` re-bind to the SAME persistent skeleton;
    animation channels target those joint nodes (unchanged). Reuse the
    `repopulate_skinned_template` / `restore_skinned_templates` machinery for the mapping.
  - **(d) `populate_gltf` becomes a pure importer** feeding the one producer (no editor-special / hidden
    meshes).
  - *Acceptance:* a skinned mesh's opaque↔blend material flip re-renders (no vanish, no
    `VisibilityGeometryBufferNotFound`); skinned imports + animation still deform; one materialise path;
    Fox/DamagedHelmet/skinned/morph/primitive all render via it.
  - **NEW WRINKLE found (record per the guardrail):** re-materialise mints a NEW `geometry_morph_key`, so
    any MORPH animation channel bound to the old key (`populate_gltf_animation_morph`) must REBIND on
    rebuild — the skeleton/joint case is fine (persistent `TransformKey`s) but morph-weight animation is
    keyed to the geometry. Static captured meshes already re-materialise on edit, so check whether the
    editor already rebinds morph animation there (reuse it) or whether this is a pre-existing gap to
    handle. Resolve within (b)/(c); if it needs an animation-binding redesign, record options here.

> **⚠ Risk note (Phase 2).** This rewires the editor's skinned + morph + animation materialise path —
> core, high-blast-radius systems whose correctness needs LIVE verification with a
> skinned+morphed+animated model (import, deform, flip, save→reload).

### Phase 2 (b)+(c)+(d) — DECISION: option 1 (full unification), grouped. SANCTIONED, proceed.

> Human chose **option 1** (full unification) + explicitly OK'd grouping b+c+d (and relevant 3/4
> consolidation) into one coherent change to avoid technical debt. High-level goals to honour: ONE way to
> do things (debug/optimise in one place); fix the non-transactional perf problems; NO perf regressions;
> reduce resource consumption; the coherent "scene editor → our format → player/editor" flow — **while
> model-tests keeps working for plain-GLB import.**
>
> **Refined shape — EVERYTHING through the re-exported rig glb (the human's call; merges Phase 2 + 3):**
> The rig glb is the editor's ONE canonical geometry source (incl. skin + morph). The editor builds,
> captures, re-materialises, persists, and MCP-edits skinned geometry from it — one decode path, errors
> surface in one place, and what's on disk == what renders == what MCP edits.
> - **glTF is import-only (§0):** the renderer renders OUR format (clean glb), never glTF directly. The
>   `renderer-gltf` crate splits into an IMPORTER (glTF → clean glb + materials + clips) and an
>   our-format RENDERER (clean glb → `GeometrySource` → the transaction). Phase 2 does this for the
>   EDITOR's skinned content first; **model-tests routes through the SAME import→our-format→render too**
>   (in-memory) in the model-tests phase below — it is NOT a permanent plain-GLB exception, just sequenced
>   after the editor unification.
> - **At import the editor reexports the original → rig glb (already done), then builds its skinned
>   renderables FROM the rig glb** (decode it → skeleton transforms + skin + geometry); materials +
>   animation clips are still extracted from the original into the editor library/clips (the rig glb is
>   geometry-only). The existing `repopulate_skinned_template` already builds a template from the rig glb —
>   reuse/extend it as the ONE skinned build path (import AND reload AND re-materialise all use it).
> - **Skin inserted ONCE from the rig glb, `skin_key` cached per (source,node,prim) and REUSED** on every
>   (re-)materialise (the skin = skeleton + per-vertex weights, both fixed by the rig glb). Geometry reps
>   rebuild with the CURRENT material's kind, referencing the stable `skin_key`. So re-materialise = decode
>   rig-glb geometry (cache the decode per node) + `register_geometry`(skin_key reused) + `add_mesh`(current
>   material) + commit — through the ONE producer (`apply_kind`). **Delete `set_mesh_material`-on-template.**
> - **IBM conflict dissolved by construction:** a SINGLE IBM source (the rig glb). `reexport_clean` is a
>   deterministic pure transform, so every decode yields identical IBMs; inserting the skin once (or
>   re-inserting bit-identically) never trips `Skins::insert`'s `JointAlreadyExistsButDifferent`. (The
>   earlier "use the original decode's IBMs" idea is REJECTED — it reintroduced a second source.)
> - **Make the skinned drawable node-owned** (`model_meshes`) so teardown+rebuild works like static; no
>   double-geometry (don't also keep a populate-built skinned drawable for editor nodes). Clean up the
>   per-node geometry/morph on teardown; the cached `skin_key` is source-level (like the skeleton) and
>   lives as long as the source is referenced (reuse the existing reclaim-guard).
> - **Skeleton + animation:** joints are persistent scene `TransformKey`s from the rig-glb build; skeleton
>   animation stays bound. Map animation-channel joint targets to the rig-glb-built skeleton (the template's
>   `node_index_to_transform`). **Morph-anim rebind** wrinkle: re-materialise mints a new morph key → rebind
>   the morph channel (check/reuse how static captured morphed meshes handle it).
> - **This IS Phase 3 for geometry:** the rig glb = the geometry container of "our format"; materials +
>   clips are the separate sidecars (already separate). Group them. Add byte-fidelity round-trip tests +
>   a `?stress`/`?trace` perf check (no regression; transactional; reduced resource use via no
>   double-geometry + one decode).
>
> **Implementation findings (turn after Phase 2a — affect HOW, not WHETHER):**
> - **Foundational prerequisite — NODE-INDEX SPACE consistency.** `reexport_clean` flattens/renumbers
>   nodes, so the rig glb's node indices ≠ the original glTF's. `ExtractedSkin.joint_node_indices` (2a)
>   are RIG-GLB indices. Today the editor builds the skinned template from the ORIGINAL at import but from
>   the RIG GLB on reload (`repopulate_skinned_template`). To capture/re-materialise skinned from the rig
>   glb consistently, the EDITOR'S IMPORT must ALSO build the skinned template + node refs from the rig glb
>   (unify import onto `repopulate_skinned_template` → import AND reload AND re-materialise share ONE
>   node-index space). This is the first sub-step; it's behavioural so needs live skinned verify.
> - **Verification reality.** Acceptance is visual+behavioural (a rig must DEFORM + ANIMATE, then flip/
>   save-reload). `cargo test` can't catch skinned-render regressions (WebGPU-only, no GPU tests). Live
>   verify needs a rigged glb imported INTO the editor; the editor import takes a URL and Fox (skinned) is
>   fetchable at http://localhost:9080/media/glTF-Sample-Assets/Models/Fox/glTF-Binary/Fox.glb, but cross-
>   origin (:9085←:9080) may hit CORS, and judging skeletal DEFORMATION from a screenshot is unreliable.
>   ⇒ best CO-DRIVEN with a human glancing at deformation. `skins::remove` + rig-glb/bind-pose caches exist.
> - **Autonomous-friendlier alt track:** Phase 5 (route model-tests through import→our-format) is
>   screenshot-verifiable on the model-tests canvas (reliable all session) + foundational (the importer/
>   renderer split serves the editor too) — but carries its own material-extension round-trip risk.
>
> **Verification (live, with a rigged asset):** import a skinned model (deforms + animates), flip its
> material opaque↔blend (re-renders, no vanish / no `VisibilityGeometryBufferNotFound`), save→reload
> (restores). Fox/DamagedHelmet regression-clean. Find a rigged glb in test-assets / model-tests
> collections (CesiumMan / RiggedFigure / RiggedSimple, or Fox if skinned).

Tracing the live skinned flow surfaced the failure modes the above shape avoids. Code-verified findings:

- **populate builds + SHOWS the skinned drawable today.** `hide_template_meshes` hides only the
  *non-skinned* populate meshes; the **skinned** ones stay visible and ARE the rendered geometry, reused
  by `materialize_skinned_mesh` via `set_mesh_material`. So a per-node `add_raw_mesh(skin)` can't just be
  *added* — it would DOUBLE the geometry unless populate also stops building/showing the skinned drawable.
  ⇒ **(b) is entangled with (d)** (can't do one without the other).
- **Skin re-insert conflict.** `Skins::insert` has a `JointAlreadyExistsButDifferent` guard. populate
  already inserts the skin for the node's joint `TransformKey`s; a per-node `add_raw_mesh(skin)` re-inserts
  a skin over the SAME joint keys → matches only if the rig-glb-reexported inverse-bind matrices are
  bit-identical to populate's originals. `reexport_clean` may perturb them ⇒ a real
  `JointAlreadyExistsButDifferent` failure mode that only shows at runtime.
- **populate-as-pure-importer is itself large.** populate currently sets up the skeleton transform tree
  + skin store + animation channels AND the drawable skinned mesh. "(d) pure importer" means it must still
  build the skeleton/skin/animation but NOT the drawable — a non-trivial split of the import pipeline.
- **Morph-anim rebind** (already-recorded wrinkle) compounds this for face rigs.

These are the failure modes the **DECISION shape above avoids** by routing EVERYTHING (editor) through the
single re-exported rig glb: plain-GLB `populate` stays only for model-tests; the editor builds + captures +
re-materialises skinned content from the rig glb (one deterministic IBM source → no dedup conflict); skin
inserted once + `skin_key` reused; one materialise path; node-owned drawable (no double-geometry). Proceed
with that shape — no longer a blocker. (Resolved: option 1, full unification, grouped, all-rig-glb — see
the DECISION block. The earlier 3-option list + the "original-decode IBMs" idea are settled/rejected.)

- **Phase 3 — The proprietary save format (geometry-glb + materials sidecar + animation clips).**
  Define + implement the editor's persistent format: per-asset geometry glb (via `awsm-glb-export`,
  skins/morphs included, NO materials baked) + a materials sidecar + the authored clips. Save = export
  authored content; Load = import the glb as geometry-only + bind the sidecar materials (generalise the
  bundle loader's materialless-glb + `Single`-material path) + load the clips. Re-materialise = re-import
  the asset's own geometry-glb.
  - *Acceptance:* save → reload a scene (static + skinned + morphed + animated + materials) round-trips
    visually identically; editing a material or clip does not touch geometry and vice-versa.

- **Phase 4 — Make the round-trip the only path + verify.**
  Editor edits write through to the authored glb; re-materialise always re-imports. Add lossless
  round-trip tests (geometry / skin / morph byte-fidelity through export → import). Perf pass (incremental
  re-materialise / cached decoded source for big rigs).
  - *Acceptance:* byte-fidelity round-trip tests green; no second materialise path exists; re-materialise
    of a large rig is acceptably fast (or incremental).

- **Phase 5 — glTF is import-only EVERYWHERE: split `renderer-gltf` into importer + our-format renderer; route model-tests through it (§0).**
  Today `populate_gltf` renders glTF directly (gltf → renderer meshes). Make the renderer render ONLY our
  format: (i) an IMPORTER `glTF bytes → our format` = `reexport_clean` (clean glb) + `extract_material_specs`
  (materials) + `extract_animations` (clips); (ii) an our-format RENDERER `clean glb → GeometrySource →
  transaction` (this is `populate_gltf` re-pointed at the clean, materialless glb) + bind the extracted
  materials (the materialless-glb + per-node-material pattern the bundle loader already uses) + load the
  clips. Route **model-tests** through import→our-format→render **in-memory** (it stays a glTF viewer; it
  just converts first). No code path renders glTF directly after this.
  - *Acceptance:* model-tests renders every sample (incl. transmission/clearcoat/sheen/etc.) via the
    our-format path — materials + animation intact, regression-clean vs today; ONE renderer (our format),
    glTF only at the import boundary; no perf regression on cold load (`?trace`).
  - *Note (perf/scope):* reexport-on-every-model-load is CPU work — acceptable for a viewer, but measure;
    if a sample regresses, record it. The material-extension round-trip (all KHR_* the test models use) is
    the main surface — verify each renders identically.

## 4. Decisions (resolved — these are NOT open; implement as written, no stopping to ask)

- **Skin/skeleton identity across re-import — DECIDED.** The skeleton (joint scene nodes) + the animation
  channels targeting them are PERSISTENT and never recreated by re-materialisation. Rebuild only the
  geometry+skin weights and re-bind to the existing joints, keyed by
  `skin.source`/`node_index`/`primitive_index` (stable) — the exact pattern
  `restore_skinned_templates` → `repopulate_skinned_template` already uses on project reload. No new
  TransformKeys, no stranded animation. (This was the scariest risk; the existing reload path already
  solves it, so just reuse it.)
- **Performance — DECIDED: full re-materialise, optimise only if measured.** Default-equals-today: the
  static-capture path already fully re-uploads on every edit, so a skinned mesh doing the same is
  acceptable. Do NOT pre-build incremental re-materialise / caching. If `?stress=N` + `?trace=sub-frame`
  shows a real stall on a big rig, add a cached decoded source then (the editor already owns the rig glb
  bytes, so caching is local) — but only when a benchmark proves it's needed.
- **Round-trip fidelity — DECIDED: pin it with tests, not discussion.** Add byte-fidelity proptests
  across `glb-export` ↔ the decode for skins/morphs (joint order, inverse-bind matrices, morph-delta
  layout), mirroring the existing visibility/transparency packer-parity proptests. A failure is an
  ordinary bug to fix, not a design question.
  - ✅ **DONE (commit `71fd898e`)** — `glb-export/tests/roundtrip_proptest.rs` gained
    `skin_roundtrips_bit_exact` (per-vertex JOINTS_0/WEIGHTS_0 + skin joint flatten-index list +
    inverse-bind matrices) and `morph_roundtrips_bit_exact` (per-target position/normal deltas +
    default weights), both through `write_glb → reexport_clean`. Confirms export→import preserves
    skin + morph LOSSLESSLY — the fidelity the "everything through the clean glb" decision rests on.
    (The renderer-gltf buffer-decode leg — `into_data`/`primitive_buffer_info` — is GPU-buffer
    construction, covered by the live materialise verify, not this data-layer net.)
- **Save format — DECIDED: reuse the existing per-asset side-file scheme.** Geometry already persists as
  `assets/<id>.rig.glb` (skinned) and `assets/<id>.mesh.bin` (static); materials + animation clips already
  persist separately in the project. The epic CONSOLIDATES toward glb-for-all-geometry but does NOT
  require a new container format or breaking the project format — extend the existing side-file scheme.
  (If, in Phase 3, unifying static `.mesh.bin` onto glb turns out to need a project-format migration,
  that's the ONE thing worth a quick heads-up — note it in this doc and keep going on everything else.)

## 5. Standards gate (unchanged from the foundation work)

- Keep `cargo test -p awsm-renderer -p awsm-materials -p awsm-scene-loader --lib` GREEN and `task lint`
  clean per step; run `cargo test -p awsm-renderer-gltf` after any gltf change.
- default-equals-today; no per-frame heap allocs in the hot path; never-silent-cap; MSAA-compile
  invariant; `commit_load` stays atomic (no yields — see the scope note).
- Commit each coherent step with explicit paths (NEVER `git add -A`, NO backticks in `-m`), end messages
  with the Co-Authored-By trailer; do NOT push or open a PR.
- Verify visually via chrome-devtools (model-tests :9080, editor :9085) — navigate + screenshot to
  CONFIRM before trusting console.
- **Honour the ⭐ TRANSACTION PRINCIPLE (§0) at every step:** load = `begin_load → declare many ops (in
  dependency order: transforms before the geometry that references them) → commit_load`; the commit does
  the dedup/concurrency. NO post-hoc re-materialisation passes; NO per-operation commits; NO "smart"
  ordering/batching outside `commit_load`.

## 5b. FINAL REVIEW STEP (David, required — run at the END, before declaring the epic done)

Before calling the epic complete, do a dedicated review pass confirming the codebase actually works the
transaction way IN GENERAL (not just where this epic touched):

1. **Grep + read for transaction-principle violations** across the editor + loaders: per-operation
   `commit_load` calls inside a load loop (should be ONE commit per load); any "re-materialise" / "repopulate"
   / re-run-after-the-fact pass that patches an ordering/timing race (should be fixed by ordering the
   transaction); any ordering/dedup/batching done by a caller that `commit_load` should own. List each hit
   with a verdict (legit one-off live-edit vs a load that should be batched into one transaction).
2. **Confirm the editor's LOAD paths (import + reload) are single transactions** with transforms declared
   before geometry — the skinned-reload bone-ordering bug is the canonical test (must render + deform +
   flip + survive save→reload, all via ONE transaction, no re-materialise).
3. **Record findings** (fixed vs deferred-with-reason) in this doc; only declare the epic done once the
   load paths are transaction-shaped and the acceptance list passes.

## 6. Out of scope / tracked elsewhere

- **Worker-hosted renderer** (main-thread responsiveness; the loading-UI paint nuance) — `docs/plans/multithreading.md`.
- Minor model-tests picker quirks (`Sponza` / some names → "Not Found"; `IridescenceDishWithOlives`
  framing) — cosmetic/pre-existing.
