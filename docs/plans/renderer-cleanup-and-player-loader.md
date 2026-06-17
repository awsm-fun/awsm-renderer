# Renderer cleanup (MSAA compile invariant) + player-grade scene loader

Two workstreams, run as one loop. **Part A** is David's demanded cleanup (a hard
compile invariant + dead-code removal). **Part B** is the player-grade scene-loader
API for third-party games (folds in `awsm-renderer-improvements.md` R1–R6, plus the
broader spirit: third-party games loading meshes / animations / materials / colliders
from our scene editor and populating for *game* usage — expose the public API that
serves that, documented well).

**Branch:** `material-increase` (no push). Byte-parity is the gate for any
GPU-affecting change (Part A); Part B is new API (parity N/A, but model-tests must
still render + tests stay green). Stage ONLY explicit renderer/scene-loader src paths.

GPU-verify method (Part A): model-tests :9080 (trunk watches the renderer crate;
`touch packages/crates/renderer/src/lib.rs`; tail `/tmp/mt-trunk.log` for
`Compiling awsm-renderer` + a new `✅ success`). chrome MCP → `/app/model/<Name>`,
sleep ~14s, screenshot, `python3` PIL pixel-diff excluding sidebar `x<215`
(`aa=np.asarray(a)[:,215:,:]`), require max-channel-diff 0. Anchors in
`experiments/_parity/baseline/`. Models: MetalRoughSpheres, SheenChair, MultiUv.

---

## David's hard invariant (the spine of Part A)

> In no case should we compile **any MSAA code with MSAA off**, nor **any non-MSAA
> code with MSAA on.** A module compiled for a given AA config must contain ONLY the
> entry points + bindings that config dispatches.

Concretely for the opaque material module: **non-MSAA → `cs_opaque` only;
MSAA → `cs_shade` only** (interior sample-0 → opaque_tex + edge per-sample →
accumulator, the unified-edge kernel). Never both in one compiled module; never build
a pipeline whose entry point the module (for that config) doesn't emit.

---

## Part A — renderer cleanup

### A0 [baseline] — capture current pipeline-count + parity anchors
Record the model-tests "compiling N pipelines" eager count (the prewarm pass set:
OpaqueEmpty, ClassifyMsaa, GeometryMsaa, Display, ScenePassClear [+HzbSeed]
[+EdgeResolveBlend]) and the per-material MSAA pipeline count, so A1/A2 wins are
measurable. Baseline anchors already exist.

### A1 [DONE] — delete the dead empty-opaque pipeline (the "5 pipelines" cruft)
Deleted `empty.wgsl` + `ShaderCacheKeyMaterialOpaqueEmpty` + `ShaderTemplateMaterialOpaqueEmpty` +
the `MaterialOpaqueEmpty` shared-enum variants + `OpaquePipelineSlot::{EmptyMsaa4,EmptySingle}` + the two
`*_empty_compute_pipeline_key` fields + `get_empty_compute_pipeline_key` + the empty-descriptor push +
`PassDef`/`PassKind::OpaqueEmpty` + the eager-set push + the `empty_opaque_shader_validates` test. Boot now
compiles 0 opaque pipelines (was 1 dead empty); the eager pass set is 5 → 4 (ClassifyMsaa, GeometryMsaa,
Display, ScenePassClear [+HzbSeed][+EdgeResolveBlend]). 259+34 green, warning-free. **GPU byte-parity
VERIFIED max-diff 0** (MetalRoughSpheres + SheenChair) + still renders (prewarm/readiness fine without it).
This is the stale "5th compiling pipeline" David flagged.

