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

### Morning queue results (iteration 13, fresh limits)
- ① SKELETON OVERLAY SEEN: full fox rig (spine/leg/tail/neck chains) as saturated
  orange bone-lines through the mesh (HDR colors clamp per-channel in the line
  target → use saturated LDR; b5b161e0). ③ WALK PLAYBACK SEEN: set_current_clip +
  set_playing{on} in animation mode → fox walks in viewport, two distinct gait
  frames, skeleton overlay TRACKS the animation. (Transport only ticks in animation
  mode; SetPlaying field is `on` — agent-guide-worthy.) ④ MORPH A/B SEEN:
  MorphStressTest "Key 3" by NAME → blue bar at full height (after delete_clip —
  clips own weights, as documented). ⑤ VERTEX HIGHLIGHT SEEN: amber cross markers
  on a sphere (predicate shape: {"kind":"top_percent","axis":N,"percent":P};
  top_percent selected 792/825 — semantics worth a doc note). ⑥ ROUND-TRIP PIXELS
  SEEN: authored vs load_player_bundle reload visually identical (sphere+box+PBR
  colors). ⑦ bone icons in outliner = DOM, needs USER EYEBALL.

### Stress battery results (iteration 13b) — ALL PASS, one bug found+fixed
- S1: 30-mesh scene under a 12-deep Empty chain + 8 shadow-casting point/spot
  lights → renders, zero errors, 17.1ms frame (~58fps). FOUND+FIXED: transient
  vertex-selection markers SURVIVED new_project (ghost dome) — NewProject now
  clears vertex_selection (observer tears markers down).
- S2: 4-modifier stack (twist/taper/subdivide/inflate) + 48-op undo/redo storm →
  mesh_stats byte-identical pre/post (undo log sound), mesh renders, zero errors.
- S3: 18-step reparent storm (3 groups + root, cycles) → node + transforms intact;
  orthographic projection toggle renders (SEEN); zero errors; 14.8ms frame.
- Perf eyeball: 14-17ms frames on the stress scenes with 8 shadow lights on a
  2032×1094 canvas — no regression flags. (Quantitative baselines remain a
  follow-on; nothing tonight touched render hot paths except logging.)

### Tier-1 verification closed (user-in-the-loop, morning after)
- ✅ Bone icons in outliner: USER-VERIFIED (joint rows show the bone glyph).
- ✅ Skeleton overlay: USER-VERIFIED "looks good as-is".
- ✅ top_percent "selected 96%": NOT a bug — `percent` is 0..1 (I sent 15.0 → clamped
  to 1.0). ROBUSTNESS TODO: select_vertices_where tool doc must state 0..1 and
  out-of-range input should warn instead of silently clamping.
- ✅ Pose-vs-clip semantics: USER SIGNED OFF on DCC behavior (clip owns bones/morphs
  while active; pause/delete to hand-pose). Already documented in AGENT_GUIDE §8.

