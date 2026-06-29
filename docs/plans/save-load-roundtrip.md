# Save/Load Roundtrip Fidelity — Comprehensive Plan

**Goal:** A project Save → Load is **lossless**. Whatever is in the editor's
in-memory "converted format" after an import/edit is exactly what you get back
after save → reload — geometry, textures, materials, animations, geometry edits,
modifier stacks, skinning/rigs, morphs, LOD/nanite, environment, lights, cameras,
particles, decals, curves, scene structure. No silent drift, ever.

**Invariant (the law):** Loading and Saving are each ONE transaction. Save
serializes the complete in-memory converted format; Load reconstructs it exactly.
If a Save cannot be lossless it must FAIL LOUDLY (never silently write a partial
project that overwrites a good one). See `check_save_complete` (already shipped).

**Status legend:** ✅ lossless · ⚠️ partial/conditional · ❌ drops · 🔬 needs verify

---

## STATUS — substantially complete (2026-06-29)

**Root cause of the data loss FOUND + FIXED:** the project Save was a fire-and-forget
`spawn_local`; triggering anything else mid-write (or navigating) cut the write loop off
at a variable point → silent partial project. Fixed by a **blocking save modal**
(`app.rs` → `begin_activity` → `busy_overlay`) + **per-file write-verify** (`fs.rs`) +
**save-completeness guard/census** (`persistence.rs`). The drift was never in extraction
or the caches (oracle proved both complete).

**Shipped this effort (all compile; touched-crate tests green — 302+32+…):**
geometry interrupted-save fix · authored-tangent parity through the static/captured +
skinned + player paths (P0-C) · external-URI texture capture (P0-A) · KTX/HDR env
persistence (P1-A) · confirmed custom-material WGSL reload already works (P1-B) ·
no-browser extraction oracle + in-editor `SaveCensus` query (Phase 0) · modifier-stack
roundtrip test (P2-D) · stale-comment/TODO cleanups (P2-E) · authored-tangent rig-glb
fix (earlier). **No player perf regressions** (encoded_images/tangents are load-time,
never on the render hot path; player keeps `None`⇒regenerate).

**Open (need a product decision, NOT regressions):** P1-C procedural-texture persistence
(no recipe/bytes today), P2-A runtime morph multi-track mixing (a feature, not a
roundtrip bug). P0-D robot dark-patch is most likely lighting/orientation (robot has no
authored tangents) — verify visually if it recurs.

---

## 0. Corrected root cause (read this first)

An earlier hypothesis blamed a worker-thread boundary. **That was wrong** and is
recorded here so nobody re-chases it:

- `GltfParseJob` (`packages/crates/renderer-gltf/src/worker_job.rs`) is **dead
  code** — it is `register`'d (`engine/context.rs:135`) but **never `dispatch`'d**.
  The editor import (`ImportModelFromFile` → `bridge/gltf.rs:import_typed` →
  `GltfLoader::load`) and the player (`scene-loader` → `GltfLoader::from_glb_bytes`)
  both parse **inline on the main thread**. There is no worker byte-transfer loss.

The drift has two real, independent causes:

1. **Textures (confirmed):** `bridge/gltf.rs:268` builds the persistence image map
   via `extract_texture_images(&data.doc, &data.buffers.raw)` — it reads ONLY the
   embedded buffer bytes and an **empty** external-image pool. The loader already
   retains the encoded bytes in `GltfLoader.encoded_images` (loader.rs:71, incl. a
   best-effort URI re-fetch), but the persistence path **ignores `data.encoded_images`**.
   So any image whose bytes aren't trivially in `buffers.raw` (external-URI, or a
   read that comes up short) gets no `content_hash` → no `.png` written → white on
   reload. Variable 2/8…8/8 capture tracks which images resolve from buffers vs not.

