# Mesh pipeline overhaul + skins/morphs first-class (overnight batch)

**Status:** ACTIVE. Branch `mesh-authoring`. Authored 2026-06-10 as the spec for an
autonomous overnight run. Commit incrementally; keep the tree compiling at every
commit. **Everything claimed "done" overnight must be `cargo test` / `cargo clippy`
verifiable** — in-browser render checks are DEFERRED to the user and must be
flagged as such in the morning report (never claimed verified).

This doc is the source of truth. Read it first. Conceptual content here is also
the basis for the user-facing `docs/buffers.md` (Phase 1).

---

## 0. Why we're doing this (root cause)

Every render bug in this thread traces to **two implementations of the same job**:
the editor renders imported meshes through `add_raw_mesh` (from captured
`MeshData`), the player through `populate_gltf` (from glb accessors). Two buffer
builders drift → divergence. The fix is **one conversion + one population path**,
with all the interesting logic moved *before* the GPU so it's property-testable
without a browser.

### The three representations (the core mental model — goes in `docs/buffers.md`)

1. **glTF/glb data** — encoded accessors. NOT mutable (can't sculpt packed bytes).
2. **`MeshData`** (`awsm_meshgen::MeshData`: positions/normals/uvs/colors/indices) —
   plain geometry arrays. Mutable → what editing operates on.
3. **The editor's editable *model*** — `MeshData` + modifier stack + per-vertex
   override layers + history. The heavy, editor-only part.

**Asymmetry (must be documented clearly, in `docs/buffers.md` AND code comments):**
- The **editor** needs (2)/(3) because it edits: `glb → MeshData → pack → GPU`.
- The **player** never edits, so materializing (2)/(3) is wasted work:
  `glb → pack → GPU` directly (this is what `populate_gltf` already does — it
  never builds a `MeshData`).
- **Why not standardize on `MeshData` everywhere?** It would force the player to
  materialize an editor-only form it never needs. The player thinking only in glb
  is the efficient choice.
- **Why does the player "know about" MeshData at all?** It doesn't, really — both
  front-ends funnel into ONE shared packer; the player feeds it decoded accessor
  data, the editor feeds it `MeshData`. Same bytes out → no divergence possible.

### Agreed architecture decisions (locked)

- **Shared `pack_mesh_buffers`**: extract visibility+transparency byte-packing +
  `MeshBufferInfo` construction into ONE function in `renderer`; both
  `add_raw_mesh`/`add_raw_mesh_transparent` and `renderer-gltf`'s
  `create_visibility_vertices`/`create_transparency_vertices` call it. Keystone:
  makes parity true by construction.
- **`awsm-gltf-convert`** (NEW crate, pure data — no `web-sys`, no renderer):
  `convert(bytes) -> CanonicalImport { glb, materials, images, animations,
  is_already_canonical }`. Detects-or-converts; the proptest centerpiece.
- **`AWSM_format` glTF extension (versioned, e.g. `{ "version": 1 }`)**: marks a
  glb as already-canonical (editor-saved). Present → pass through; absent →
  convert. Makes the round-trip idempotent. (Precedent: existing
  `AWSM_materials_none` extension convention.)
- **Canonical glb is COMPLETE**: bake tangents (MikkTSpace, pure CPU) + ensure
  normals during conversion, so population is a dumb byte-upload and tangent
  generation is covered by pure-data proptests. Editing regenerates tangents.
- **Do NOT merge multi-primitive nodes** in the converter (current
  `extract_node_mesh` merges — lossy for per-primitive materials). glTF supports
  multi-primitive natively; `populate_gltf` handles it.
- **Eager editability** (NOT lazy): convert-on-import → decode straight to
  editable `MeshData` → immediately editable. Safe *because* the packer is shared
  (editor's edit-time packing == player's load-time packing, same code). Only
  cost is the editor holding CPU `MeshData` for imports (normal for an editor);
  lazy-decode stays available as a pure future optimization.
- **Deletes the wasteful step**: today the editor calls `populate_gltf` only to
  bake textures, then HIDES those meshes and rebuilds via capture
  (`gltf.rs:284` populate, `gltf.rs:290` hide). With the shared packer + convert,
  the editor builds the editable mesh ONCE via the packer; textures upload at
  population. No populate-then-hide.

---

## 1. Already fixed this session (Phase 0 — commit first)

In-browser VERIFIED earlier this session; commit on `mesh-authoring`:
- **Visibility-buffer double-render fix** (`renderer/src/raw_mesh.rs`):
  `add_raw_mesh_transparent` was emitting BOTH visibility + transparency geometry
  → transmission meshes rasterized as opaque occluders. Now transparency-only
  (visibility `None`), mirroring `populate_gltf`'s `mesh_buffer_geometry_kind`
  (transmission/blend/mask → `Transparency`).
- **Tangent generation** (`raw_mesh.rs`): was synthetic `[0,0,0,1]`; now real
  MikkTSpace via `RawMeshData::compute_tangents` + `material_wants_tangents`
  gating (normal map present), matching `renderer-gltf`'s `ensure_tangents`.
- **Transparent shadow default** (`raw_mesh.rs`): transparent meshes default to
  `MeshShadowFlags::TRANSPARENT_DEFAULT` (no cast/receive) — they have no
  visibility geometry, so the shadow pass would otherwise look up a missing
  buffer.
- **env-from-URL MCP capability**: new `EditorCommand::ImportKtxEnvFromUrl`
  (`editor-protocol/src/command.rs`, handler in `controller/state.rs`,
  `activity_feed.rs`); `set_environment` MCP tool (`mcp/src/mcp.rs`) now accepts
  `builtin` / KTX UUID / `https://…ktx2` URL for skybox + both IBL maps. Verified
  end-to-end loading PhotoStudio from the CDN.
- **glb geometry round-trip proptest** (`glb-export/tests/roundtrip_proptest.rs`,
  `proptest` wired as workspace dev-dep). Bit-exact MeshData → glb → MeshData.

---

## 2. Phased execution plan

### Phase 1 — `docs/buffers.md` (write FIRST, it's the spec)
The three representations + the asymmetry + why-not-standardize-on-MeshData + the
shared packer + convert pipeline + `AWSM_format` + eager editability. Plus terse
comments at each seam (`pack_mesh_buffers`, `convert`, `add_raw_mesh`,
`populate_gltf`, the editor import). Reviewable in the morning even if code is
partial.

### Phase 2 — Shared `pack_mesh_buffers` (keystone)
- Extract visibility (56B/exploded vtx) + transparency (40B/vtx) packing +
  `MeshBufferInfo` into one fn in `renderer` (callable from `renderer-gltf`).
- Route `add_raw_mesh`/`add_raw_mesh_transparent` and `renderer-gltf`'s vertex
  builders through it.
- **Byte-identity test**: old packing == new packing (proves behavior-preserving).
- Editor-input-vs-gltf-input parity proptest (same geometry → identical bytes).
- Verification: pure Rust ✅.

### Phase 3 — `awsm-gltf-convert` crate (pure data)
- Scaffold crate (no web-sys/renderer deps).
- `AWSM_format` read/write (versioned).
- `convert(bytes) -> CanonicalImport`: detect-or-convert; strip materials/anims/
  unused; bake tangents + ensure normals; keep primitives un-merged; emit
  canonical glb + extracted material defs + image byte-blobs + animation clips;
  stamp `AWSM_format`.
- Move PURE extraction logic out of the editor bridge (`engine/bridge/gltf.rs`'s
  `extract_material_specs`/`extract_extensions`/`extract_animations`) into here.
  Texture *image bytes* are pure data (extract); GPU upload stays in population.
- Proptests: idempotency (`convert(convert(x))==convert(x)`,
  `convert(canonical)==canonical`), round-trip, extraction fidelity, "any foreign
  glTF converts without panicking."
- Verification: pure Rust ✅.

### Phase 4 — Wire editor + player onto convert + shared packer
- Player (`scene-loader`): route through convert (if needed) + shared packer.
- Editor (`engine/bridge/gltf.rs` + `node_sync.rs`): import → convert → eager
  editable MeshData → shared packer; delete populate-then-hide; export stamps
  `AWSM_format`.
- **RISK**: browser-dependent render verification. Make it COMPILE + lint; commit
  separately; flag clearly as needs-your-eyes. Do NOT claim render-verified.

---

## 3. Skins & morphs first-class (NEW — user-requested)

Make skins/morphs first-class, **edited strictly through MCP** (mirror the
mesh-MCP philosophy: pull out the stops, use third-party crates, no
human-ergonomic constraints — empower an agent to do rich skin/morph/animation
work via prompting). The ONE human-GUI exception: moving a joint node's transform
(it's just a regular transform).

### Phase 5 — Skin/morph MCP editing backend
- The **inverse of the mesh edit-guard**: we strip skins/morphs to edit geometry;
  here we edit the skin/morph DATA itself on the bound/flattened mesh. So MCP
  tools for: joint weights (per-vertex `JOINTS_0`/`WEIGHTS_0`), bind poses /
  inverse-bind matrices, joint hierarchy, morph-target deltas + names + default
  weights, live morph weights, and skeletal/morph animation authoring.
- Evaluate third-party crates: IK solver (for "cool" pose adjustments), weight
  smoothing/normalization, retargeting, blend-shape utilities. (Pick at design
  time; note choices in the doc.)
- New `EditorCommand`s + MCP tools + `EditorQuery`s (read-back to verify, like the
  mesh tools: get skin/joint data, morph data). Compile + unit-test the command
  layer; structured-output schemas for the MCP tools.
- Verification: command/MCP layer is Rust ✅; visual correctness DEFERRED.

### Phase 6 — Skin/morph visualization (editor UI)
- Bone icons in the outliner for joint/skin nodes (if absent).
- Visualize skins (skeleton/bone lines) + morphs, **including during animation
  playback**.
- **RISK**: editor UI, browser-verified. Build it (compiles); flag for review.

---

## 4. Phase 7 — Quality sweep
- Doc-comment sweep across touched crates (and beyond where thin).
- **MCP fidelity**: audit tools (coverage, truthful descriptions), resources
  (docs/prompts/templates exposed over MCP), and documentation completeness.
- Code cleanup + comments at non-obvious seams.
- Verification: compile/clippy + doctests ✅.

---

## 5. Phase 8 — Dish shading analysis (code-level, no blind fix)
Reference: `/Users/dakom/Downloads/olives.png` (Khronos viewer + "color photo
studio" IBL). Target appearance: **clear refractive glass dome** with a *subtle*
pink-violet thin-film sheen near the crown; **clean warm gold metal bowl**
(`goldLeaf`, no iridescence); olives glossy/detailed. Iridescence is UNDERSTATED.

Our renders diverge two ways: model-tests went white on the bowl top; the editor
showed OVER-STRONG green/rainbow iridescence on the dish. Both implicate the
**iridescence path**, not transmission (fixed). Investigate the thin-film
iridescence shader math + its Fresnel weighting + compositing order over
transmission and the underlying metal, diffed against the glTF
`KHR_materials_iridescence` spec and this reference. Deliver a written diagnosis
with suspected root cause(s) + proposed fix; no blind edits.

---

## 6. Conventions & guardrails
- Branch: **`mesh-authoring`**. Commit incrementally, clear messages, tree
  compiles at every commit (bisectable). End commit messages per CLAUDE.md.
- **Never write the banned project codename** or its repo path into committed files (see memory).
- Overnight = cargo-verifiable only. Anything needing the browser: build it,
  commit it separately, flag it. **Report outcomes faithfully** — split
  "cargo-verified" vs "needs your eyes" in the morning report.
- The user will add MORE work; fold new items in as Phases 9+.

## 7. Morning report must contain
Per phase: what landed, commit hashes, what's `cargo`-verified, what needs
in-browser verification, what's scaffolded/partial, and any decisions/blocks
encountered.

---

## PROGRESS LOG (overnight run, newest notes at bottom)

Sequencing the run by value×safety (zero-risk/completable first; hot-path + browser
work deferred). Done so far, all `cargo`-verified + committed on `mesh-authoring`:

- **Phase 0** ✅ — committed the session's in-browser-verified work in 4 commits:
  `b165cdaa` (renderer transmission/tangent/shadow fix), `3b6fae5c` (env-from-URL
  MCP), `df42cfc7` (glb round-trip proptest), `94463275` (this plan doc).
- **Phase 1** ✅ — `docs/buffers.md` written + committed (`afea4b66`).
- **Phase 8** ✅ (analysis) — `docs/iridescence-analysis.md` committed (`85adb942`).
  Prime suspect: the 3-wavelength two-beam thin-film approx in `brdf.wgsl` vs the
  spec's spectral→RGB (Belcour-Barla/`evalSensitivity`). Ruled out texture
  extraction + thickness mapping. FIX needs render verification.

- **Phase 3** 🔨 IN PROGRESS — new crate `awsm-gltf-convert` (decision: separate
  crate depending on glb-export, NOT a module inside it — clean boundary so both
  editor + player can depend without glb-export's export surface). Increment 1
  committed (`8b943443`): `AWSM_format` (versioned) + `is_canonical` + `convert()`
  geometry path (reuses `reexport_clean_scene`/`write_glb`); 2 unit tests green.
  ✅ Increment 2 committed (`6d8dc9f9`): `AWSM_format` STAMPING via JSON-chunk
  surgery (`stamp_awsm_format`, `gltf::binary::Glb`) + `awsm_format_version` read —
  idempotency works. ✅ Proptests committed (`7ed5c49b`): geometry-preservation +
  idempotency over arbitrary meshes (256 cases each, green).
  REMAINING increments (each documented in `gltf-convert/src/lib.rs`, do in order):
  1. ~~Stamp AWSM_format~~ ✅ DONE.
  2. **Bake tangents + ensure normals** into the canonical glb — needs
     `MeshData.tangents: Option<Vec<[f32;4]>>` + a `TANGENT` accessor in
     `write_glb`, then bake via bevy_mikktspace in `convert` (reuse the mikktspace
     adapter from `renderer/src/raw_mesh.rs` `TangentGeometry` — consider lifting
     it to a shared spot). Bake whenever normals+uvs exist (materials are
     stripped, so can't gate on normal-map presence — over-bake is harmless).
  3. **Extract materials + animations** — move the PURE logic out of the editor
     bridge (`engine/bridge/gltf.rs`: `extract_material_specs`/`extract_extensions`/
     `extract_animations`) into `gltf-convert`; image bytes are pure data, GPU
     upload stays in population. Populate `CanonicalImport.materials`/`.animations`.

- **Phase 3 increment 2 (tangent-baking)** ✅ committed (`feat(glb-export): bake
  MikkTSpace TANGENT`): `glb-export/src/tangents.rs` (pure mikktspace) + `write_glb`
  now emits a `TANGENT` accessor from normals+uvs. Canonical/exported glbs are now
  self-contained. Native tests green. ⚠️ changes editor bundle-export output (every
  glb carries TANGENT now — additive/standard, but wants an in-editor export→player
  visual confirm).
- **Phase 2 (shared packer)** ✅ KEYSTONE committed (`refactor(renderer): extract
  shared mesh_pack`): `renderer/src/mesh_pack.rs` (`pack_visibility_bytes` /
  `pack_transparency_bytes`); `add_raw_mesh`/`add_raw_mesh_transparent` route
  through it. Behavior-preserving literal move; compiles (wasm). ⚠️ renderer is
  wasm-only-testable so the byte-layout tests compile but don't run under bare
  `cargo test`.

### REMAINING WORK (fresh-context continuation; newest state above)
- **Phase 3 material + animation extraction** ✅ MOSTLY DONE (committed):
  `gltf-convert` got its own neutral structs (decision taken: decoupled from BOTH
  editor-protocol AND scene). `materials.rs`: base PBR + standard texture slots +
  all KHR extension FACTORS (`MaterialSpec`/`MaterialExtensions`). `animations.rs`:
  `AnimationSpec` (raw sampler data, via the gltf crate's pure channel reader).
  `CanonicalImport.materials`/`.animations` populated. Tests + clippy green.
  ✅ images DONE: `CanonicalImport.images` carries raw encoded PNG/JPEG bytes
  (`images.rs`, View/GLB-embedded source); convert() switched to
  `Gltf::from_slice` + `import_buffers` (no image decode — robustness + speed).
  **The convert crate is now DATA-COMPLETE** (geometry + materials + animations +
  images), all proptested.
  REMAINING sub-items (lower priority): extension TEXTURE refs on MaterialSpec
  (factors only today); `data:`-URI image bytes (needs base64 dep); sampler +
  KHR_texture_transform on `TexRef`.

  ✅ convert crate also PROPTESTED beyond geometry: material-factor survival +
  animation-sampler survival (`tests/convert_proptest.rs`). The convert crate is
  DONE for the autonomous run.

- **NEXT for the autonomous loop:** the remaining HIGH-value work is browser-gated
  (Phase 4/5/6 wiring + skin/morph visuals + Phase 2b). Safe autonomous work left:
  ✅ (a) tangent-generator consolidation DONE (`awsm-tangents` crate; renderer +
  glb-export share it; renderer-gltf byte variant is the remaining follow-on).
  ✅ extension TEXTURE refs DONE (`MaterialSpec.extension_textures`) — material
  extraction is now feature-complete (base PBR + all KHR factors + textures).
  ✅ `data:`-URI image bytes DONE (base64). **The convert crate is now
  FEATURE-COMPLETE** (geometry + full materials + animations + images).
  (c) LEFT (genuine but smaller): Phase 7 doc/MCP fidelity sweep; when this turns
  marginal the loop posts the morning report — the big features need the browser.
  These are GENUINE but smaller; when they run dry the loop should STOP and post
  the morning report rather than manufacture busywork — the big features need the
  user + browser.
  **Phase 5 skin/morph:** READ-BACK queries safe; MUTATING tools additive but
  visual-correctness = "needs your eyes". Full value wants the user present.
- **Phase 2b — gltf unification — ⚠️ DEFER (needs your eyes):** route
  `renderer-gltf`'s `create_visibility_vertices`/`create_transparency_vertices`
  through `mesh_pack` (decode attribute byte-maps → typed slices; thread
  `front_face` into `pack_visibility_bytes`). It changes how EVERY gltf mesh is
  packed; renderer-gltf is wasm-only-testable so a byte mistake can't be caught
  by native `cargo test` and would break all rendered models. The autonomous loop
  should NOT attempt this blind — do it with the user present to verify a render.
  (The shared packer already exists and is wired on the raw-mesh side; this is
  just the second caller.)
- **Phase 5 — skin/morph MCP backend (USER PRIORITY).** Landscape surveyed:
  morph already exists as an ANIMATION TRACK target (mcp.rs add_track
  `morph(node,index)`); `drop_skinning` bakes skin→editable; scene types
  `SkinnedMeshRef`/`SkinJoint` in `scene/src/tree.rs`. MISSING (build as NEW
  commands+tools+queries, additive/safe at the command layer, visual = browser):
  live `set_morph_weight(node,index,value)` + `get_morph_data` query (target
  count/names/current weights); skin joint-weight / bind-pose editing; richer
  skeletal/morph animation authoring. "Pull out the stops, 3rd-party crates (IK,
  weight-smoothing, retarget), no human-ergonomic constraints." Find the renderer
  morph-weight API + how the animation morph track drives it, mirror that.
- **Phase 7 — sweep** (doc comments, MCP tool/resource/doc fidelity, cleanup).
  Also CONSOLIDATE the now-THREE mikktspace tangent generators (renderer
  `raw_mesh::TangentGeometry`, `glb-export::tangents`, `renderer-gltf::ensure_tangents`)
  into one shared home — tricky because `renderer` deliberately avoids depending on
  `meshgen`; consider a tiny pure `mesh-buffers`/`tangents` crate they all use.
- **Phases 4 (wiring) + 6 (visualization)** — build-but-don't-claim (browser
  verification needed).

### Phase 9 — STANDING LATITUDE (opportunistic, runs the loop dry slowly)
Once the listed phases are progressing/done, keep finding valuable work each
iteration — the loop should NOT stop early. Broad mandate from the user, with
guardrails:
- **Code + docs cleanup**: dead code, confusing names, missing/clarifying doc
  comments on code you touch, README/doc drift, TODO triage.
- **Efficiency gains**: implement ones you spot — but ONLY when behavior-preserving
  (or proptest/byte-identity-guarded). NO perf regressions; don't micro-opt a
  render hot path on a hunch without a measurement or a guard; flag anything that
  could change rendered output for browser verification.
- **MCP robustness + helpers**: better error messages, input validation,
  idempotency, truthful tool/resource descriptions, and NEW query/tool helpers
  that make agent-driving easier (e.g. richer read-backs, batch ops, safer
  defaults). Keep the tool layer compile/clippy-clean.
- **Mesh / editor capabilities**: new useful mesh ops, editor tools, and MCP
  capabilities you think of — additive, tested at the command/cargo layer; flag
  visual/browser bits.
Always: cargo-verifiable, small incremental commits, tree compiles at every
commit, never claim render-verified what isn't, log notable adds in this progress
section. Prefer high-value/low-risk; when unsure whether a change is safe without
the browser, build it behind a flag or leave a note rather than risk a regression.

---

## SESSION HANDOFF (2026-06-11, interactive) — read `docs/plans/OVERNIGHT-HANDOFF.md`

Landed this session (all committed on `mesh-authoring`, fmt+compile clean; shadow/cutout
items BROWSER-VERIFIED live via the `:9086/debug` relay):
- Editor fix batch #14–#18: multi-node drag-reparent into Empty (`d623ca5b`); light-gizmo
  settings toggle + drag-to-scrub numeric inputs (`65b63041`); **bulb-glyph light icons +
  direction rays** replacing the cyan-sphere marker (`f0dd0421`).
- Shadows: Soft penumbra tamed + **PCSS acne killed**, unified per-light **Softness** knob
  (`pcss_penumbra_scale` now drives Soft AND PCSS; world-sized→texel→scale-invariant) (`cf352b30`);
  **double-sided shadow casters** via `CullMode::None` so thin cutout panels/planes cast
  hole-shaped shadows (4→8 caster pipeline variants) (`3303be95`); **frame_globals bound into
  the masked-shadow pass** so a time-driven procedural cutout animates its SHADOW for free
  (`d384a072`).

Accurate remaining scope (was previously ambiguous): **Phase 5 (skin/morph MCP backend) and
Phase 6 (bones-in-outliner + skeleton/morph viz) are NOT built — surveyed only.** Plus:
animation playback in the editor/loader, Phase 4 packer/convert parity browser-verify, and the
vertex-selection-highlight cosmetic. Full prioritized scope + the time-saving gotchas +
the ready-to-paste overnight `/loop` prompt are in **`docs/plans/OVERNIGHT-HANDOFF.md`**.

### Overnight run, iteration 2 (Phase 5)
- `SetMorphWeight`/`MorphData` BROWSER-VERIFIED: MorphPrimitivesTest imports with its
  glTF default weights (0.5) intact; set_morph_weight 0→1.0 persists + visibly morphs
  (A/B screenshots). Two fixes en route: (a) morph-bearing imports were baked to captured
  Mesh and silently LOST their morph buffers — they now ride the SkinnedMesh/populate
  path (`mesh_has_morphs` in asset_template + the node-kind decision); (b) new shared
  `renderer_meshes_for_node` resolver (model_meshes OR template-owned SkinnedMesh keys) —
  also fixes the pre-existing R::MorphWeight readback, which could never see SkinnedMesh
  nodes. KNOWN + DEFERRED to the animation-playback item: a model whose glb ships a morph
  CLIP (AnimatedMorphCube) has its weights re-written every frame by the populate-baked
  renderer animation player, clobbering live pokes — the editor needs to own/neutralize
  template players (same root as "editor doesn't play imported clips").

### Overnight run, iteration 3 (Phase 5 skin + 2 findings)
- **SkinData query + get_skin_data MCP tool landed**: per skinned node →
  { source, primitive_index, joints:[{node,index,name,live,translation,rotation,scale}] }.
  Joints ARE editor nodes (mirror bones) — posing = SetTransform on the joint's node id,
  animating = a Transform track targeting it; this query is the discovery map. `live` flag =
  the skin bridge holds the mirror→baked mapping (Fox: 24/24 live). VERIFIED: query returns a
  real rig over /debug. Pose-deforms-skin NOT yet seen (blocked by the finding below).
- **FINDING (blocker, NEXT UP): edge_resolve/final_blend pipeline never reinstalled after
  import.** Importing Fox (textured PBR) → register_material → clear_dynamic_pipelines()
  nulls final_blend_pipeline_key → relaunch pushes "7 layout-level edge sub-pipelines" but
  final_blend is never installed → render-frame preamble warn-skips EVERY frame
  ("pipeline not compiled at material_opaque::edge_resolve (id=final_blend)", suppressed
  after first log) → CANVAS FREEZES at the last presented frame while frame_count keeps
  advancing AND wait_render_settled returns settled:true (the scheduler drained because
  final_blend was never queued — settle is lying). Likely the known "variant edge pipeline
  never installed" MSAA bug (msaa-unify memory; Fix A may not be on this branch). Leads:
  pipeline_scheduler/launch.rs:1110 (install site), launch_edge_resolve_compile (launch.rs:762),
  edge_pipeline.rs clear_dynamic_pipelines + render_pass.rs:128 guard.
- **GOTCHA (added to handoff): frozen-canvas mode.** Symptom: frame_count advances, queries
  answer, settled:true, but canvas_stats/ScenePng never change (luma frozen). The earlier
  fox pose screenshots were INVALID because of this. Sanity-check renders with an
  insert-box + canvas_stats delta before trusting any A/B. Force-recover by touching an
  editor file (trunk rebuild → page reload) — but the freeze RECURS on the next skinned
  import until the final_blend bug is fixed.

### Overnight run, iteration 4 (frozen-canvas instrumentation + skin-pose detective work)
- Edge-launch instrumentation LANDED (launch.rs): INFO breadcrumbs for in-flight skips,
  "0 pushed (N cache-hit installs, M in-flight skips)", and apply-path "no longer desired —
  dropped". With these in, the original final_blend freeze did NOT reproduce (fresh-session
  imports + 2nd/3rd imports all healthy; relaunch shows clean cache-hit reinstalls). The
  freeze remains REAL but stateful/intermittent — breadcrumbs will name the eaten branch
  when it recurs. Keep the insert-box+luma sanity check before trusting A/Bs.
- **Skin pose still does NOT deform** (fox neck/root pokes → byte-identical renders, canvas
  PROVEN live), even after delete_clip of all 3 fox clips. Chain verified so far: 24/24
  joints registered; SetTransform commits to the mirror's renderer local (node_transforms
  shows it); sync_bones_to_skin IS in the render loop (render_loop.rs:222, before
  update_transforms). REMAINING SUSPECTS: (a) animation_sync::pin_pose runs every frame
  BEFORE the skin bridge and may re-pin bone mirrors from LOWERED renderer players that
  delete_clip didn't unlower → clobbers manual pokes (same mechanism as the morph-cube
  clip clobber); (b) the transforms_eq guard/copy in sync_bones_to_skin. NEXT: read
  animation_sync::pin_pose + the lowering lifecycle; test pose with playhead transport
  fully neutralized; if (a), the fix likely also solves the morph-clip clobber + is the
  groundwork for core item (3) animation playback.

### Overnight run, iteration 5 (pose-clobber root-caused to a systemic stateful degradation)
- skin_bridge breadcrumb LANDED ("copied N changed bone local(s) → baked joints").
  Evidence chain on a live session: with clips present, pin_pose rewrites ~20 bone
  mirrors EVERY frame (per-frame "copied 20" — manual pokes are clobbered by design
  while a clip owns the pose, like any DCC). After delete_clip: fight stops, a neck
  poke logs "copied 1" (mirror→baked write CONFIRMED) — yet pixels don't move.
- **CAPTURE-PATH SPLIT (critical for verification discipline): canvas_stats froze
  (stuck at a stale luma across new_project + imports) while ScenePng kept tracking
  scenes correctly. canvas_stats is NOT a liveness oracle — use ScenePng byte-diffs
  across DIFFERENT scenes/cameras instead (identical-bytes on same-scene+camera is
  legitimate determinism, not freeze).**
- Unified hypothesis: the session enters a degraded state (origin = the earlier
  final_blend/preamble freeze): canvas-element presentation breaks (user-visible
  freeze + frozen canvas_stats), the GPU coverage pass output goes stale/zero, and
  the skinning-LOD coverage gate then "freezes submeshes in their last-skinned pose"
  (meshes.rs update_world skip_skins grace logic) — which is EXACTLY why a confirmed
  baked-joint write doesn't deform. ScenePng still updates because render() keeps
  running for non-skin content.
- NEXT (fresh window): force trunk reload FIRST, verify fox pose on a CLEAN page
  (delete clips → poke → expect "copied 1" + ScenePng diff). If deform works clean,
  the skin path is DONE-verified and the single remaining bug is the stateful
  degradation — chase it with the edge breadcrumbs + a coverage-pass staleness probe.

### Overnight run, iteration 6 — SKINNED IMPORTS FIXED + POSE-DEFORM BROWSER-VERIFIED 🦊
- **ROOT CAUSE of everything skin-related tonight: editor skinned imports NEVER rendered
  correctly** — they arrived as collapsed shards (verified: every "framed fox" screenshot
  was an empty grid or fragments; NodeBounds returns a unit-cube fallback for SkinnedMesh
  so frame_node aimed at nothing — separate small bug, still open). Mechanism:
  skins.insert seeded matrices with bare IBM (no world×), correct only if a later pass
  refreshes every joint — but an ASYNC mid-session import lands after the frame consumed
  its joints' dirty flags, so un-animated joints kept IBM-only matrices forever →
  vertices collapsed (only clip-touched bones rendered: the "strips"). The player never
  hit it (cold-boot derives all worlds before first render).
- **FIX (renderer): `pending_full_refresh` one-shot full joint-matrix seed** — skins
  record their key at insert; the next update_transforms seeds EVERY joint from current
  worlds (bypassing dirty set + skip gate), then never again. VERIFIED: fresh fox import
  arrives FULLY INTACT (first time in the editor), and a neck-bone SetTransform visibly
  bows the head (A/B screenshots fox13_arrival/fox14_neckbend).
- Also landed: skins.update_transforms diagnostic ("N joint matrices updated, M skins
  skipped") which proved the dirty-flow worked and localized the seed bug.
- Phase 5 state: morph editing VERIFIED (iter 2), rig discovery + posing VERIFIED (now).
  Remaining Phase 5: richer animation authoring polish; NodeBounds-for-SkinnedMesh fix;
  pin_pose-vs-manual-pose semantics note (pose while clip active is owned by the clip —
  by design, document in tool descriptions).

### Overnight run, iteration 7 (bounds fix + bone icons; capture mystery)
- **NodeBounds fixed for SkinnedMesh** (QUERY-VERIFIED: fox reports real ±90-unit world
  extents instead of the unit cube): node_bounds now prefers the renderer's LIVE
  world AABB (union over the node's materialized meshes via renderer_meshes_for_node,
  resolved BEFORE the renderer lock to avoid bridge-lock nesting), falling back to the
  scene-side local_aabb only when nothing is materialized. frame_node on rigs now has
  real bounds to aim at (visual confirm pending — see capture issue).
- **Phase 6 first slice: bone icons in the outliner.** New "bone" glyph in the shared
  icon set; outliner rows show it for Group nodes registered as skin joints (bridge
  skin_joint_baked lookup — zero NodeKind/protocol change). NEEDS VISUAL CONFIRM in the
  morning (outliner is DOM; ScenePng only captures the viewport).
- **OPEN: ScenePng = "no image available"** (after ~2min; the earlier empty replies were
  my own curl timeouts aborting the write → STOP_SENDING warns). State: fresh page,
  edge pipelines all cache-hit-installed, NO preamble warn-skips, queries fine — so
  presentation looks healthy but poll_scene_capture can't grab an image. Viewport size
  changed to 2032×1094 around the same time. NEXT: read the scene-capture impl
  (editor engine/query.rs poll_scene_capture) — suspects: copy alignment at this size,
  capture queue wedged by the aborted writes, or capture-canvas re-init after reloads.
  drive.py curl timeout bumped to 150s.

### Overnight run, iteration 8 — capture mystery SOLVED: the display went to sleep
- scene_png now surfaces the real capture error via console_logs (was swallowed into
  "no image available"). Reproduced: "scene capture timed out (no frame presented)" —
  and frame_globals shows frame_count FROZEN at 1 with dt 0.0: **the render loop (RAF)
  is paused because the Mac's display slept/locked (~00:30). Chrome pauses RAF for
  occluded windows; Chrome GPU process at 0.4% CPU.** This also retro-explains the
  night's intermittent "degraded sessions" (canvas freezes that recovered after
  reloads ≈ display dozing between polls). caffeinate -u fired + `caffeinate -d -t
  28800` armed (display won't RE-sleep once unlocked), but a LOCKED session keeps
  Chrome occluded — no remote fix, correctly so.
- CONSEQUENCE: visual verification is BLOCKED until the user unlocks in the morning.
  Pivot: build + NUMERICALLY verify animation playback (SampleClipTimeseries is
  GPU-independent by design), MCP robustness (query-verifiable), and queue all visual
  confirms (bone icons, skeleton viz when built, fox-framed screenshot) for morning.

### Overnight run, iteration 8b — animation playback verified NUMERICALLY (partial) + a lowering suspect
- With the display locked (no RAF/no pixels), used the GPU-independent
  sample_clip_timeseries to verify item (3)'s data chain: Fox "Walk" (0.708s) sampled at
  pinned times via NodeLocalTrs readbacks shows TIME-VARYING bone rotations
  (b_RightFoot01: max quaternion delta 0.0218 across t=0/0.25/0.5) — editor clip →
  lowering → renderer sampling → bone TRS all function headlessly. Scrub posing
  (pin_pose) was already proven (the copied-20-per-frame fight in iter 5).
- **SUSPECT for next window: left/right asymmetry** — b_LeftFoot01/b_LeftLeg01 read
  dead-zero deltas at the same times the right side swings. Either the Walk clip is
  genuinely asymmetric at those sample phases (check times against the 0.708s loop) or
  the lowering DROPS some channels (per-bone TrackTarget resolution gap). Compare
  get_track_data for a left-leg track vs right, and sample finer times.
- MORNING VISUAL QUEUE: bone icons in outliner (DOM); fox framed via fixed NodeBounds;
  fox Walk playing in viewport (set_playing); skeleton viz (not yet built); cutout/
  shadow scenes from earlier iterations are already verified.

### Overnight run, iteration 9 — animation-channel materialization race FIXED (numerically verified)
- Root-caused the left-leg asymmetry: import registers clips (anim_revision → debounced
  relower at ~200ms) while bone mirrors are still materializing ASYNC; channels whose
  target node lost that race were skipped as "pending" and NOTHING re-fired when the
  node appeared → silently un-animated bones (Fox: left legs static, right legs won the
  race — nondeterministic per run). Probes en route: all 21 Walk tracks carry real
  motion + target real bones (two of my own probe bugs corrected: alphabetized serde
  fields truncated past, and TrackTarget serializes flat).
- FIX: node_sync nudges the (pub(crate)) debounced schedule_relower whenever a node
  materializes — a rig's burst coalesces into one rebuild; no-op without clips.
  NUMERICALLY VERIFIED post-fix: b_LeftLeg01 Δ0.018 == b_RightLeg01 Δ0.018, LeftFoot01
  Δ0.1137 (was 0.0), Neck Δ0.0227 across pinned Walk times. Item (3)'s lowering is now
  complete; viewport playback visual goes to the MORNING VISUAL QUEUE (display locked).

### Overnight run, iteration 10 — Phase 6 skeleton bone-line overlay BUILT (morning visual)
- New engine/skeleton_viz.rs: per-frame fat-line overlay of every registered skin's
  bone hierarchy (parent→child segments from the MIRROR transform hierarchy — the
  thing posing/animation actually drive), warm orange, depth_test_always so the rig
  reads through the mesh, one LineKey rebuilt per frame (tens of segments). New
  Settings → "Skeleton overlay" toggle (default on), wired in the settings drawer +
  render loop beside light icons. Compiles + lint-green; CANNOT see it (display
  locked) → MORNING VISUAL QUEUE.
- Confirmed vertex-selection highlight (backlog item b) was ALREADY fully built
  (bridge/vertex_highlight.rs — cross markers per selected vertex, one LineKey,
  selection-observer driven). Morning visual only.

### Overnight run, iteration 11 — docs + Phase 4 headless data checks; HEADLESS BACKLOG DRY
- docs: AGENT_GUIDE gains "§8 Skins & morphs (rigs over MCP)" (discover/pose/animate/
  morph/see-the-rig recipes incl. the clip-owns-the-bones caveat + numeric verify via
  sample_clip_timeseries); MCP.md lists get_skin_data / get_morph_data / set_morph_weight.
  Both are served as MCP resources (agent-guide) so agents self-serve the recipes.
- Phase 4 headless data checks: export_glb of Fox+MorphStressTest → valid glb, 3 meshes /
  30 nodes / 3 animations; rigs flatten to static (skins:0, targets:0) which is the
  DOCUMENTED current behavior ("Skinned/morph glb re-export from source is a follow-on").
  export_player_bundle{name} → 6-file set with sane scene.toml (env/shadows/assets +
  source-glb refs). Pixel parity (load_player_bundle screenshot compare) → morning queue.
- HEADLESS BACKLOG NOW DRY. Switching to slim wakeups polling frame_globals; when
  frame_count advances (user unlocked), run the MORNING VISUAL QUEUE: ① bone icons in
  outliner (user eyeball or full-page shot), ② skeleton overlay on Fox, ③ frame_node on
  fox (fixed bounds), ④ fox Walk via set_playing (viewport), ⑤ vertex highlight markers,
  ⑥ set_morph_weight visual on MorphStressTest (named targets), ⑦ load_player_bundle
  round-trip screenshot compare, ⑧ hostile-QA stress scenes + console_logs, ⑨ visual-
  quality A/Bs, ⑩ delta_time perf eyeball on stress scenes. Then the final report.

### Overnight run, iteration 12 (morning, user unlocked) — frame_node FIXED+SEEN; LIMIT PAUSE
- FrameNode now uses the live world-AABB (same policy as the NodeBounds query fix —
  the command had its own bounds path). **SEEN VERIFIED: frame_node centers a fresh
  Fox import** (am3 screenshot; tight framing, fox fills view). Faint whitish lines
  visible at the chest = likely the skeleton overlay but WASHED OUT — check
  BONE_COLOR/width/depth handling when resuming (maybe HDR-bright it like the
  light icons, or the lines pass tonemaps it down).
- ▶▶ RESUME QUEUE (run on restart, tab open + unlocked): ① skeleton overlay clearly
  visible on Fox (fix color if washed out) ② fox Walk PLAYING in viewport
  (set_current_clip + set_playing, two shots apart) ③ set_morph_weight visual A/B on
  MorphStressTest (named targets) ④ vertex highlight markers (select_vertices_where +
  SetVertexSelection on an editable mesh) ⑤ load_player_bundle round-trip screenshot
  compare ⑥ bone icons in outliner (user eyeball — DOM) ⑦ hostile-QA stress scenes +
  console_logs + FIX ⑧ visual-quality A/Bs ⑨ delta_time perf eyeball ⑩ FINAL REPORT.
- Session inventory (27 commits on mesh-authoring tonight, cf352b30..HEAD): shadows
  (soft/PCSS/double-sided/animated-cutout), editor batch #14-18, morph editing
  end-to-end, skin rig discovery+posing, skinned-import seed fix (fox renders!),
  channel-race fix (L/R legs verified), NodeBounds+FrameNode live-AABB, bone icons,
  skeleton overlay (built), morph names, docs/recipes, edge breadcrumbs, capture
  error surfacing. All gated (lint+tests); nothing pushed.
