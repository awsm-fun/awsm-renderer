# Plan: one geometry flow — editor content as a source

**Remaining work.** The "geometry into the load transaction" foundation has landed (see *Already landed*
below). What's left is to make **editor content a first-class source**, so EVERY producer — primitives,
glTF imports, and editor-authored content (including skins + morphs) — flows through the *one* geometry
path. This dissolves the last special case (skinned meshes) and gives us one place to optimise + debug.

> Scope note: worker-hosting the renderer (main-thread responsiveness / the loading-UI paint nuance) is
> tracked separately in `docs/plans/multithreading.md` and is explicitly OUT of scope here. We are NOT
> changing `commit_load` to add mid-operation yields — the library should not add asynchronous jank for
> an application threading choice.

---

## 0. The north star

There must be **one way** geometry reaches the screen:

```
begin_load()  →  add sources  →  commit_load()
```

A "source" is a `GeometrySource` (geometry + optional skin + optional morph). EVERY producer lowers to
it. Today primitives/raw and the glTF decode do; **editor content does not** — it has two divergent
paths (static = captured + re-materialised; skinned = shares populate-built meshes). Close that.

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
> - **The plain-GLB `populate` path STAYS for model-tests / player-of-raw-glb only** (the sanctioned
>   exception — "model-tests works for plain GLB import"). The EDITOR no longer renders skinned content
>   from the original decode.
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

## 6. Out of scope / tracked elsewhere

- **Worker-hosted renderer** (main-thread responsiveness; the loading-UI paint nuance) — `docs/plans/multithreading.md`.
- Minor model-tests picker quirks (`Sponza` / some names → "Not Found"; `IridescenceDishWithOlives`
  framing) — cosmetic/pre-existing.