2. **Geometry (drop CONFIRMED; exact mechanism to pin 🔬):** meshes are dropped too —
   empirically observed every run (e.g. `clean-save-4` = **30 of 38** `.mesh.bin`,
   earlier 10/16/18/36 of 38), with the "mesh list is empty" load error on the
   missing ones. The drop is real and is a P0 equal to textures. What still needs
   pinning is the precise cause: `extract_node_meshes` (`bridge/gltf.rs`)
   → `glb-export::extract_node_mesh` returns `None`/empty `MeshData` for a *variable*
   subset of nodes, so those never `mint_imported_mesh` into
   `mesh_cache` → not written → "mesh list is empty" on reload (re-bake fallback
   yields empty for captured-base meshes). The variability points at
   `data.buffers.raw` not being fully/consistently available at extraction time, or
   an accessor-bounds/padding mismatch between the extraction read and the renderer
   `populate_gltf` read (`loader.rs` ~206 padding applied after length validation).
   **First task is to pin this precisely** (see P0-B).

**Why it reproduces for the user but not headless MCP:** the failure is
load/timing/size sensitive on a real machine with a 26 MB model; a fast idle
headless tab wins the race every time. So verification is **layered** (see §2
Phase 0): the P0 byte-loss bugs live in **CPU-only extraction** and are caught by
**Rust tests with no browser** (the primary, CI-able oracle); the GPU/materialization
paths are caught by an **in-editor `VerifyRoundtrip` command** driven end-to-end.

**Infra — the loop manages its own editor (nothing is pre-running):**
- **Pick exactly ONE dev task — never both.** `mcp-dev` is a SUPERSET of
  `editor-dev` (both start the editor on :9085 + the media server on :9077; `mcp-dev`
  adds MCP on :9086). Running both, or starting one while the other is already up,
  collides on those ports. **Default: `task mcp-dev`** (covers everything). Use plain
  `task editor-dev` only if you explicitly don't want MCP.
- **Check before you start.** Probe the ports first (e.g. `curl -sf
  http://localhost:9085 >/dev/null` and `:9077`, `:9086`) and reuse a running server
  instead of launching a duplicate. Start the chosen task in the BACKGROUND once, and
  tear it down at the end of the run.
- Drive the editor via chrome-devtools `evaluate_script` calling
  `window.wasmBindings.editor_dispatch_json` / `editor_query_json` /
  `editor_snapshot_json` (proven: import via a `blob:` URL =
  `{cmd:"import_model_from_file", name, url}`). The `awsm-scene` MCP is OPTIONAL — if
  used (it ships with `mcp-dev`), navigate chrome-devtools to
  `http://localhost:9085/?mcp=http://127.0.0.1:9086&pair=<code>` to attach. Do NOT
  depend on a human-kept editor or a live MCP session.

---

## 1. Audit results (per subsystem)

### Geometry
| Item | Status | Notes / file |
|---|---|---|
| Imported static mesh geometry | ❌ | Variable drop at `extract_node_mesh`; not minted to `mesh_cache`. **P0-B** |
| Captured / editable (sculpt/collapse) meshes | ✅ | `.mesh.bin` (bitcode `CapturedMesh`); snapshot id via `captured_snapshot_id` also saved (persistence.rs ~220, 319) |
| Per-vertex overrides (pos/normal/color/uv) | ✅ | `MeshDef.overrides` inline in `project.toml`; test `vertex_overrides_uvs_roundtrip` |
| Modifier stacks (primitive/lathe/superquadric/sweep/sdf + modifiers) | ✅ | Inline; re-bake fallback `persistence.rs:709`. (No test exercises a non-empty modifier list — add one) |
| Sweep with deleted curve_node | ⚠️ | `mesh_eval.rs:95` `.unwrap_or_default()` → empty mesh. Low severity, document |

