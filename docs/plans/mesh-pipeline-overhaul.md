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
- **Never write "lockstep"** or its repo path into committed files (see memory).
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
  (b) LEFT: `data:`-URI image bytes (needs a base64 dep) — the last convert edge.
  (c) Phase 7 doc/MCP fidelity sweep.
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