### A1 (orig) — delete the dead empty-opaque pipeline (the "5 pipelines" cruft)
`get_empty_compute_pipeline_key` has **zero callers** repo-wide → the empty-opaque
pipeline is compiled (eager `OpaqueEmpty` pass) but never dispatched. Classify routes
uncovered/sky pixels to the **SKYBOX bucket** (dispatched in the bucket loop via
`main[SKYBOX]`=skybox_primary / its cs_shade arm), not this pipeline. Remove it whole:
- `empty.wgsl` + `ShaderCacheKeyMaterialOpaqueEmpty` (cache_key) + its template.
- `OpaquePipelineSlot::{EmptyMsaa4,EmptySingle}`, the two `*_empty_compute_pipeline_key`
  fields, `get_empty_compute_pipeline_key`, and the empty descriptor pushed in
  `shader_descriptors_for_config_with`.
- `PassDef::OpaqueEmpty` + `PassKind::OpaqueEmpty` + its scheduler/launch install +
  the eager-set push in `renderer.rs`.
Pure dead-code (never dispatched) → GPU byte-parity is automatic; the win is the eager
count **5 → 4** + one fewer compiled pipeline per AA flip. Verify the count dropped +
byte-parity. (Investigate first that nothing — prewarm readiness gating, tests — needs
it; delegate the mechanical multi-file deletion, then GPU-verify.)

### A2 — split cs_opaque / cs_shade by MSAA (the invariant)
Today the opaque module emits `cs_opaque` unconditionally + `cs_shade` under
`{% if multisampled_geometry %}`. Under MSAA `cs_opaque` is built (lazy `main` map +
the eager set) but dispatched only at no-MSAA (render.rs: `msaa ? render_shade :
render`). Fix to the invariant:
- compute.wgsl / skybox_primary.wgsl: gate the `cs_opaque` entry to
  `{% if !multisampled_geometry %}` (cs_shade already `{% if multisampled_geometry %}`).
  Net: non-MSAA module = cs_opaque; MSAA module = cs_shade. Exactly one.