### ▶▶ NEXT LOOP SCOPE (user-agreed 2026-06-12 morning)
A. Robustness quickies: select_vertices_where percent documented 0..1 in tool desc +
   WARN on out-of-range input; demote overnight diagnostic info! logs (edge breadcrumbs,
   skin-bridge/skins-update counts) to debug!; frame_node rig padding slightly generous;
   skeleton overlay: DIM the root-tether segments (user-agreed) — a bone segment whose
   PARENT joint sits at/near the rig's scene-origin root (e.g. _rootJoint/b_Root_00 at
   world origin) renders at reduced alpha (~0.35) so the anatomical skeleton pops and
   the floor-to-pelvis root-motion tether recedes (it stays visible — it's information).
   Verify in-tab on the fox: pelvis-to-floor line dimmer than limb/spine bones.
B. HUMAN KEYING (user priority — the human animation gap): the transport add-key
   button samples the TRACK's own curve (sample_at(t)) instead of the live scene, so
   "pose a bone with the gizmo → press key" captures nothing. Build: (1) key-from-pose —
   add-key reads the LIVE node value (bone transform / morph weight) for the selected
   track; (2) AUTO-KEY toggle — while on (animation mode), a gizmo edit on a tracked
   node writes a keyframe at the playhead. Verify in-tab: pose → key → scrub away/back
   shows the captured pose; auto-key drag creates diamonds in the dope sheet.
C. Rig-preserving scene-glb export: export_glb flattens skins/morphs — re-export skins
   (joints/IBMs/JOINTS_0/WEIGHTS_0) + morph targets (+ targetNames). Verify headless
   (parse: skins>0, per-prim targets>0, names) + reimport round-trip in-tab (fox
   arrives intact, posable, Walk playable).
D. TWO-BONE IK for posing: new command/MCP tool (e.g. solve_ik {chain_end_node,
   target_pos, pole?}) that solves a 2-bone chain (hip→knee→foot etc. discovered via
   skin_data hierarchy) and writes the joint rotations — agent- AND human-reachable
   (stretch: a small gizmo affordance). Verify: fox foot pinned to a target while the
   chain bends naturally (screenshots).
E. PER-VERTEX JOINT-WEIGHT EDITING: read+write skin JOINTS_0/WEIGHTS_0 on live skinned
   meshes (renderer storage-buffer surgery): get_skin_weights {node, indices} +
   set_skin_weights {node, entries:[{vertex, joints[4], weights[4]}]} (+normalize).
   Verify: reweight a fox region to a different bone, pose that bone, watch the region
   follow (A/B screenshots).
F. (Stretch, byte-guarded) Phase 2b: route renderer-gltf vertex builders through the
   shared mesh_pack packer — byte-identity proptest committed BEFORE the switch; abort
   + log if identity can't be proven.
G. DEFERRED BY DECISION: retargeting — agent-driven over MCP once D+E exist (the
   decision layer is agentic: rig analysis via get_skin_data/get_track_data; the
   per-keyframe math eventually wants a small deterministic retarget_clip kernel).

### Day-2 loop, scope A — DONE (all four quickies)
- percent documented as 0..1 fraction in the select_vertices_where tool desc + WARN on
  out-of-range input. Overnight diagnostics (skin-bridge copies, skins-update counts,
  3 edge breadcrumbs) demoted info!→debug!. frame_node live-AABB fit ×1.15 breathing
  room (note: elongated rigs faced end-on still compose head-on — camera-DIRECTION
  choice, not padding; cosmetic, deferred). Skeleton root-tether segments (chain depth
  ≤1) dimmed to 0.35 alpha — SEEN VERIFIED on the fox side view: anatomy pops, tether
  reads as a faint drop to the floor origin.

### Day-2 loop, scope C — RIG-PRESERVING SCENE-GLB EXPORT done (parsed + SEEN)
- Both export paths (export_glb single-node + export_scene_glb whole-scene/MCP) now
  EMBED cached clean-rig glbs: collect_rig_scenes + append_rigs (IR-level merge with
  DFS node-offset on skin joints + skin-index bump; appending never shifts existing
  indices so lowered clip channels stay valid). Editor SkinnedMesh nodes skip their
  bind-pose static when their rig embeds. Import-side: morph-only files (no skins)
  now ALSO cache a rig glb (gate widened skins→skins||morphs). glb-export crate:
  extract carries mesh.extras.targetNames (reader extras on), writer emits them
  (gltf-json "extras" feature added).
- PARSED: export of fox+MorphStressTest → SKINS:1 (24 joints + IBM), per-prim
  targets [0,8], targetNames ["Key 1".."Key 8"]. SEEN: reimporting the export → fox
  fully intact with skeleton overlay live, 24/24 joints live, 8 named morphs.
- v1 limitations (logged in code): embedded rigs export at source placement (mirror-
  hierarchy edits don't retarget into the rig glb); rig materials are source defaults
  (reimport renders untextured; the bundle path re-applies materials from scene.toml).
- TOOLING NOTE: trunk's file-watcher silently stopped rebuilding TWICE this scope —
  symptom: wasm mtime frozen despite touches; fix: full stack restart. Watch for it.

### Day-2 loop, scope D — TWO-BONE IK landed (SEEN)
- EditorQuery::SolveIk { end_node, target, pole? }: analytic two-bone solve (law of
  cosines + sequential rotation-arc deltas) over the renderer mirror hierarchy
  (end→parent=mid→grandparent=root), returning LOCAL rotations + reach (clamped to
  the chain span). Application = two SetTransforms in one DispatchBatch (one undo
  step, auto-key compatible). MCP solve_ik tool wraps solve→fetch-locals→batch-apply
  with apply:false for solve-only.
- SEEN VERIFIED on the fox left hind leg: no-pole solve lifts/tucks the leg to the
  target (reach 1.0); WITH a pole in front of the knee the leg takes a natural
  bent-knee step pose. Undo of the batch restores the stance. NOTE for agents (in
  the tool desc): give a pole for natural knee/elbow direction.

### Day-2 interactive session (after the closing report) — user-verified keying + 4 fixes
- User retried human keying with their own hands; found the scope-B commit had landed
  ONLY live_track_value (neither consumer wired — commit message overclaimed; data-path
  "verification" couldn't catch it). Landed + USER-VERIFIED ("worked perfectly"):
  b339cbfe (pin_pose gated on playing||playhead-moved → paused hand-poses HOLD, DCC
  semantics; tool palette mounted in Animation viewport), 7db5dc18 (transport ⬩ wired
  to live_track_value; auto-key hooked into gizmo commit_drag — gesture-level only,
  programmatic SetTransform/MCP/IK/undo never auto-keys; dope-sheet geometry now
  listens to clip duration signal; skeleton overlay update-in-place instead of
  remove+add per frame — ~300k short-lived GPU objects/idle-hour churn was the prime
  suspect for the user's Aw-Snap tab crash after 1.5h idle; mesh_pack synthetic-tangent
  test read offset 32→36 test-bug fix), beb4c72f (auto-key toggle moved Settings →
  transport bar, record-style).
- Environment: user's disk hit 100% full mid-session (cargo incremental ballooned;
  freed ~90GB by deleting target/debug/incremental; target/ still ~149G — deep-clean
  candidate). Disk-full may have contributed to the crash.

### ▶▶ NEXT LOOP SCOPE (user-agreed 2026-06-12 afternoon) — day-3 loop
User decisions: IK default-pole heuristic IN; rig-export material preservation IN
(user chose "fix in this loop" over v1-accept); all four proposed scopes IN; plus an
open-ended mandate: "anything else you can grok from the docs and/or code that will
make the library code more performant and the editor+mcp more featureful and usable,
especially over MCP for agents".
- A. PERF/STABILITY SWEEP (first — crash-motivated): audit ALL per-frame paths for
  per-frame alloc/create-destroy churn (gizmo, light_icons, curve_handles, particles,
  node_sync effects, anything calling add_*/remove_* or creating GPU objects in the
  render loop); fix in-place-update patterns like the skeleton one; then a soak check
  (leave renderer running, sample memory via performance.memory over minutes, assert
  flat-ish slope). Also: deep-clean guidance for the 149G target/ (document, don't
  surprise-delete).
- B. RIG-EXPORT MATERIAL PRESERVATION: exported scene-glbs' embedded rigs should carry
  materials+textures (reimport renders textured). Reuse the existing glb-export
  material path; the rig merge currently drops to source defaults.
- C. IK DEFAULT-POLE HEURISTIC: when no pole is passed, bias the bend toward the
  chain's CURRENT bend plane (preserve existing knee/elbow direction); fall back to
  character-forward only for a perfectly straight chain.
- D. VERTEX HIGHLIGHT ON SKINNED MESHES: extend vertex_highlight to skinned/posed
  meshes (positions must come from the posed/skinned result, not the rest mesh).
- E. PHASE 4 PARITY VERIFY: byte-identity proof between renderer mesh_pack and the
  convert crate's packer (the deferred proptest); if identity holds, consider the 2b
  unification go/no-go with evidence.
- F. FLIPBOOK MATERIAL (deferred from cutout phase).
- G. OPEN-ENDED: survey docs/ + code for high-leverage perf or agent-over-MCP
  usability wins; pitch-then-do the top items within budget (log triage in this doc).

### ▶▶ DAY-3 STANDING RULE CHANGE (user-directed 2026-06-12): DONE-MEANS-DONE
User: the "land the safe subset, defer the rest" pattern has caused major prod
bites before ("I thought something was finished and it slipped through as
deferred"). New contract, superseding the safe-subset rule for this and all
future loops on this project:
- A scope exits ONLY in one of two states: (1) FULLY COMPLETE and verified at
  the appropriate level (SEEN/parsed/tests), or (2) blocked on something only
  a human can physically do (real click/drag, eyes-on-taste, hardware) — which
  goes on the user-judgment queue with EXACT repro steps. There is no third
  state. "Deferred"/"remaining"/"partial" are not outcomes.
- Time is explicitly NOT a constraint: hours or days of looping are preferred
  over a fast pass with gaps. The loop keeps iterating (ScheduleWakeup) until
  every scope is in state (1) or (2); it does not end after one sweep.
- Context for the bar: this renderer is meant to power AAA-quality games on
  the web. Performance and features are critical; code quality and docs are
  EQUALLY critical because the codebase is complex. The editor must be able to
  express all kinds of changes, increasingly via agentic creation — MCP tools
  that only non-humans can drive are a first-class feature surface.

### Day-3 loop — first burst (scopes A-instrument/B/C/D code landed)
- A: per-frame churn AUDIT done across render_loop callees — light_icons (sig-gated
  rebuild + in-place reanchor ✓), curve_handles (zoom-only ✓), gizmo (set_hidden/zoom ✓),
  particles (preallocated bufs; per-frame Vec allocs are CPU-only, scoped to live
  emitters), skin_bridge (1 small Vec/frame ✓), vertex_highlight (selection-driven ✓),
  render() preamble (polls/no-ops ✓). NOTABLE: material_opaque edge-resolve creates 3
  bind groups/frame BY DESIGN (documented tradeoff; bind groups pin no buffer memory —
  soak arbitrates). MemoryStats instrumentation landed (93e6a805): query + MCP tool
  get_memory_stats (JS heap + meshes/transforms/materials/lines/pipeline counts).
  BASELINE soak (empty scene, ~13min): heap sawtooths 7–14MB (healthy GC), all counts
  rock-flat (/tmp/soak-baseline-empty.csv). Fox-playing soak rescheduled to AFTER all
  code lands (own rebuilds wiped the staged scene — measurement windows must contain
  zero trunk rebuilds).
- B (d4ffbb8c): rig re-exports carry materials+textures per-primitive (ExtraPrimitive
  IR; per-primitive write; ImagePool original-bytes copy; append_rigs image offsets).
  Round-trip test contract updated (material PRESERVED). Browser verify queued.
- C (551a0c4c): IK bend-plane normal extracted pure + 4 unit tests; straight-chain
  fallback now character-forward (old Y-fallback degenerated to sideways-X on downward
  reaches — the day-2 "lift/tuck"). Browser verify queued.
- D (2905d161): vertex highlight CPU-skins selected vertices with the GPU palette
  (new Skins::read_joint_matrices); posed-surface markers, world-space, set-0.
  Staleness rule documented. Browser verify queued.
- Env: stack restart needed run_in_background (nohup alone dies with the sandbox shell).

### Day-3 loop — verification batch (all browser SEEN evidence collected)
- E (89b8551f) BROWSER-CONFIRMED: fox imports through the unified mesh_pack gltf
  path and renders correctly (geometry/texture/skeleton intact — a byte error
  would corrupt every mesh).
- C (551a0c4c) SEEN: no-pole solve_ik on the fox hind leg → natural forward step
  (knee in its anatomical plane, no sideways tuck), foot at target err=0.0.
  Tooling lesson recorded: node_transforms returns the WHOLE subtree keyed by
  node id — read entries[<requested-id>], not values()[0] (an early misread
  cost a debugging round).
- D (2905d161) SEEN A/B: amber vertex markers on the fox muzzle at rest, then
  following the muzzle DOWN after a head pose + selection re-emit — markers
  track the deformed surface via the GPU palette.
- B (d4ffbb8c) PARSED+SEEN: export_glb carries materials:1 images:2 textures:2
  skins:1 (fox_material with baseColorTexture); reimported export renders FULLY
  TEXTURED (orange/white/dark-leg fox). The "reimports render untextured"
  limitation is closed. Note: image pool carries a scene-level + rig-level copy
  of the fox texture (2 images for 1 source) — dedup is a known nice-to-have.
- F (1fc15bd8 + b5ac5236) SEEN: agent-authored Mask FlipBook (atlas import →
  add_builtin_material flipbook → update_builtin_material Mask+atlas → assign
  to quad) renders as an ANIMATED CUTOUT — pinned-clock screenshots show
  circle (t=0.5) → square (t=1.5) → triangle (t=2.5), pure hole-out, no quad
  silhouette. SHADOW: top-down captures show the hole-shaped shadow MORPH
  (soft ellipse → soft rectangle) between cells while a control box's shadow
  stays constant — the masked shadow pass runs the same shared cell math.
  set_frame_time/clear_frame_time = the deterministic capture instrument.
- USER-CAUGHT mid-batch regression (b5ac5236): the cell-math extraction dropped
  flipbook_get_material → WGSL validation killed editor BOOT (flipbook edge/
  masked variants newly compile at startup). Fixed + permanent per-base
  module-completeness test (renders edge_resolve for all 4 bases, asserts every
  *_get_material call is defined). Agent-observability gap closed alongside:
  editor POSTs boot failures to the relay's new /boot-error; GET /health
  returns { editor_attached, last_boot_error } — first check when /debug goes
  quiet.
- Shadow-debug gotchas recorded: per-light shadow max_distance=100 means
  shadows only render within ~100 units of the CAMERA (orbit close!); TWO
  shadowed directional lights overflow the EVSM atlas (warn-spam + shadows
  degrade) — delete, don't just dim, the spare; consecutive set_transforms on
  one node COALESCE into one undo entry.
- UpdateBuiltinMaterial (5a58992a) PROVEN over MCP-path dispatch: the whole
  flipbook authoring chain above ran through commands an agent can issue.
- A FINAL SOAK (closes scope A): 30 min, rich scene (Walk-playing fox + live Mask
  flipbook + shadows + skeleton overlay + textured-reimport fox). 120 samples via
  get_memory_stats: ALL renderer object counts FLAT the entire run (meshes 16,
  transforms 232, materials 13, lines 1, render pipelines 160, compute 81 — zero
  churn-class growth). JS heap: warm-up rise for ~15 min then PLATEAU — quartile
  floors 15.6/23.5/27.5/25.8 MB, Q3→Q4 mean 41.7→41.5 MB (flat). No leak at
  steady state. The remaining tab-crash question (multi-hour unattended) gets an
  overnight soak after the shadow pass. Baselines: /tmp/soak2.csv,
  /tmp/soak-baseline-empty.csv.
- Test assets RESOLVED (2026-06-13): both moved out of the main repo's `media/`
  into the sibling LFS assets repo `../awsm-renderer-assets/mesh-authoring-tests/`
  (Git-LFS, published via GitHub Pages — the established fixture convention; see
  `taskfiles/config.yml` PATH_MEDIA_ADDITIONAL_ASSETS, served on :9083 in dev).
  Load from:
    dev  http://localhost:9083/mesh-authoring-tests/flipbook-atlas-2x2.png
    prod https://dakom.github.io/awsm-renderer-assets/mesh-authoring-tests/...
  (the main repo `media/` server on :9082 carries only glTF-Sample-Assets).
  NOTE: the assets-repo commit is local — push that repo too for GitHub Pages.

### Day-3 scope H (user-extended): shadow correctness + lighting review
- 5c2b375f SEEN-verified: EVSM atlas auto-grow (2nd+ shadowed sun no longer
  degrades to PCF — one info log 2048→4096, zero warn spam, two shadow lobes);
  shadow max_distance <= 0 = AUTO/camera-far and is now the default (the fixed
  100.0 was a scale trap: cm-scale scenes lost all shadows beyond 100 units of
  the camera — the root of most of the verification-batch shadow confusion);
  per-frame warn spam latched to once-per-episode.
- Lighting application audited CLEAN: IBL/ambient never shadow-attenuated;
  per-light visibility multiplies the direct term only; receive_shadows gate
  honored; SSCS applied to directional only (point/spot use their own maps).
- OVERNIGHT unattended soak launched (8h, 60s cadence) — the multi-hour
  tab-crash question's definitive instrument; results analyzed on completion.

### Day-3 overnight soak — early termination + relay staleness fix
- The 8h unattended soak ran 46 CLEAN minutes (heap plateau ~50 MB used /
  96 MB total, every object count flat — fully consistent with the 30-min
  verdict), then the editor session died INSTANTLY between samples: no heap
  ramp, no count growth, no degradation curve. The healthy-then-cliff
  signature + the user being away points to macOS display sleep / App Nap
  freezing the tab (a known gotcha: display sleep pauses RAF), NOT a renderer
  leak. The multi-hour crash question stays open but the evidence now favors
  ENVIRONMENTAL causes over renderer growth; a definitive run needs sleep
  disabled (caffeinate) — queued for the user.
- REAL BUG found by the dead session: the relay kept the stale WebTransport
  session, so GET /health reported editor_attached:true while every /debug
  failed with "open_bi: session error". Fixed: a transport-level request
  failure now DETACHES the session, so /health tells the truth (the exact
  scenario /health was built for). Soak sampler stopped (it would have ERRed
  for 7 more hours against a dead tab).

### ▶▶ DAY-4 LOOP SCOPE (user-agreed 2026-06-13) — overnight correctness/perf/features
Standing rules unchanged: DONE-MEANS-DONE (exit only state-1 complete+verified or
state-2 human-blocked w/ exact repro; no "partial"); honesty bar (claim only what
was OBSERVED; UI/visual = browser SEEN, never inferred); gates before EVERY commit
(task lint + cargo test for EVERY touched crate, exit-status checked); grep ADDED
staged lines for the banned codename; keep the stack DOWN during native gates
(trunk+cargo artifact-lock fight = exit 143), restart as a TRACKED background task
for browser verification; GET :9086/health FIRST when /debug goes quiet (now
truthful — detaches dead sessions); imports via :9082 (glTF samples) / :9083
(awsm-renderer-assets fixtures incl. mesh-authoring-tests/); node_transforms
returns the WHOLE subtree — read entries[<id>]; set_frame_time/clear_frame_time for
deterministic time-varying captures; clips own bones (delete to hand-pose).

OVERNIGHT REALITY: the tab froze ~46min unattended last loop (display sleep / App
Nap, not a leak). So ORDER the work most-verifiable-first and do NOT let browser-
only features gate the loop's value: pillar 1 (correctness) + pillar 2 (perf) are
largely NATIVE-verifiable and come first; pillar 3 (features) comes last, attempts
browser SEEN when /health says attached, else lands the native-verifiable subset +
queues the visual confirm with exact repro. A `caffeinate -dis` is running to
stretch the window — use it, don't rely on it.

PILLAR 1 — CORRECTNESS (native-first; shadow/material/MCP):
- Shadow: audit the full lighting×shadow matrix for more bugs of the class already
  fixed (EVSM multi-sun, max_distance scale-trap) — spot lights + point/cube
  shadows under the new max_distance=auto; cascade-split correctness; bias defaults;
  receive_shadows gate on every pass; IBL never shadow-attenuated. Add native tests
  where the logic is pure (cascade fit, atlas packing, descriptor write).
- Material: extend the per-base shader-module-completeness test (the one that caught
  the flipbook_get_material boot break) to cover the OPAQUE compute + transparent +
  masked + shadow-masked templates for ALL bases — any `<x>_get_material`/helper
  called must be defined. Proptest material uniform-buffer packing round-trips.
- MCP: dispatch/echo/undo round-trip tests for the command surface; truthful error
  messages (no silent clamps); schema coverage. Audit every tool description for
  accuracy against behavior.

PILLAR 2 — PERF (per-frame + time-to-first-render):
- Per-frame: finish the churn audit (any remaining add_*/remove_* or GPU-object
  creation in the render loop → in-place update); add a lightweight perf-regression
  guard (e.g. assert no per-frame allocation on the steady-state paths via counters).
- TTFR: the "starts black until resize" latch (render_loop.rs DID_REAL_SIZE_SYNC) is
  a WORKAROUND — find the root cause (surface-config/resize-observer race) and fix it
  so first paint is correct without the backstop. Audit pipeline PREWARM coverage:
  which variants compile lazily on first use (first-frame hitches) vs prewarmed at
  boot — widen prewarm to the common set; parallelize createRenderPipelineAsync where
  serialized. Native-measure what's countable (prewarmed variant count, compiles per
  boot); browser-measure wall-clock TTFR when attached.

PILLAR 3 — FEATURES (vetted open backlog; do in value order, finish each to the bar):
- Tangent-generator consolidation: 3 impls still exist (renderer raw_mesh, glb-export,
  renderer-gltf) despite the awsm-tangents crate — route all through it, byte-identity
  proptest. (Also perf/quality.) NATIVE.
- Camera animation lowering (scene/animation.rs "lowering deferred") — NATIVE + browser.
- Custom-material alpha-only WGSL round-trip through the player (scene-loader/dynamic.rs
  TODO) — NATIVE + browser.
- Multi-UV (TEXCOORD_1+) + COLOR_n attribute wiring (renderer-gltf buffers) — NATIVE + browser.
- NLA/mixer MCP tool coverage + agent-guide docs (commands exist; MCP surface thin) — NATIVE.
- Phase-7 sweep: MCP tool/resource fidelity audit + module/doc-comment coverage on the
  tricky seams (mesh_pack, convert, skinning, animation lowering, shadows). NATIVE.
EXPLICITLY DEFER (too deep / low value / needs the user): Draco import, exotic KTX
formats, animation RETARGETING, the intermittent frozen-canvas bug (stateful,
browser-only, won't repro reliably unattended) — log, don't chase.

CLOSING: when every elected item is state-1 or state-2, write the closing report
(per item: landed commit + verification evidence, or queued w/ repro) + a soak
verdict if one ran, update memory, post a morning status via SendUserMessage, and
END (no further ScheduleWakeup). Do NOT push (the user pushes / PRs); the
mesh-authoring PR #119 is already open — note new commits land on that branch.

### Day-4 loop — pillar 1 (correctness), item: shader-module completeness guard
- Generalized the day-3 edge-resolve completeness test into a centralized native
  guard (`renderer/src/shader_completeness.rs`) covering ALL material-bearing
  templates that call `<base>_get_material` — opaque-compute, edge-resolve, AND
  transparent — for every first-party base (PBR/Unlit/Toon/Flipbook). Each
  rendered WGSL is scanned: every `<x>_get_material(` call must have a matching
  `fn` def in the same module (the exact class that took the editor down at boot
  on the day-3 flipbook cell-math extraction). Inline edge test removed →
  pointer. Added a scanner SELF-TEST (catch_unwind) proving the guard panics on a
  missing def + accepts a present one (a guard that can't fail is inert). 4 tests,
  213 renderer tests green, lint clean. NATIVE — no browser needed. State 1.

### Day-4 loop — pillar 1, item #20: shadow×lighting correctness audit
- AUDIT RESULT: no new bug found; day-3 fixes hold + verified deeper.
  · max_distance=auto (day-3) verified SAFE against infinite-far cameras:
    extract_near_far reverses the perspective m22/m23; an infinite-far proj
    gives m22=-1 → (m22+1)≈0 hits the guard → finite (0.1,100) fallback, so ∞
    never reaches pssm_splits. Large-world coarseness under auto is the
    documented quality tradeoff (set explicit max_distance), not a bug.
  · spot + point/cube shadows derive reach from influence_radius(intensity,
    range) — NOT max_distance — so the scale-trap was directional-only; they
    already carry their own range-collapse fix.
  · lighting application re-confirmed by reading apply_lighting.wgsl: IBL/
    ambient added BEFORE the punctual loop (never shadow-attenuated); shadow
    visibility multiplies the DIRECT term only; receive_shadows gates the
    sample; SSCS directional-only.
- LOCKED: 8 native tests on the pure cascade math (cascade.rs) — pssm_splits
  count-clamp / monotonic / last==far / within-range / lambda0=uniform /
  lambda1=logarithmic / near0-finite; cascade_resolution min-floor; a
  fit_cascades structural smoke test (count, split ordering, finite matrices).
  221 renderer tests green, lint clean. NATIVE. State 1.

### Day-4 loop — pillar 1, item #21: material packing + MCP-wire correctness
- Material uniform packing LOCKED (materials/uniform_packing_tests.rs, NullTextureContext
  stub): shader_id-is-word-0 invariant across PBR/Unlit/Toon/FlipBook (wrong word 0
  → wrong kernel branch); full FlipBook word layout pinned to wgsl/flipbook_material.wgsl
  (shader_id, alpha_mode, alpha_cutoff, atlas(5 SkipTexture words), tint(4), cols, rows,
  frame_count, fps, time_offset, mode, flip_y = 76 bytes) — a silent CPU↔WGSL drift here
  corrupts shading with no error; + FlipBook clamp behavior (cols/rows floor 1,
  frame_count clamped to cols*rows).
- MCP wire round-trip (editor-protocol/transport.rs): serde ser→de→ser idempotence over
  a representative Request slice (9 commands + 5 queries + envelope variants) incl. an
  explicit `cmd` tag-drift assertion — guards the agent surface against serde rename
  drift (silent wire break, no compile error). Truthful-error audit folded into #29.
- 6 new tests, materials + editor-protocol suites green, lint clean. NATIVE. State 1.

### Day-4 loop — pillar 2, item #22: per-frame churn audit + overlay scratch reuse
- AUDIT: renderer hot path already hardened day-3 (instance writes zero-alloc
  095b0e9a; skeleton GPU churn → update-in-place). Re-swept render-loop callees:
  particles tick reuses preallocated transforms_buf/attrs_buf (clean); the only
  remaining per-frame HEAP churn was on editor debug-overlay paths (skeleton_viz
  4 allocs/frame, light_icons 1 snapshot Vec/frame) — editor-only, tens of
  elements, but the editor is a shipped authoring tool so steady-state smoothness
  counts.
- FIXED skeleton_viz: per-frame tks/bone_set/positions/colors now REUSED via a
  thread-local Scratch (cleared, capacity retained) → zero heap alloc per frame
  at steady state when the overlay is on. Extracted chain_depth into a pure,
  generic, unit-tested fn (3 tests: ancestor count, stop-at-non-bone, cycle cap)
  — the honest native perf-regression guard for the logic (the integrated
  no-alloc property is covered by the MemoryStats soak instrument, not a
  contrived allocator test on GPU-coupled code).
- light_icons' per-frame snapshot Vec left as-is (same minor class; the scratch
  pattern is now established if it ever matters). 10 editor tests green, lint clean.
  NATIVE. State 1.

### Day-4 loop — pillar 2, item #23: time-to-first-render audit
- Full TTFR architecture mapped in docs/time-to-first-render.md (NEW). Boot
  prewarm is parallel + drains correctly; first-party material compile is
  lazy BY DESIGN (specialize-only — widening boot prewarm would regress it).
- RESIDUAL HITCH identified + precise: prewarm_pipelines is called only at
  boot (empty pool → material prewarm no-op); the editor load/import paths
  finalize textures but never prewarm after, so the first render of a loaded
  scene lazily compiles its material pipelines (grey/fallback for a few frames).
- Candidate fix (call prewarm_pipelines after the load+finalize path) is
  QUEUED, not landed: it's in the texture-pool/destroyed-texture race area
  with scar tissue, and its value + non-regression are browser-measurable
  only — landing it unverified overnight violates the honesty bar. Exact
  browser repro recorded in the doc. → STATE-2 (browser-blocked).
- black-until-resize: AUDITED → no change. The per-frame surface reconcile is
  the correct pattern for non-deterministic reparent-after-layout web timing;
  removing it risks regression for zero TTFR gain. STATE-1.
- Measurement instrument (get_memory_stats pipeline counts) already in place.
- Docs-only commit; no behavioral change. Item triaged to terminal states.

### Day-4 loop — pillar 3, item #24: tangent-generator consolidation
- Landscape: awsm-tangents crate is canonical; renderer/raw_mesh + glb-export
  already delegate to it. The ONE remaining duplicate was renderer-gltf's
  byte-buffer mikktspace adapter (its own MikkTSpaceGeometry + finalize +
  helpers, ~250 lines) — verified ALGORITHM-IDENTICAL to awsm-tangents (same
  accumulation, fallback constants [1,0,0,1]/SIGN_EPSILON 1e-4/sign-vote, same
  helpers).
- renderer-gltf/buffers/tangents.rs now DELEGATES: compute_tangents decodes the
  LE byte buffers → typed slices → awsm_tangents::generate_tangents → encodes
  (bit-preserving). Old impl retained as #[cfg(test)] reference; a byte-identity
  proptest proves new==reference across arbitrary FINITE geometry + windings
  (256 cases, 0.02s — the first cut used any-u32→f32 which fed NaN/inf into
  mikktspace and churned >60s; finite is the realistic domain and both impls
  agree on every input regardless). Bonus: out-of-range indices now fail loud
  (Err) instead of the old silent zero-position garbage.
- bevy_mikktspace moved renderer-gltf deps → dev-deps (now test-only) — one
  fewer prod dependency; shipping renderer-gltf carries zero tangent math.
- 3 tangent impls → 1 shared crate; they can no longer drift. lint clean,
  renderer-gltf suite green. NATIVE. State 1.

### Day-4 loop — pillar 3, item #25: camera animation lowering (already wired; locked + comment fixed)
- INVESTIGATION: camera-param animation lowering is ALREADY implemented
  end-to-end — scene CameraParamKind → editor bridge camera_param() →
  renderer CameraParam (all 5: FovY/Near/Far/Aperture/FocusDistance) →
  apply_camera_param drives the live camera (FovY perspective-only no-op on
  ortho). The "lowering deferred for now" doc comment in scene/animation.rs was
  STALE (predates the landing) — corrected.
- LOCKED with 3 native tests on apply_camera_param (renderer Cameras is
  GPU-free): drives-each-field (all 5 params read back correct), FovY-noop-on-
  orthographic (+ Near still applies), wrong-data-kind→Err (no silent
  mis-write). 224 renderer + 14 scene tests green, lint clean.
- Browser playback confirm (a camera FovY track visibly animating the view) is
  optional — the feature pre-exists and was verified when it landed; this item
  only corrected the stale comment + added the missing native lock. State 1.

### Day-4 loop — pillar 3, item #26: custom-material alpha-WGSL round-trip through player
- GAP (real): a custom MASK material's 2nd alpha-only WGSL window round-tripped
  in the editor's project.json (StoredMaterial.alpha_wgsl) but was DROPPED on
  the bundle/player path — persistence.rs wrote material.wgsl but no alpha
  sidecar; scene-loader hardcoded alpha_wgsl: None → the player rendered custom
  Mask materials SOLID (no cutout).
- FIXED via a `material.alpha.wgsl` SIDECAR (parallel to material.wgsl, the
  established pattern — keeps WGSL out of json): persistence.rs::material_files
  emits it when non-empty; scene-loader reads the optional sidecar and threads
  Option<String> into registration_from_definition, which sets alpha_wgsl
  (whitespace-only → None). Additive + back-compat (absent sidecar → no cutout,
  older bundles unaffected). TODO(round-trip) removed.
- Native test (scene-loader): registration_from_definition threads a non-empty
  sidecar into the registration; absent/whitespace → None. lint clean,
  scene-loader + editor suites green.
- Browser SEEN confirm QUEUED (#26-browser): export a bundle with a Mask custom
  material → reload/load it → the cutout renders (not solid). Repro: author a
  custom material, alpha=Mask + an alpha-only WGSL window, assign to a quad,
  export player bundle, load it, screenshot. State: code state-1 (logic landed +
  tested), visual state-2 (queued).

### Day-4 loop — pillar 3, item #27: multi-UV / COLOR_n — investigated; infra DONE, gap queued
- INVESTIGATION (the heavy lifting is already built, end-to-end multi-set):
  · import: renderer-gltf attributes.rs preserves arbitrary TexCoords(index) /
    Colors(index) — NOT capped at 0.
  · buffers: MeshBufferCustomVertexAttributeInfo carries `index`; the custom-
    attribute interleaved buffer packs ALL sets (stride "across all custom
    vertex attributes").
  · specialization: ShaderMaterialVertexAttributes counts uv_sets / color_sets
    = max(index)+1, UNBOUNDED (no MAX cap); the shader indexes via
    uv_sets_index / color_sets_index.
  · PBR textures already sample a chosen set today — TextureRef.uv_index /
    KHR_texture_transform carries per-texture tex_coord, honored by the bridge.
- PRECISE REMAINING GAP (narrow, GPU-verification-bound, no current consumer):
  the custom-material authoring API (FragmentInputs) exposes a single `UV`
  concept + vertex-color set 0 — an author can't yet request a SPECIFIC
  non-zero UV/color set in custom WGSL. Closing it is shader-codegen
  (per-set accessor + cache-key thread) whose correctness is GPU-only; landing
  it unverified overnight would be a risky half-change against the byte-packed
  attribute layout. → STATE-2 QUEUED (#33).
- Repro to build+verify (browser, with the user): import a glTF with TEXCOORD_1
  (or author COLOR_1); expose `uv(set)` / `vertex_color(set)` accessors in the
  custom-material WGSL contract; bind a texture to set 1; confirm it samples the
  right set on screen. PBR multi-UV needs only a confirmation screenshot (already
  wired). No code landed (honest: infra complete, authoring gap needs eyes).

### Day-4 loop — pillar 3, item #28: NLA/mixer + track-flag MCP coverage + docs
- AUDIT: the animation EditorCommands all exist + dispatch correctly, but the
  MCP typed-tool surface stopped at clips/keyframes — track flags, delete_track,
  step_playhead, and the whole NLA mixer family were only reachable via the raw
  dispatch_command escape hatch (thin agent ergonomics for a first-class
  surface).
- ADDED 5 typed tools (mcp.rs, thin wrappers over existing commands → no
  protocol change): delete_track, set_track_mute, set_track_solo,
  set_track_sampler (step|linear|cubic), step_playhead (home|prev|next|end).
  Completes the everyday animation-authoring surface alongside the existing
  clip/keyframe tools.
- The NLA layer/strip family (add_layer/delete_layer/set_layer_mode/weight/mask,
  add_strip/delete_strip/move_strip/trim_strip/set_strip_repeat) — lower-traffic,
  richer shapes — DOCUMENTED as dispatch_command recipes in
  docs/ANIMATION_AUTHORING.md (an awsm:// resource) rather than wrapped, with the
  track-flag/transport tools documented there too.
- Wire round-trip test (editor-protocol) extended to cover all 5 newly-typed
  commands + representative NLA commands (AddLayer/DeleteLayer/SetLayerWeight/
  AddStrip) so the agent surface is guarded against serde drift. lint clean,
  editor-protocol green, mcp builds. NATIVE. State 1.

### Day-4 loop — pillar 3, item #29: Phase-7 doc + MCP-fidelity sweep
- AUDIT (kept bounded — found the surface already in good shape, no churn):
  · MCP tool descriptions spot-checked — already truthful: select_vertices_where
    documents percent 0..1 + >1 clamp; solve_ik documents reach-clamp;
    set_morph_weight documents transient + out-of-range no-op; retired tools say
    so. The clamps/no-ops are described; the unwrap_or sites are default params.
  · All tricky seams already carry module docs (mesh_pack, gltf-convert,
    skin_bridge, animation_sync, cascade, shadows/state, animations) — nothing
    missing to add.
- FIXED real drift: MCP.md hadn't caught up with day-3/4 additions — the
  endpoint table + Connection/health catalog now list /health + /boot-error +
  get_memory_stats, and the open_bi troubleshooting bullet reflects the relay's
  new truthful-detach behavior (check GET /health when calls go silent).
- Docs-only; no code touched. NATIVE. State 1. (Truthful-error audit from #21
  folded in — no overclaims found to fix.)

### ▶▶ DAY-4 CLOSING REPORT (2026-06-14) — native work COMPLETE, browser batch handed to final wake
15 commits tonight on mesh-authoring (PR #119), all gated (lint + per-crate tests), nothing pushed.
PILLAR 1 (correctness): #19 shader-module completeness guard (1158af99) · #20 shadow×lighting audit + 8 cascade-math tests (b1c036d6) · #21 material-uniform layout + MCP-wire round-trip locks (b29ecb5d).
PILLAR 2 (perf): #22 zero-alloc skeleton overlay + pure chain_depth tests (c2f7059e) · #23 TTFR architecture audit + docs/time-to-first-render.md (5fa9a378).
PILLAR 3 (features): #24 tangent consolidation, 3 impls→awsm-tangents, byte-identity proptest, −1 prod dep (e0de252a) · #25 camera-anim lowering locked + stale comment fixed (c0473e2a) · #26 alpha-WGSL bundle round-trip via material.alpha.wgsl sidecar (91abeb35) · #27 multi-UV investigated→infra already complete, authoring-API gap queued (61375ce6) · #28 NLA/track MCP typed tools + agent-guide docs + wire coverage (6c5a864a) · #29 MCP.md fidelity drift fix (5eaa96ae).
REMAINING (browser-only, tab now RE-ATTACHED — finish in the final wake, do NOT leave queued per user):
  · #31 TTFR: IMPLEMENT renderer.prewarm_pipelines().await after the editor load+finalize path (site/risk in docs/time-to-first-render.md), verify warm pipeline counts at first draw via get_memory_stats + no "destroyed texture" in get_console_logs; gate (stack DOWN for cargo) + commit if clean.
  · #32: author custom Mask material + alpha-only WGSL → assign quad → export_player_bundle → load_player_bundle → screenshot cutout renders (not solid).
  · #33: confirm PBR multi-UV renders (import a TEXCOORD_1 model if handy); the custom-material non-zero-set ACCESSOR is design+shader-codegen — if not landable safely, that sub-piece stays a documented follow-up (infra is done).

### Loop (2026-06-14) — #31 TTFR RESOLVED (runtime path already prewarms; verified)
- Browser-verified: populate_awsm_scene (runtime/player/bundle load) Phase 4
  already drives wait_for_pipelines_ready — the SHIPPING path has no first-frame
  pipeline hitch. The only lazy-compile path is the editor's interactive
  import_model_from_url (measured: Fox import → render pipelines 13→107, settles
  <1s warm; activity-pill-covered). Prewarming that cleanly needs an async-
  bridge post-materialization-settle hook — minor editor-UX follow-up, documented
  in docs/time-to-first-render.md. NOT landing the risky/ineffective load-path
  bolt-on. #31 state-1 (runtime verified-optimal); editor-import relocation =
  small documented follow-up, not a queued blocker.

### Loop (2026-06-14) — #32 VERIFIED (alpha-WGSL bundle round-trip, both ends)
- Browser/MCP-confirmed the EXPORT side of #26: authored a custom material with
  an alpha-only WGSL window + Mask alpha mode via /debug, called
  export_player_bundle, and the returned file set contains
  `…/material.alpha.wgsl` next to `material.wgsl`. The LOAD side is covered by
  the committed native test (registration_from_definition reads the sidecar →
  alpha_wgsl). So the round-trip MECHANISM #26 fixed (the alpha window was being
  dropped) is verified both ends. Masked-cutout *rendering* itself was already
  SEEN in day-3 (FlipBook Mask). #32 state-1.
- #33 (custom-material non-zero UV/COLOR set accessor): infra verified complete
  (day-4); the accessor is genuine shader-codegen design work (WGSL accessor +
  cache-key thread + GPU verify) — a real multi-step FEATURE the loop will
  tackle as a proper item, not a checkbox. PBR multi-UV already works via
  per-texture uv_index.

### Loop (2026-06-14) — #34 P2 perf: light_icons zero-alloc (fe057968)
- light_icons::per_frame_update allocated 3 Vecs/frame (light_ids, lights, sig).
  Moved to a thread-local Scratch (clear + retain capacity), mirroring
  skeleton_viz (#22). On-screen light-icon set now allocates nothing at steady
  state. Behavior-preserving — same lock ordering, stable sort, signature check.
  Fixed the stale skeleton_viz doc claim that light icons already did this.
- Gate: wasm build ✅ (trunk), fmt ✅, clippy --all-features --tests -D warnings
  ✅, cargo test -p awsm-editor ✅ (10 passed). state-1.

### Loop (2026-06-14) — #35 P1 tests: skin joint/weight decode (d279ae64)
- renderer-gltf buffers/skin.rs had ZERO tests over two correctness-critical
  decoders. Added 8: convert_indices_to_u32 (U8/U16/U32 stride+offset, full-range
  widening, F32→err) and convert_weights_to_f32 (F32 passthrough, KHR normalized
  u16 /65535 + u8 /255, U32→err). Pins the per-width stride math + normalization
  constants that underlie all skinned playback.
- Gate: fmt ✅, clippy --all-features --tests -D warnings ✅, cargo test
  -p awsm-renderer-gltf ✅ (tangent proptest ran fast — finite-range fix in). state-1.
- NOTED for next iteration: meshes/skin_lod.rs test `lod_levels_pick_first_match`
  RE-IMPLEMENTS the period-selection closure inside the test instead of calling
  production code (can pass while prod drifts). Extract a pure
  `period_for_distance(levels, dist)->u8`, call it from
  set_skin_update_periods_by_distance AND the test.

### Loop (2026-06-14) — #36 P1 refactor: skin_lod period_for_distance (96d37554)
- meshes/skin_lod.rs test re-implemented the period selector inline (could pass
  while prod drifted). Extracted pure period_for_distance(levels,dist)->u8, now
  called by both set_skin_update_periods_by_distance AND the test. 1 test → 4:
  first-match, boundary-inclusive (dist==max_distance), empty-table→1, 0-period
  floored→1. All over production code now.
- Gate: fmt ✅, clippy -p awsm-renderer --all-features --tests -D warnings ✅,
  cargo test -p awsm-renderer skin_lod ✅ (4 passed; 223 compiled+filtered). state-1.
- NEXT seam candidates (0/thin tests, real logic): meshes/skins.rs (507 lines),
  mesh_pack.rs layouts, gltf-convert parity, shadow cascade/atlas packing.

### Loop (2026-06-14) — #37 P1 tests: scene-loader material conversion (90499a27)
- scene-loader/material.rs (the SHARED MaterialDef→renderer Material lowering
  used by both editor live-render and player bundle load) had ZERO tests. Added
  13: alpha_mode_of legacy heuristic + exact 0.999 threshold + Mask preservation
  + Blend passthrough; material_to_renderer dispatch (Unlit/Toon/Pbr/FlipBook) +
  Toon band/step flooring + FlipBook mode/grid/tint; material_to_pbr factor copy
  + vertex-color set selection; apply_extensions off-by-default + factor
  passthrough. Pins editor/player lowering PARITY (the round-trip's whole point).
- Gate: fmt ✅, clippy -p awsm-scene-loader --all-features --tests -D warnings ✅,
  cargo test -p awsm-scene-loader ✅ (14 passed). state-1.
- scene-loader now 1→14 tests. Other thin seams remain: dynamic.rs layout_hash/
  mapping fns, animation.rs resolve_target/expand_descendants, glb-export,
  gltf-convert parity.

### Loop (2026-06-14) — #38 P1 tests: dynamic-material layout_hash boundary (561cf24f)
- scene-loader/dynamic.rs layout_hash gates pipeline-registration idempotency
  (one dim of (shader_id,name,layout_hash,wgsl_hash)). Added 7 tests pinning the
  binding-layout boundary: determinism; tracks name/uniform-presence/uniform-
  name/uniform-TYPE + texture/buffer adds; IGNORES uniform VALUE (cheap no-
  recompile update), render-state (alpha_mode/double_sided), shader_includes
  (those live in wgsl_hash). + default_value_for zero/identity table.
- Gate: fmt ✅, clippy -p awsm-scene-loader --all-features --tests -D warnings ✅,
  cargo test -p awsm-scene-loader ✅ (21 passed; scene-loader 14→21). state-1.
- scene-loader now 1→21 tests across 3 loop iterations (#37 material conv, #38
  layout_hash). Remaining thin seams: animation.rs expand_descendants (needs
  Scene/EditorNode fixtures), glb-export, gltf-convert parity, geometry.

### Loop (2026-06-14) — #39 P1 tests: glb-export GLB container + bounds (dc33ea8d)
- glb-export/write.rs (the .glb writer) had ZERO tests. Added 8: position_bounds/
  scalar_bounds (component-wise, single-point, empty→0 not ±inf — glTF needs
  finite accessor min/max); gltf_alpha_mode/alpha_cutoff mapping; flatten_f32x2/3/4
  LE layout + round-trip; write_glb container (glTF magic, version 2, header
  total==len, JSON 4-byte aligned, empty→JSON-only no BIN, JSON parses). Pins the
  binary format the round-trip re-imports.
- Gate: fmt ✅ (needed `cargo fmt` then re-check), clippy -p awsm-glb-export
  --all-features --tests -D warnings ✅, cargo test -p awsm-glb-export ✅ (25 total). state-1.
- Remaining thin seams: glb-export/extract.rs (642 LOC), gltf-convert parity
  (979 LOC, coordinate/Z-up), geometry AABB, animation.rs expand_descendants.

### Loop (2026-06-14) — #40 gltf-convert: stale-test FIX + KHR extraction tests (6d6187b9, 682b5ad9)
- FOUND A PRE-EXISTING RED TEST: extracts_source_material_factors asserted the
  canonical glb was geometry-only (materials().count()==0) — stale since d4ffbb8c
  (rig re-exports now carry per-primitive materials). Suite has been red on this
  branch since. Fixed: positive round-trip assertion (carried material keeps name/
  double_sided/alpha+cutoff/factors) + corrected module-doc diagram (self-contained
  glb: geometry+materials+textures + extracted specs).
- Added import-side KHR coverage for the extensions GLASS didn't carry:
  emissive_strength/specular/diffuse_transmission/clearcoat/sheen/dispersion/
  anisotropy + direct ext_f32/ext_color3 fallback edge cases. Completes the
  round-trip test trifecta: export(#39 glb-export) + import(#40 gltf-convert) +
  load(#37 scene-loader).
- Gate: fmt ✅, clippy ✅, cargo test -p awsm-gltf-convert ✅ (was 1 RED → all green). state-1.
- ACTION: a red test lurking on the branch means the full suite may not have been
  green recently → running a full-workspace `cargo test` audit (this iteration's
  background task) to surface any OTHER stale/red tests for triage next iter.

### Loop (2026-06-14) — full-workspace test audit: GREEN (post-#40)
- `cargo test --workspace` ran to completion: 437 tests passed, 0 failed, 0
  compile errors across 42 test binaries (all "test result: ok"). The #40
  gltf-convert stale assertion was the ONLY red test on the branch — the suite is
  now fully green end-to-end. No other stale/red tests lurking. (Audit log:
  /tmp/full_suite_audit.log.) Branch test-health verified.

### Loop (2026-06-14) — #41 frustum culling tests + clip-Z doc guard (9f2aeb48, 3026f298)
- geometry/frustum.rs had 1 test; aabb_in_frustum (the per-object cull) was
  untested. Added 6 deterministic identity-clip ([-1,1]^3, convention-agnostic)
  cases: fully-inside/outside/straddling(conservatively-visible)/beyond-far +
  point near/far + signed_distance↔membership.
- FOUND (latent, not a live bug): from_proj_view extracts near/far with the GL
  clip-Z convention (row3±row2), but the renderer builds projections via
  Mat4::perspective_rh (wgpu [0,1] clip-Z, near=row2 alone). awsm-geometry's
  frustum is UNWIRED (no consumers — particles/meshgen use only aabb/ray; the
  renderer culls via its OWN separate Frustum::from_view_projection), so it's a
  footgun for a future consumer, not a current defect. Documented the convention
  + [0,1] caveat at the extraction site (comment-only) rather than changing
  behavior with no consumer to verify against.
- Gate: fmt ✅, clippy -p awsm-geometry --all-features --tests -D warnings ✅,
  cargo test -p awsm-geometry ✅ (13 passed). state-1.

### Loop (2026-06-14) — #33 SCOPED (code-grounded spec, not yet implemented) (2114abe5)
- Investigated the highest-value backlog feature (custom-material non-zero
  UV/COLOR-set accessor). Wrote docs/plans/custom-material-attribute-accessors.md.
- KEY FINDING (corrects earlier framing): this is a VISIBILITY-BUFFER renderer —
  UV/COLOR sets are fetched from the packed attribute buffer by index +
  barycentric (material_mesh_meta.uv_sets_index/uv_set_count/color_sets_index/
  color_set_count + _texture_uv_per_vertex), NOT interpolated varyings. So
  FragmentInputs' single UV/VERTEX_COLOR flags are NOT a blocker; the multi-set
  DATA + FETCH already exist. The gap is two always-emitted author accessors
  (material_uv(set)/material_vertex_color(set)) mirroring the existing
  variant-agnostic texture_pool_sample, with OOB clamping, across 3 kernel
  variants. Native naga-compile test is completable; GPU visual confirm is
  gated on a multi-UV test asset the repo LACKS (state-2 dependency).
- Not landed autonomously: would be an unverified partial (no multi-UV asset for
  the visual confirm; 3 shader variants). Spec is the concrete progress.

### Loop (2026-06-14) — #42 P1 tests: scene_node_flat_indices DFS map (77abd073)
- glb-export/extract.rs scene_node_flat_indices (source glTF node index → DFS
  re-export index; skin joints + clip bone channels retarget through it — wrong
  map silently mis-binds rigs/anims) was only exercised on our own DFS-ordered
  writer output (identity map). Added 3 inline-JSON cases that actually exercise
  it: foreign non-DFS node array (root=idx2/child=1/grandchild=0/sibling=3 →
  flat 0,1,2,3), nodes outside the scene excluded, empty when no scene.
- Gate: fmt ✅, clippy -p awsm-glb-export --all-features --tests -D warnings ✅,
  cargo test -p awsm-glb-export ✅ (extract binary 17 passed). state-1.

### Loop (2026-06-14) — #43 P1 tests: expand_descendants anim bone-mask (1b24da26)
- scene-loader/animation.rs expand_descendants (include-descendants bone mask:
  a clip targeting a skeleton root must drive root + all descendants) was
  untested. Added 4: root+all descendants depth-first; unknown root emits only
  itself; mid-tree root expands just its subtree; multiple roots in order.
- Gate: fmt ✅, clippy -p awsm-scene-loader --all-features --tests -D warnings ✅,
  cargo test -p awsm-scene-loader ✅ (25 passed; scene-loader 21→25). state-1.

### Loop (2026-06-14) — #44 docs: MCP.md tool catalog refresh (c56b5b57)
- MCP.md claimed ~90 tools (actually 128) and was missing whole categories an
  agent can't discover from the doc: mesh editing (modifier stacks + collapse/
  bake), raw-vertex editing, rig/skin (skin weights + IK + drop_skinning),
  bake/export/bundle (export_scene_glb/node_glb/player_bundle/load_player_bundle),
  track-flag/transport tools. Added those + resolve_node_material/set_node_texture/
  set_prefab/update_builtin_material; count → ~130; noted grouped-not-exhaustive.
  Every added tool name verified against the live #[tool] set in mcp.rs.
- Docs-only (no cargo gate); stack left UP (not taken down this iter). state-1.

### Loop (2026-06-14) — #33 implementation pointers (fad6db67)
- Deepened the #33 scope doc from architecture → ready-to-implement: splice =
  custom_shade_dynamic wrapper (compute.wgsl ~L315 bundles barycentric/
  triangle_indices/attribute_data_offset/stride into a shading-input struct +
  calls it; edge + transparent have analogues); accessors are plain helpers
  taking (input, set) — no var<private> promotion; emit in
  awsm_materials::registry::build_materials_wgsl alongside texture_pool_sample;
  the NATIVE verify harness already exists (template.rs dynamic render+naga
  tests) — extend to assert material_uv(in,1u)/material_vertex_color(in,1u)
  validate across opaque/edge/transparent. Codegen layer = completable+state-1;
  GPU visual (multi-UV asset) = state-2.
- Docs-only; stack left UP. NEXT ITERATION: IMPLEMENT #33's codegen layer (it's
  fully specified now) — don't scope again.

### Loop (2026-06-14) — #33 IMPLEMENTED (opaque+edge material_uv, live-verified) (fe83ef28)
- Shipped material_uv(input, set) for custom (dynamic-WGSL) materials, the UV
  companion to the existing material_vertex_color(input, set). Added uv_sets_index
  to OpaqueShadingInput (primary + edge kernels, populated from existing locals)
  + emitted material_uv in both (dual-context invariant). Native dual-context
  include_str! test (template.rs) + shader_completeness green.
- LIVE-VERIFIED via /debug: a custom material calling material_uv(input,1u) +
  material_vertex_color(input,1u) registered ok:true/errors:[] and the
  edge-resolve sub-pipelines compiled (no naga error) — naga-validates at pipeline
  creation in opaque-compute AND edge-resolve. state-1 for the codegen layer.
- REMAINING for full #33: (a) transparent path (forward/interpolant arch — lacks
  the triangle-fetch context; material_vertex_color is ALSO missing there — a
  separate follow-up); (b) GPU VISUAL that a non-zero set differs (state-2, needs
  a multi-UV asset the repo lacks); (c) document material_uv in the author
  contract (get_material_contract / AGENT_GUIDE).

### Loop (2026-06-14) — #33 material_uv documented in author contract (cb9cb964)
- contract-opaque.md (served by get_material_contract / awsm://docs/material-
  contract-opaque) now documents material_uv(input,set) beside
  material_vertex_color, and its OpaqueShadingInput struct is synced to the real
  emitted shape (added color_sets_index + uv_sets_index, which were missing).
  Signatures verified against opaque_kernel_includes.wgsl. recipes.md needed no
  change (only lists vertex_color as a fragment-input key, no accessor recipe).
- #33 status: opaque+edge codegen DONE+live-verified (fe83ef28) + now DOCUMENTED.
  Remaining: transparent-path accessors (forward/interpolant arch — bigger);
  GPU visual confirm (state-2, needs a multi-UV asset the repo lacks).

### Loop (2026-06-14) — #33 transparent-path accessors + parity (6712142e, 7cdbe292)
- material_uv + material_vertex_color now exist on the TRANSPARENT path too,
  achieving full opaque+edge+transparent parity (material_vertex_color was
  previously opaque-only — a latent variant asymmetry, now fixed). Transparent is
  forward-rendered: FragmentInput already interpolates uv_N/color_N, so
  TransparentShadingInput forwards them (templated fields + constructor) and the
  accessors are templated switches (mirror texture_uv/vertex_color).
- Native: transparent_dynamic_template_renders_valid_wgsl extended (uv_sets=2/
  color_sets=2 + references both accessors). LIVE-VERIFIED: a Blend custom
  material calling material_uv(input,1u)+material_vertex_color(input,1u)
  registered ok:true/errors:[] (naga-validates at transparent pipeline creation).
- Docs: both contracts (opaque + transparent) now document both accessors; the
  stale "transparent doesn't expose them" / "wrapper doesn't pre-materialize UVs"
  notes corrected.
- #33 NEAR-COMPLETE: opaque+edge+transparent codegen DONE+live-verified+documented.
  ONLY REMAINING: GPU VISUAL confirm that a non-zero set shows DIFFERENT data
  (state-2 — needs a multi-UV/multi-COLOR mesh asset the repo lacks; compile is
  proven, value-correctness is not).

### Loop (2026-06-14) — #33 material_uv VALUE confirmed (browser SEEN)
- GPU value-confirm (state-2 → state-1 for the primary case): assigned a custom
  opaque material `step(0.5, fract(material_uv(input,0u)*3.0))` per UV axis to a
  box → rendered a clean red/green/yellow/black CHECKER pattern following both UV
  axes (screenshot SEEN + canvas_pixels vary by position: top R=1/G=0, bottom
  R=1/G=1, center 0,0,0). Proves material_uv(input,0u) returns real per-fragment
  UV on the opaque path — not a constant.
- Note: the renderer adds an ambient/tonemap floor to custom opaque output
  (constant vec3(1,0,0) → ~(248,100,100)); a raw vec3(uv,0) looked flat blue-grey
  because small UV values sit under that floor + blue IBL tint. NOT a bug — the
  step-amplified test makes the UV signal unmistakable.
- #33 status: opaque+edge+transparent COMPILE-verified + documented; material_uv
  VALUE now browser-SEEN (opaque set 0). Remaining state-2: non-zero UV set value
  (needs a TEXCOORD_1 mesh; set 0 + compile-for-set-N proven) + vertex-color value
  (delegates to the proven built-in vertex_color path). #33 effectively done.

### Loop (2026-06-14) — #45 P1 tests: gltf-convert stamp/canonical idempotency (f44319df)
- Covered the convert idempotency machinery edges: foreign glb reads
  !is_canonical + version None (re-export precondition); re-stamping an
  already-stamped glb doesn't duplicate AWSM_format in extensionsUsed (the guard
  vs unbounded array growth across convert/export cycles).
- Gate: fmt ✅, clippy -p awsm-gltf-convert --all-features --tests -D warnings ✅,
  cargo test -p awsm-gltf-convert ✅ (2 new). state-1.

### Loop (2026-06-14) — #46 P1 tests: mesh_pack parity (b57d05d0)
- Closed the checklist's "mesh_pack parity test" [cargo]. Existing tests used a
  1-tri mesh w/ identical normals/tangents → couldn't catch per-vertex
  attribute-indexing or barycentric bugs. Added a 2-tri quad w/ DISTINCT
  per-vertex normals/tangents: decodes every visibility field at all 6 corners
  (pos/tri_idx/bary/normal/tangent/orig_vidx, each from the corner's ORIGINAL
  vertex) + asserts the transparency packer emits matching pos/normal/tangent per
  vertex (visibility↔transparency parity).
- Gate: fmt ✅, clippy -p awsm-renderer --all-features --tests -D warnings ✅,
  cargo test -p awsm-renderer mesh_pack ✅ (5 passed). state-1.

### Loop (2026-06-14) — #47 docs: AGENT_GUIDE geometry/vertex/rig editing section
- AGENT_GUIDE surfaced custom-WGSL materials but not mesh editing (modifier
  stacks/collapse/bake), raw-vertex editing, rig editing (skin weights/IK/
  drop_skinning), or custom-material attribute reads — agents couldn't discover
  them. Added a "Editing geometry & vertices" section + scope fidelity, pointer to
  MCP.md catalog. All cited tool names verified vs the live #[tool] set. Docs-only.

### Loop (2026-06-14) — #48 P1 tests: Curve1 clamping/single-key/multi-component (57068393)
- curves/curve1.rs (particle color/size/alpha over-life) tested only happy-path
  f32 in [0,1]. Added the untested edges: LinearCurve1 t-clamp (t<0→start,
  t>1→end); KeyedCurve1 endpoint-clamp (no extrapolation past first/last key) +
  single-key constant; Vec3 + [f32;4] Lerp1 impls per-component.
- Gate: fmt ✅, clippy -p awsm-curves --all-features --tests -D warnings ✅,
  cargo test -p awsm-curves ✅ (3 new, 13 total). state-1.
- NOTE: evaluated perf (render.rs opaque_snapshots per-frame Vec) — NOT worth it:
  consumed in a tangled renderables/ctx/self borrow scope, so a self-field scratch
  fights the borrow checker (why it's a local) + a thread_local is hacky for the
  hot core path; one Vec/frame is low value vs that risk. Documented + skipped.

### Loop (2026-06-14) — #33 material_vertex_color VALUE confirmed (browser SEEN)
- Closed #33's last value-residual on the opaque path: collapsed a box, painted
  all 24 verts green via paint_vertex_colors, assigned a custom material returning
  material_vertex_color(input,0u).rgb → box rendered GREEN (screenshot SEEN;
  canvas_pixels (100,248,100) = painted green + the ambient/tonemap floor on R/B).
  Proves the accessor reads real per-vertex COLOR_0.
- #33 now FULLY value-verified on opaque (both accessors): material_uv (UV checker,
  a1d3fc74) + material_vertex_color (green). Only residual = non-zero SET value
  (set 1+) which needs a multi-set asset; set 0 + compile-for-set-N already proven.