### Textures
| Item | Status | Notes / file |
|---|---|---|
| Imported raster, embedded-in-glb | ⚠️ | Works *when* buffer read succeeds; tied to P0-B buffer completeness |
| Imported raster, external-URI / short read | ❌ | `extract_texture_images` ignores `data.encoded_images`. **P0-A** |
| Procedural / generated textures | ❌ | GPU-only, no recipe captured. **P1** (capture recipe or bake to png) |
| Environment equirect panorama (png/jpeg) | ✅ | Rides `texture_cache` like an imported texture |
| Environment KTX2 / HDR cubemaps | ❌ | `env_sync.rs:31` `KTX_BYTES` session-local; never written to disk. **P1** |
| Node-inline texture binding (uv_index, transform, sampler) | ✅ | On `TextureRef`; depends on the asset's bytes persisting |

### Materials
| Item | Status | Notes |
|---|---|---|
| Built-in PBR scalars (base/metallic/rough/emissive/normal_scale/occlusion/alpha mode+cutoff/double_sided/shading/vertex_colors) | ✅ | All inline on `MaterialDef` |
| KHR extensions (emissive_strength, ior, transmission, diffuse_transmission, clearcoat, sheen, iridescence, dispersion, anisotropy, volume, specular) | ✅ | All serialized |
| Custom WGSL material **player** load (material.wgsl + material.json) | ✅ | `scene-loader` rebuilds registration |
| Custom WGSL material **editor Studio** reload (source back into the editor) | ❌ | `persistence.rs` header: "Reloading custom-material bodies into the Studio is the follow-on". **P1** |
| Material instance overrides (texture/buffer/uniform_overrides) | ⚠️ | Present; verify all hookups roundtrip. **P2 verify** |
| Per-node material assignment (assigned + inline) | ✅ | |

### Animation / Skinning / Morph
| Item | Status | Notes |
|---|---|---|
| Clips: name/duration/loop/speed/direction/color | ✅ | `animation.rs:stored_from_live` |
| Tracks: target/sampler/mute/solo/expanded | ✅ | |
| Keyframes: times/values/interp (Step/Linear/Cubic)/in+out tangents | ✅ | |
| Mixer / NLA doc (layers/strips/masks/weights) | ✅ | |
| Transport (current clip / playhead / playing) | ⚠️ | Reset on load by design (persistence.rs ~675). **Confirm acceptable** |
| Skinned rig (skeleton, inverse-bind, JOINTS/WEIGHTS, bone→NodeId) | ✅ | `rig.glb` via `reexport_clean_scene`; restored pre-apply |
| Bind-pose bakes (drop_skinning) | ✅ | `.bake.bin`; **stale TODO** at `skinned_bake_cache.rs:15` (now implemented — update comment) |
| Morph target geometry + default weights | ✅ | In rig.glb |
| Morph animation, multiple tracks same mesh, different indices | ⚠️ | Per-index masked blending deferred (`animation.rs:159`) — they stomp. **P2** |
| Skinned import completeness | 🔬 | Same extraction path as static — verify P0-B covers skinned rig export too |

### LOD / Nanite / other node kinds / scene
| Item | Status | Notes |
|---|---|---|
| LOD toggle (`MeshLodConfig.enabled`) | ✅ | Inline; LOD *bake* is export-time only (by design — confirmed) |
| Nanite ClusterMesh (view-only) | ✅ | `.clusters.bin` via `cluster_files`; restored pre-apply |
| Primitives / curves / instances / particles / decals | ✅ | Full configs inline |
| Lights (all params + shadow) / cameras (projection + behavior) | ✅ | |
| Scene tree: ids/name/TRS/kind/visible/locked/prefab/children/env/shadows | ✅ | |
| UI-only: expanded / asset_status / selection / vertex highlight | ❌ | Not persisted **by design** — acceptable |

---

## 2. The fix plan (phased, prioritized)

### Phase 0 — Test harness FIRST (so every later fix is verifiable)
- **0.1 Rust extraction oracle (no browser — PRIMARY).** A `cargo test` that loads
  each fixture glb via `GltfLoader::from_glb_bytes` → `into_data` → runs the editor's
  extraction (`extract_node_meshes`, `extract_texture_images` + `data.encoded_images`)
  and asserts: **every** mesh-bearing node yields a non-empty `MeshData`, and **every**
  texture yields encoded bytes. This is CPU-only and is where P0-A/P0-B live, so it
  pins both the RED repro and the GREEN fix deterministically in CI. Extend the
  existing serde roundtrip tests (`editor-protocol/tests/mesh_roundtrip.rs`,
  `material_roundtrip.rs`) to cover every subsystem struct (byte-equal through
  bitcode + toml).