- pipeline.rs (`main` cs_opaque build): build the `main`/`cs_opaque` pipeline **only for
  non-MSAA** configs (the lazy `ensure_scene_pipelines` + AA-recompile + launch path).
  Under MSAA the bucket's pipeline is the cs_shade one built by `edge_pipeline.rs`
  (already MSAA-only). So `get_compute_pipeline_key` returns None under MSAA (render()
  isn't called there) and `get_shade_pipeline_key` returns the kernel.
- Verify nothing builds a `.with_entry_point("cs_opaque")` pipeline under MSAA (would
  fail pipeline-create with the entry gone). The launch.rs opaque-pipeline build
  (`with_entry_point("cs_opaque")`) must be non-MSAA-only; the MSAA bucket build is the
  cs_shade path.
- naga both AA configs; GPU byte-parity (prep on/off, all models); confirm per-material
  MSAA now compiles 1 opaque kernel (cs_shade), not 2. Fragile — investigate the build
  paths first, delegate the mechanical edit with this invariant as the spec, GPU-verify.
- size_regression: with cs_opaque gone from the MSAA module, the MSAA module shrinks
  (reverses the U3a ceiling raise) — re-measure + tighten ceilings to the real values.

### A3 — remove remaining dead helpers
After A1/A2: drop the now-dead `pub` offset fns left from unified-edge U3b
(`data_per_shader_count_offset`, `data_skybox_count_offset`, `sample_entries_offset`,
`skybox_sample_list_offset`, `sample_entries_per_bucket`, `skybox_edge_args_offset`,
`per_shader_args_offset`) + their tests, and shrink `data_header_bytes` to just the
counter mirrors (the per-bucket count region is dead) — recompute `edge_to_xy_offset`
consistently across the Rust builder + the (now 5-field) `EdgeBufferLayout` uniform so
offsets stay consistent; GPU byte-parity. (Low value; do only if clean.)

---

## Part B — player-grade scene loader (`packages/crates/scene-loader/`)

Source of truth: `awsm-renderer-improvements.md` (R1–R6). Implement the requirements;
the API *shapes* there are a guide — match awsm-renderer's internals, lockstep adapts.
**Spirit:** a third-party game loads a baked editor `Scene` once, then drives it every
frame (move/hide/read named nodes, spawn prefab instances per entity). The loader owns
"how to render each NodeKind"; the game owns gameplay semantics. Everything the game
needs must be **public + documented** (rustdoc with usage examples; the crate's lib.rs
should orient a newcomer).

Keep `populate_awsm_scene` working (model-test page uses it) — layer it on the new path
or leave alongside. Each stage: tests green + (where it renders) model-tests renders.

### B0 — survey + design
Read `scene-loader/src/lib.rs` (`populate_awsm_scene`, `AnimResolveMaps`,
`materialize`, `load_glb_under`, `mesh_data_to_raw`) + `scene/src/tree.rs`
(`NodeKind`, `EditorNode`, `RuntimeMesh`, `Trs`, `Curve`). Confirm the renderer
primitives each NodeKind needs (sprites/particles/lines/decals/instancing — check what
`renderer` already exposes: lines pass, decals feature, instancing). Write the concrete
public API (types + signatures) into this doc before coding. Decide: does the renderer
expose enough for Sprite/ParticleEmitter/Line/Decal, or is a small renderer-side public
API needed too (R3 says the renderer is the right owner of "how to render X")?

### B1 — R1: per-`NodeId` handles
`NodeHandles { transform, meshes, light, camera }` + `LoadedScene { nodes:
HashMap<NodeId,NodeHandles>, prefabs, clips, + teardown handles }`. Stop discarding the
internal node→key map. Acceptance: `loaded.nodes[&id].transform` is a live key.

### B2 — R5: `SceneAssets` async trait
`trait SceneAssets { async fn fetch(&self, path:&str) -> Result<Vec<u8>> }` + blanket
impl for `HashMap<String,Vec<u8>>` (model-test path unchanged). Loader pulls lazily;
`on_phase` still reports progress.

### B3 — R3: full NodeKind coverage
Add Sprite, ParticleEmitter (CPU; opaque + transparent-blend), Line (fat polyline),
InstancesAlongCurve (place a source node along a `Curve`); Curve = data only; Collider
skipped; Decal if feasible else documented-unsupported + clean skip. If the renderer
lacks a primitive, expose+document the minimal public renderer API for it. Each kind
renders through the loader with no consumer per-kind code.

### B4 — R2: prefab templates + instancing
`prefab==true` subtrees → `PrefabTemplate` (materialized once, hidden);
`PrefabTemplate::instantiate(renderer, world_trs) -> PrefabInstance { root, nodes }`,
cheap (duplicate mesh handles / shared GPU buffers — the `duplicate_mesh_with_transform`
refcount pattern; no re-parse/re-upload). Twice → two independent instances sharing
geometry.

### B5 — R6: visibility + teardown
Honor `EditorNode.visible` at load; one documented call to toggle a materialized node's
visibility via its `NodeHandles`; `LoadedScene` teardown unloads
meshes/lights/clips/instances with no leak across reload.

### B6 — R4 + entry point + docs
`load_scene_for_player(renderer, scene, assets: &impl SceneAssets, on_phase) ->
LoadedScene`; make `materialize_node_mesh` / `load_glb_under` / `mesh_data_to_raw`
public (R4 hedge). `populate_awsm_scene` becomes a thin wrapper. Full rustdoc + a
crate-level "loading an editor scene in your game" example. Re-check the Consumer
contract in `awsm-renderer-improvements.md` matches.

---

## Rules
After every commit: `cargo test -p awsm-renderer -p awsm-materials -p awsm-scene-loader
--lib` green (add scene-loader). naga = compile-only; GPU byte-parity is the real gate
for Part A. ONE coherent increment per commit; mark stages [DONE] here. Do NOT start the
uber-shader. Delegate large mechanical edits to fresh agents (no git in agent;
investigate first; parent commits explicit paths + GPU-verifies). If a Part A stage
can't reach byte-parity, STOP + report the exact diff. Public API must be documented.
