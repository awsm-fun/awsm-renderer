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

### A2 [DONE] — split cs_opaque / cs_shade by MSAA (the invariant)
Gated `cs_opaque` to `{% if !multisampled_geometry %}` in compute.wgsl + skybox_primary.wgsl (cs_shade
already `{% if multisampled_geometry %}`), so a compiled opaque module carries EXACTLY one kernel:
non-MSAA → cs_opaque; MSAA → cs_shade. Build side: gated the per-bucket opaque build (launch.rs
`build_opaque`) + the first-party `main` descriptor loop (pipeline.rs `shader_descriptors_for_config_with`)
to `active_msaa.is_none()` — under MSAA nothing builds a `.with_entry_point("cs_opaque")` pipeline (would
fail create now); cs_shade is built by the edge path (`launch_edge_resolve_compile`, already MSAA-only).
naga tests flipped to assert the invariant (MSAA module has cs_shade & NOT cs_opaque; non-MSAA the reverse).
261+34+25 green. **GPU-VERIFIED:** MSAA SheenChair + MetalRoughSpheres == baseline (max-diff 0), only a
benign first-frame final_blend warmup warn, NO cs_opaque pipeline-create error; no-MSAA renders correctly
via cs_opaque (97% identical to the MSAA capture, differences only at antialiased edges). **MSAA opaque
module shrank ~90.7 KB → 82.0 KB empty / ~126.7 KB → 118.0 KB all** (cs_opaque dropped) — size_regression
ceilings re-tightened (94K→84K, 132K→120K).

### A2 (orig) — split cs_opaque / cs_shade by MSAA (the invariant)
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

### A3 [DONE] — remove remaining dead helpers
Removed the dead-since-U3b `pub fn` offset helpers (`data_per_shader_count_offset`,
`data_skybox_count_offset`, `sample_entries_offset`, `sample_entries_per_bucket`,
`skybox_sample_list_offset`, `skybox_edge_args_offset`, `per_shader_args_offset`) + their now-unused
`SAMPLE_*` consts + 2 obsolete layout tests + the stale `sample_entries_per_bucket` field in the alloc-log
line — all in edge_buffers.rs (zero live callers; only the removed sample-list feature used them). Pure
Rust dead code: NO WGSL / pipeline / buffer-layout change (`data_header_bytes` kept intact), so GPU output
is identical by construction. 259+34+25 green, warning-free. **Part A2-part-2 (shrink `data_header_bytes`
to drop the dead per-bucket count region) DEFERRED** — it shifts edge_to_xy/slot_map/accumulator offsets
(GPU-affecting) for a ~B*4-byte win; not worth the risk. **Part A COMPLETE.**

### A3 (orig) — remove remaining dead helpers
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

### B0 [DONE] — survey + design (the B-stage contract)

**Survey findings (file refs in commit msg):**
- `LoadedScene` today = `{ meshes: Vec<MeshKey>, lights: Vec<LightKey>, clips: Vec<AnimationClipKey> }`
  (flat). The node→key maps ALREADY EXIST in the private `AnimResolveMaps` (animation.rs): `transforms`,
  `lights`, `cameras`, `meshes`, `skin_joints`, `node_materials` (all `HashMap<NodeId, *Key>`) — built
  across phases, dropped on return. R1 = stop dropping it.
- `materialize` (lib.rs) handles Mesh / SkinnedMesh / Light / Camera (+ Group=transform); `_ => {}` for
  Line / Decal / Sprite / ParticleEmitter / InstancesAlongCurve / Curve / Collider. It DOES recurse
  `children` but does NOT read `node.prefab` or `node.visible`.
- Renderer public APIs that EXIST (so the loader just needs to call them):
  - Lines: `renderer.add_line_strip(&[Vec3], &[Vec4], width, depth_test_always) -> Result<Option<LineKey>>`
    (+ `add_line_segments`, `remove_line`, `ensure_line_pipelines_compiled`).
  - Decals: `renderer.insert_decal(transform: Mat4, texture_index: u32, alpha: f32) -> Result<DecalKey, AwsmDecalError>`
    — errors `FeatureNotEnabled` when the `decals` feature is off.
  - Instancing: `meshes.duplicate_mesh_with_transform(MeshKey, TransformKey) -> Result<MeshKey>` (cheap,
    shares geometry+material buffers) and `meshes.enable_mesh_instancing_opaque(MeshKey, &[Transform])`.
  - Transforms: `transforms.insert(Transform, parent) -> TransformKey`, `set_local`, `set_parent`, `remove`.
  - Visibility: `meshes.set_mesh_hidden(MeshKey, bool)` (the only runtime visibility lever).
  - Teardown: `meshes.remove_mesh(MeshKey)`, `lights.remove_light(LightKey)`,
    `animations.remove_clip(AnimationClipKey)`, `transforms.remove(TransformKey)`.