- **0.2 In-editor `VerifyRoundtrip` command (end-to-end, GPU paths).** Add a debug
  `EditorCommand::VerifyRoundtrip` that `serialize_inmem` → clears **ALL** byte caches
  including `mesh_cache` (today's self-test deliberately skips it, `state.rs:1247`,
  which is exactly why the mesh drift hid) → `apply_inmem` → asserts per-subsystem
  **counts + byte-equality** (every `AssetSource::Mesh` non-empty; every raster texture
  has bytes; clip/track/keyframe counts; rig/bind/cluster present) and returns a JSON
  census via `editor_query_json`. The loop drives it over chrome-devtools against a
  self-started `task editor-dev` (see §0 Infra), over the fixture set (static
  multi-mesh, skinned, morph, sculpted, custom-material, nanite, KTX env) served on
  :9077. No human, no reliance on a live MCP.
- **Acceptance:** 0.1 reproduces the robot drift (a fixture mesh/texture count below
  golden) as a RED `cargo test`; 0.2 reproduces it end-to-end; both go GREEN only when
  the real fix lands.

### Phase 1 — P0 byte-loss fixes (the actual bugs)
- **P0-A Textures — use the bytes we already have.** Thread `data.encoded_images`
  into the persistence image map at `bridge/gltf.rs:268`: build `tex_images_by_index`
  from BOTH embedded buffer images AND `data.encoded_images` (give
  `extract_texture_images` / `ImagePool` the external bytes via the existing
  `with_external`/`reexport_clean_scene_with_images` path). Result: every texture the
  loader fetched gets a `content_hash` + `texture_cache` entry.
  - **Acceptance:** 8/8 textures hash + write `.png`, deterministically, for the
    robot and the external-URI fixture; harness green.