- NO renderer support (gaps): **Sprite** — no sprite primitive; must build a quad mesh + material
  (FlipBook if `flipbook` else Unlit, with `tint`) + billboard (mesh has a `billboard_mode` field; runtime
  setter not found → see B3). **ParticleEmitter** — no particle pass at all; needs a per-frame CPU sim
  (gameplay, not loader) → documented clean-skip + flagged as a separate renderer effort.

**Public API contract (Part B builds to this; lockstep adapts):**
```rust
// scene-loader public surface
pub struct NodeHandles {
    pub transform: TransformKey,
    pub meshes: Vec<MeshKey>,             // empty for non-mesh nodes
    pub light: Option<LightKey>,
    pub camera: Option<CameraKey>,        // the renderer camera, if a Camera node
    pub camera_config: Option<CameraConfig>, // authored config, for the consumer's camera rig (R1)
    pub line: Option<LineKey>,            // Line nodes
    pub decal: Option<DecalKey>,          // Decal nodes (when the decals feature is on)
}
pub struct LoadedScene {
    pub nodes: HashMap<NodeId, NodeHandles>,        // static (non-prefab) world (R1)
    pub prefabs: HashMap<NodeId, PrefabTemplate>,   // prefab roots (R2)
    pub clips: Vec<AnimationClipKey>,
    pub meshes: Vec<MeshKey>,  pub lights: Vec<LightKey>, // kept flat for back-compat + teardown
    // (+ lines/decals/transforms collected internally for teardown)
}
impl LoadedScene { pub fn teardown(self, renderer: &mut AwsmRenderer); } // R6 (no leak on reload)

pub struct PrefabTemplate { /* opaque: hidden materialized subtree + per-node metadata */ }
pub struct PrefabInstance { pub root: TransformKey, pub nodes: HashMap<NodeId, NodeHandles> }
impl PrefabTemplate {
    pub fn instantiate(&self, renderer: &mut AwsmRenderer, world_trs: Trs) -> Result<PrefabInstance>; // R2
}

pub trait SceneAssets { async fn fetch(&self, bundle_relative_path: &str) -> Result<Vec<u8>>; } // R5
impl SceneAssets for std::collections::HashMap<String, Vec<u8>> { /* blanket — model-test path */ }
// static dispatch (`&impl SceneAssets`) — avoids dyn-async; native async-fn-in-trait (Rust 1.75+).

pub async fn load_scene_for_player(                                   // R6 entry point
    renderer: &mut AwsmRenderer, scene: &Scene, assets: &impl SceneAssets, on_phase: impl FnMut(LoadPhase),
) -> Result<LoadedScene>;
pub async fn populate_awsm_scene(/* unchanged sig */) -> Result<LoadedScene>; // thin wrapper over the above

// R4 — public mesh materialization (hedge)
pub async fn materialize_node_mesh(renderer, scene, node: &EditorNode, assets: &impl SceneAssets,
    material: MaterialKey) -> Result<Vec<MeshKey>>;
pub async fn load_glb_under(/* now pub */) -> ...;  pub fn mesh_data_to_raw(/* now pub */) -> ...;
```

**Per-NodeKind plan (B3):** Line → `add_line_strip`/`add_line_segments` (→ `NodeHandles.line`). Decal →
`insert_decal` (texture via `texture::load_texture`; node transform → `Mat4`; if `decals` feature OFF,
clean-skip + one-time warn). InstancesAlongCurve → eval the referenced `Curve(CurveDef)` (Catmull-Rom
sample) → per-instance `Transform`s (spacing/side_offset/orient_to_tangent) → `enable_mesh_instancing_opaque`
on the `source_node` mesh. Sprite → quad mesh (meshgen) + Unlit/FlipBook material + `tint`; billboard via a
new tiny renderer setter if one's missing + cheap, else world-aligned quad + documented billboard caveat.
Curve = data-only (consumed by Sprite-flipbook/Instances/camera). Collider = skip. **ParticleEmitter =
documented clean-skip** (no renderer particle pass; CPU per-frame sim is gameplay — note loudly, recommend
a future dedicated renderer particle pass; the game can drive its own via the mesh/line APIs meanwhile).

**Renderer-side additions anticipated:** (a) maybe a runtime `meshes.set_billboard_mode(MeshKey, mode)` for
sprites (only if not already settable at insert). Keep renderer additions minimal + documented.

### B0 (orig) — survey + design
Read `scene-loader/src/lib.rs` (`populate_awsm_scene`, `AnimResolveMaps`,
`materialize`, `load_glb_under`, `mesh_data_to_raw`) + `scene/src/tree.rs`
(`NodeKind`, `EditorNode`, `RuntimeMesh`, `Trs`, `Curve`). Confirm the renderer
primitives each NodeKind needs (sprites/particles/lines/decals/instancing — check what
`renderer` already exposes: lines pass, decals feature, instancing). Write the concrete
public API (types + signatures) into this doc before coding. Decide: does the renderer
expose enough for Sprite/ParticleEmitter/Line/Decal, or is a small renderer-side public
API needed too (R3 says the renderer is the right owner of "how to render X")?