- **P0-B Geometry — RE-SCOPED (extraction is proven lossless, see 0.1).** The drop is
  NOT in `extract_node_mesh` (38/38 non-empty synchronously). So it's downstream in the
  editor's runtime: either (a) the import's `data.buffers.raw` is somehow incomplete at
  the editor call site despite `GltfLoader::load` awaiting it, or (b) `mint_imported_mesh`
  → `mesh_cache` population is partial/raced under real load, or (c) it's a
  resource/environment failure mid-import on the user's machine (reliable for them, never
  in headless). **Step 1 = measure with 0.2:** import the robot in a real editor and
  count `mesh_cache`/`texture_cache` vs the asset table (the renderer's status-bar "38
  meshes" is the GPU count, NOT the persistence cache — they can disagree). If 0.2 shows
  the editor cache < 38/8, instrument mint + the import flow to find the drop; if 0.2 is
  always 38/8 in headless too, the drop is environment-specific → reproduce on David's
  setup (and the shipped `check_save_complete` guard already prevents silent loss there).
  - **Acceptance:** 0.2 shows editor `mesh_cache`=38 & `texture_cache`=8 after import on
    every fixture; "mesh list is empty" load error gone; guard never fires.

### Phase 2 — P1 remaining real drops
- **P1-A Environment KTX2/HDR bytes:** persist `env_sync` `KTX_BYTES` as
  `assets/<id>.ktx2` side files (mirror `texture_files`/`restore_textures`); restore
  before env applies. **Acceptance:** KTX skybox/IBL roundtrips.
- **P1-B Custom-material Studio reload:** implement `restore_material_bodies` so a
  loaded project repopulates the WGSL editor (source/alpha/vertex/includes) from the
  `material.json`/`material.wgsl` side files, not just the player registration.
  **Acceptance:** open project → custom material is fully editable, source intact.
- **P1-C Procedural textures:** capture the generation recipe (or bake to an encoded
  png at save) so they roundtrip. **Acceptance:** procedural-texture fixture green.

### Phase 3 — P2 polish / confirm-by-design
- **P2-A** Per-index masked morph blending (`animation.rs:159`) so multiple morph
  tracks on one mesh don't stomp. **Acceptance:** 2-track-2-index morph fixture green.
- **P2-B** Verify material instance `texture/buffer/uniform_overrides` roundtrip; add
  fixture.
- **P2-C** Confirm with David: transport state (current clip/playhead/playing) and
  UI state (expanded/selection) intentionally NOT persisted. Document as accepted.
- **P2-D** Add a modifier-stack roundtrip test with a **non-empty** modifier list.
- **P2-E** Update the stale `skinned_bake_cache.rs:15` TODO (bind-pose persistence is
  done).

### Already shipped (this investigation)
- `check_save_complete` guard — refuses a lossy save (writes nothing) + per-save
  census log (`persistence.rs`). Keep as the permanent backstop; once P0 lands it
  should ~never fire.
- glb-export: authored TANGENTs preserved through the rig-glb roundtrip (fixes a
  separate dark-shading drift on skinned/metallic meshes).

---

## 3. Player performance — verdict: NO REGRESSIONS (one item to keep editor-gated)

The constraint: zero player perf regressions. Audit conclusion:

- **No worker change** — the worker is dead code; we are not touching the render
  worker or any per-frame path.
- `encoded_images` is **never consulted on the player render/animation hot path**
  (`scene-loader`/`populate` never read it). It is load-time, export-only.
- The loader **already** retains `encoded_images` for both editor and player (it's
  populated in `import_image_data`); P0-A only makes the **editor persistence path
  USE** what's already in hand. No new work on the player path.
- Player bundles are glb with **embedded** images → the loader's best-effort URI
  re-fetch does not fire for players (bytes are in the buffer "for free"). So even
  the load-time cost is editor/external-URI-only.
- Memory: retaining encoded bytes for a 26 MB model is an **editor-only**, transient
  load-time spike, released after the `.png`/`.mesh.bin` side files are written.
  Player bundles never carry encoded image bytes.

**One guard rail:** if P0-B ends up changing anything in `renderer-gltf` shared by
both paths (e.g. buffer materialization), gate any *added* retention/cost behind an
editor-only flag (e.g. `retain_encoded_images` on the import input) so the player
path is byte-for-byte unchanged. **Flag to confirm before kickoff:** P0-A/P0-B as
scoped touch only the editor `bridge/gltf.rs` persistence extraction + (for P0-B)
buffer-completeness — **no player render regression expected.** If P0-B's fix must
live in shared `renderer-gltf`, we gate it; that's the only place to watch.

---

## 4. Driver prompt

Run this with `/loop` (self-paced) to drive the plan to 100%. Each iteration: pick
the next unchecked item, implement, build (`cargo check -p awsm-renderer-editor` +
relevant crates), verify against the Phase-0 harness, update the checklist in this
file, and stop when all boxes are checked and the harness is green on every fixture.

```
/loop Drive docs/plans/save-load-roundtrip.md to 100%. Build the Phase-0 oracle
FIRST: (0.1) a no-browser `cargo test` that loads each fixture glb and asserts
extraction yields every mesh non-empty + every texture's encoded bytes; (0.2) an
in-editor `VerifyRoundtrip` command that clears ALL byte caches incl. mesh_cache and
asserts per-subsystem count + byte-equality. Reproduce the robot drift (a fixture
count below golden) as a RED test BEFORE fixing P0, prove GREEN after; never weaken a
test to pass. Then work top-down through §5; for each item implement + `cargo check`
the touched crates + run the oracle, then tick its §5 box with file refs and a
one-line proof.

Manage your own infra: the editor/MCP are NOT running. Start exactly ONE dev task in
the background — prefer `task mcp-dev` (superset of editor-dev: editor :9085 + media
:9077 + MCP :9086); NEVER run editor-dev and mcp-dev together (port collisions) and
probe :9085/:9077/:9086 first to reuse a running server instead of duplicating. Drive
the editor end-to-end via chrome-devtools `evaluate_script` →
`window.wasmBindings.editor_dispatch_json`/`editor_query_json`
(`{cmd:"import_model_from_file",name,url}` with a blob: URL); don't rely on a
human-kept editor or live MCP. Tear down the dev task when done.

NO player performance regressions: keep changes in the editor persistence path; if a
fix must touch shared renderer-gltf, gate it editor-only and say so. Stop and ask me
only if a fix needs a player-path change, a product decision (e.g. transport-state
persistence), or the oracle can't reproduce a reported drop. Fixtures: copy glbs into
the media dir served on :9077 (robot-001.glb at
/Users/dakom/Documents/LOCKSTEP/CDN/cas/ro/robot-001.glb); add
skinned/morph/sculpted/custom-material/nanite/KTX fixtures as needed.
```

(Alternatively `/goal Make project save→load lossless per docs/plans/save-load-roundtrip.md`.)

---

## 5. Checklist (drive to 100%)

**Phase 0 — harness**
- [x] 0.1 No-browser extraction oracle: `glb-export/tests/extraction_fidelity.rs` (set
  `SAVELOAD_FIXTURE=<glb>`). **GREEN on the robot: 38/38 meshes non-empty + 8/8 images
  with bytes.** → KEY FINDING: synchronous extraction is LOSSLESS; the robot's drift is
  NOT in `extract_node_mesh`/`extract_texture_images`. The robot's 8 images are all
  EMBEDDED (no external-URI), and `GltfLoader::load` (loader.rs:114) awaits full buffer
  load before extracting — so the loss is DOWNSTREAM in the editor's runtime cache
  population (mint → `mesh_cache` / `ensure_import_texture` → `texture_cache`), or
  environment-specific. **This re-scopes P0-B** (see below). Keep 0.1 as a regression
  guard. (P0-A's external-URI gap is a SEPARATE real bug the robot doesn't trigger.)
- [x] 0.2 In-editor census probe: `EditorQuery::SaveCensus` (query.rs) →
  `persistence::save_census` (factored out of `check_save_complete`), driven via
  `editor_query_json({"query":"save_census"})`. **Measured live: import robot →
  `mesh_assets:38 mesh_missing_cache:0 texture_assets:8 texture_missing_cache:0
  texture_unhashed:0`, stable from t=636 ms.** The editor import is LOSSLESS in
  headless. (Full clear-all-caches + apply + byte-equality `VerifyRoundtrip` command is
  still worth adding for regression CI, but the census already answers the P0-B question.)
- [x] 0.x robot drift re-localized to the **WRITE step** (David's correction: the loss is
  in the *saved files on disk*, with the cache complete → it's `save_to_dir` not all
  writes landing). Headless: cache census 38/8 complete + OPFS write pattern 47/47
  reliable → NOT the cache, NOT the generic write loop. Remaining suspect = the
  **picked-directory File System Access backend** silently dropping/truncating writes
  (can't automate the picker headlessly). **Hardened (shipped):** `fs.rs::write_bytes`
  now re-reads each file's size after `close()` and fails LOUD on mismatch; `save_to_dir`
  gathers all files up-front, logs a per-file breadcrumb + a final `save complete: wrote
  N/total` — so a truncation/abort is named + located, never silent. David's next save
  will show either a loud `write verify failed for <file> …` error (confirms picked-dir
  truncation) or `wrote 47/47` (→ look at cache-at-save via `SaveCensus`).

**Phase 1 — P0**
- [x] P0-A textures: added `extract_texture_images_with_external` (glb-export) and switched
  the editor (`bridge/gltf.rs`) to pass `data.encoded_images` → external-URI image bytes
  the loader re-fetched now get `content_hash` + persist. Editor-only; compiles. (Robot is
  embedded-only so unaffected; this fixes external-URI `.gltf` models.)
- [x] P0-B geometry: extraction proven lossless (0.1); editor cache complete (0.2 census
  38/8); real cause was the **interrupted async save** → fixed by the save-modal + write-verify
  (see ROOT CAUSE below). "mesh list is empty" was the missing meshes' empty re-bake on load.
- [~] P0-D robot dark patch: robot has NO authored tangents → not tangent-loss; `.mesh.bin`
  preserves vertex/index order so a cold reload regenerates identical tangents. Most likely
  the original side-by-side diff was lighting/orientation or the early missing-meshes era.
  Low priority; verify visually once if time permits.

**Phase 2 — P1**
- [x] P1-A environment KTX2/HDR bytes: `persistence::ktx_files`/`restore_ktx` (mirrors
  cluster) + `env_sync::ktx_bytes`; writes `assets/<id>.ktx2` for skybox/IBL `Ktx`
  assets, restored before `apply_project` at all 5 save/load sites. Compiles. HDR skybox/
  IBL now survives reload (was session-only).
- [x] P1-B custom-material WGSL reloads into the Studio — ALREADY WORKS (audit was
  wrong): `StoredMaterial.{wgsl,alpha_wgsl,vertex_wgsl,uniforms,textures,buffers,
  shader_includes,fragment_inputs}` are `#[serde(default)]`-serialized inline in
  `project.toml`; `material_from_stored` restores them into `custom_materials` and
  `apply_project` re-registers via `spawn_auto_register`. Stale `load_from_dir`
  "follow-on" comment fixed.
- [~] P1-C procedural textures roundtrip — DEFERRED (product decision). Procedural
  textures are GPU-generated with no encoded bytes + no capture recipe today, so there's
  no clean persistence without a new recipe/bake-on-save system. Niche; not a regression.
  Flag to David before building.

**Phase 3 — P2**
- [~] P2-A per-index masked morph blending — DEFERRED. This is a RUNTIME morph-mixing
  feature (multiple simultaneous morph tracks on one mesh stomping each other), NOT a
  save/load roundtrip bug — the morph DATA + weights roundtrip fine. Out of scope for
  this plan; track separately if desired.
- [x] P2-B material instance overrides roundtrip — COVERED by the existing
  `EditorProject` serde roundtrip tests (`material_roundtrip.rs`, 32 passing): the
  per-node inline `MaterialDef` + `texture/buffer/uniform_overrides` are plain serde
  fields in the node tree, so they round-trip through project.json + bitcode.
- [~] P2-C transport state (current clip/playhead/playing) + UI state (expanded/
  selection) reset on load is BY DESIGN (standard; `persistence.rs` resets them). Treated
  as accepted unless David says otherwise.
- [x] P2-D non-empty modifier-stack roundtrip test (`mesh_roundtrip.rs::mesh_modifier_stack_roundtrips`
  — Twist/Inflate/Array/Displace through JSON + bitcode; passes). Also added `tangents: None`
  to the 3 test `CapturedMesh` literals for the P0-C field.
- [x] P2-E stale `skinned_bake_cache.rs:15` TODO updated (bind-pose persistence IS
  implemented via `bind_pose_files`/`restore_bind_poses`).

**ROOT CAUSE of the robot drift — FOUND (David): interrupted async save**
- [x] The save is a fire-and-forget `spawn_local`; triggering anything else mid-write
  (or navigating/reload) cuts the write loop off at a variable point → silent partial
  project (the missing-meshes/textures bug). Explains everything: variable file counts,
  dies-before-textures, no error, cache complete, OPFS reliable.
- [x] **FIX (shipped):** `app.rs::save_project` now holds a `begin_activity("Saving…")`
  guard across the write, raising the existing full-screen **blocking** `busy_overlay`
  (swallows all input, no close, auto-clears on drop) so the save can't be interrupted.
  Paired with `write_bytes` per-file verify + `save_to_dir` "wrote N/total" log.

**P0-C — authored-tangent preservation through the STATIC/captured path — DONE**
- [x] Full chain shipped so a static imported mesh's authored glTF TANGENT survives
  save→reload instead of being regenerated: `ExtractedNodeMesh.tangents` (glb-export,
  filled in `extract_node_mesh`'s merge — all-or-nothing per node) → `bridge/gltf.rs`
  `NodeMeshMaps` → `mint_imported_mesh` → `CapturedMesh.tangents` (`#[serde(default)]`,
  in `.mesh.bin`) → `mesh_cache::get_raw` → `RawMeshData.tangents` → `GeometrySource`
  (renderer uses verbatim, else MikkTSpace). Skinned editor + **player** paths
  (`node_sync::raw_mesh_from_rig`, `scene-loader`) also use the rig-glb's authored
  tangents now (correct + skips MikkTSpace = faster; `None`⇒regen as before → no player
  regression). Workspace green; `glb-export` tests pass; oracle asserts
  `tangents_captured == authored_tangent_nodes`.
- [!] **BUT the robot has NO authored tangents** (oracle: `authored_tangent_nodes=0`) —
  so this fix is a no-op FOR THE ROBOT, and the robot's dark patch is NOT
  authored-tangent loss. The original "tangents by elimination" call was wrong (couldn't
  measure tangents over MCP). → **P0-D.**

**P0-D — robot dark patch: REAL BUG, FIXED (texture color-space on reload)**
- [x] Root cause (commit 5e01774f): `material::restore_raster_textures` hard-coded
  `srgb_to_linear: true` for EVERY reloaded texture (a known TODO in its own doc). DATA
  maps — **normal / metallic-roughness / occlusion** — must upload **LINEAR**; decoding the
  normal map through sRGB corrupts its normals → the round-tripped head shaded warmer/duller
  (the "dark patch"). The fresh-IMPORT path was correct (per-slot `linear` in
  `create_texture`); only RELOAD was wrong.
- [x] Diagnosis path (proves it's NOT geometry): cold-loaded the user's `clean-save-7`
  vs a fresh import — geometry is byte-identical (positions, normals, **UV set 0**, vertex
  order all match BY INDEX; 6247v/11016t), material params identical. The ONLY diff was the
  rendered **color/tone** (round-tripped warm/beige vs fresh cool/blue) ⇒ a texture-upload
  color-space difference, confirmed in code.
- [x] Fix v1 (band-aid, commit 5e01774f): a per-texture `linear` bool inferred from the
  display-name slot. Superseded by v2.
- [x] Fix v2 (PROPER, persists the semantic): `TextureDef::Raster` now carries
  `color_kind: Option<TextureColorKind>` (the slot role: Albedo/Normal/MetallicRoughness/
  Occlusion/Emissive/Specular/…) set at import from the glTF slot, persisted in
  `project.toml`, and mapped to the FULL `TextureColorInfo` (color space **+ mipmap kind**)
  by the single seam `material::color_info_for_kind` on reload — so RELOAD == IMPORT for
  every slot, not just sRGB. Old projects (`None`) fall back to display-name inference.
  Editor-only; player already correct via `scene-loader::texture`. (NB: `TextureColorKind`
  is a serde data-model enum separate from the renderer's GPU-internal `MipmapTextureKind`
  — scene is GPU-free + the save format must not encode shader-index discriminants; the two
  meet only at `color_info_for_kind`.)
- [lesson] My first pass wrongly concluded "not a bug / lighting" from an OVERLAP test —
  invalid because skinned meshes can't be hidden/separated via `set_visible`/root
  `set_transform`, so both rendered superimposed and looked identical. Always isolate each
  in its own session (same origin, same camera) — that exposed the warm-vs-cool diff.
- [note] Separate minor gap (not the patch): a cold-loaded **captured** mesh reads 0 verts
  via `get_mesh_data`/`get_vertex_data` though it renders — reloaded imports may not be
  editable until re-imported. Follow-up.

**Backstop (done)**
- [x] `check_save_complete` guard + per-save census log + per-file write-verify
- [x] authored-tangent preservation through rig-glb roundtrip (skinned path)
```