### B1 [DONE] — R1: per-`NodeId` handles
Added `pub struct NodeHandles { transform, meshes, light, camera, camera_config, line, decal }` + extended
`LoadedScene` with `nodes: HashMap<NodeId, NodeHandles>` + `prefabs: HashMap<NodeId, PrefabTemplate>`
(flat `meshes`/`lights`/`clips` kept for back-compat — no literal construction anywhere, so additive). Stopped
discarding the per-node maps: extended `AnimResolveMaps` with `node_meshes: HashMap<NodeId, Vec<MeshKey>>`
(all keys/node; `meshes` stays first-key-only for the single-target animation path) + `camera_configs:
HashMap<NodeId, CameraConfig>`; `materialize` records both; after the materialize loop, `populate_awsm_scene`
assembles `loaded.nodes` from the maps. `PrefabTemplate` introduced as a placeholder (B4 fills instancing).
259+34+25 green, warning-free. Acceptance met: `loaded.nodes[&id].transform` is a live `TransformKey`.
(Note: SheenChair etc. load via `populate_gltf`, not `populate_awsm_scene`, so this additive change doesn't
touch the model-view render path; scene-loader tests are the gate.)

### B1 (orig) — R1: per-`NodeId` handles
`NodeHandles { transform, meshes, light, camera }` + `LoadedScene { nodes:
HashMap<NodeId,NodeHandles>, prefabs, clips, + teardown handles }`. Stop discarding the
internal node→key map. Acceptance: `loaded.nodes[&id].transform` is a live key.

### B2 [DONE] — R5: `SceneAssets` async trait
Added `pub trait SceneAssets { async fn fetch(&self, bundle_relative_path: &str) -> Result<Vec<u8>> }`
(new `assets.rs` module, re-exported from lib) + blanket `impl SceneAssets for HashMap<String, Vec<u8>>`
(fetch = clone or "asset not found" err). Threaded `&impl SceneAssets` (static dispatch) through
materialize / load_glb_under / resolve_material / texture::load_texture / dynamic::build_custom_material,
and made `dynamic::register_custom_materials` async. Byte-fetch sites now `.fetch(path).await` with the same
missing-asset skip/None semantics. `populate_awsm_scene` keeps its public `&HashMap<String, Vec<u8>>` sig
(passes it where `&impl SceneAssets` is expected via the blanket impl) — model-test page unchanged. The
scene asset *registry* (`scene.assets.get`) is untouched (it's metadata, not bytes). `#[allow(async_fn_in_trait)]`
on the trait (static-dispatch wasm; no Send). 259+34+25 green, warning-free.

### B2 (orig) — R5: `SceneAssets` async trait
`trait SceneAssets { async fn fetch(&self, path:&str) -> Result<Vec<u8>> }` + blanket
impl for `HashMap<String,Vec<u8>>` (model-test path unchanged). Loader pulls lazily;
`on_phase` still reports progress.

### B3 [DONE] — R3: full NodeKind coverage
Implemented the remaining `materialize` arms (wired into `NodeHandles`; world matrices composed by hand via
a threaded `parent_world: Mat4` since `get_world` isn't folded mid-load):
- **Line** → `add_line_strip` (LinePoint pos/color, world-baked) + `ensure_line_pipelines_compiled`; `NodeHandles.line`.
- **Sprite** → `awsm_meshgen::sprite_quad` scaled by `size`; Unlit (tint+texture) or FlipBook (atlas/grid/fps)
  when `flipbook` Some; billboard via the EXISTING `AwsmRenderer::set_mesh_billboard_mode` (no new renderer API).
- **Decal** → resolve `cfg.texture` to a flat pool index (`array*64+layer`, matching the decal shader packing) +
  node world Mat4 → `insert_decal`; `FeatureNotEnabled` → one-time warn + skip; `NodeHandles.decal`.
- **InstancesAlongCurve** → look up `curve_node`'s `CurveDef`, Catmull-Rom sample (via `awsm-curves`, respects
  closed/tension/sample_count), arc-length place every `spacing` (+ side_offset/orient_to_tangent), resolve
  `source_node` first mesh, `enable_mesh_instancing_opaque`.
- **Curve / Group / Collider** → no renderable (no-op). **ParticleEmitter** → documented clean-skip + one-time
  warn (no renderer particle pass; particles are gameplay-owned).
Added `awsm-curves` dep; extended `AnimResolveMaps` with `lines`/`decals`; a Phase-3a `finalize_gpu_textures`
re-commit covers sprite/decal textures staged after Phase 2. `populate_awsm_scene` public sig unchanged;
259+34+25 green, warning-free, clippy clean. **Documented caveats / follow-ons:** decal texture index assumes
≤64 pool layers/array (`DECAL_POOL_LAYERS_PER_ARRAY`, matches shader — confirm pool never exceeds); InstancesAlongCurve
does not yet apply `per_instance_colors` or per-instance `shadow` (opaque-instancing takes transforms only) and
relies on DFS order (source before instances, best-effort + warn); ParticleEmitter unrendered (future renderer
particle-pass effort).

### B3 (orig) — R3: full NodeKind coverage
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
