# examples/test-scenes — the permanent verification suite

Every renderer feature and every optimization axis is verified against this
suite. Each scene is three artifacts:

- `<scene>/project/` — an editor **project** (`project.toml` + `assets/` side
  files), loadable via the editor's Load / `load_project_from_url`.
- `<scene>/bundle/` — the baked **player bundle** (`scene.toml` + `assets/`),
  loadable by the player runtime and by `load_player_bundle`. These are the
  inputs for `examples/player-tests/` (plan 007).
- `<scene>/golden.png` — the golden screenshot (clean-capture workflow: grid,
  gizmos, overlays OFF; camera pinned by the scene's authoring script).

Scenes are AUTHORED under the reverse-Z depth convention (default since plan
003) and act as its permanent regression lock.

## Regenerating

Scenes are authored headlessly through the MCP editor link (`task mcp-dev`,
editor on :9085 attached to the dev MCP on :9186 via `?mcp=`): each scene's
`author.js` replays deterministic editor commands (fixed UUIDs, pinned camera,
grid/gizmos/light-gizmos off), then three MCP tools write the artifacts:
`save_project` (→ `<scene>/project/`), `export_player_bundle`
(→ `<scene>/bundle/`), `screenshot_scene` (→ `<scene>/golden.png`).
To regenerate after an intentional visual change: re-run the scene's authoring
script and commit the diff — goldens change ONLY with an explanation in the
commit message.

Goldens are **visual references**, not byte-exact CI locks: the capture
follows the live viewport's aspect, which the OS window constrains. Compare
them visually / structurally when verifying a change; the programmatic
regression asserts live in `examples/player-tests/` (plan 007) over the baked
bundles, which are window-independent.

## The scenes

| Scene | Features under test | What "correct" looks like |
|---|---|---|
| `anim-skinned` | skinned mesh playback, rig roundtrip | skinned model mid-stride at t=0.5, no T-pose, no candy-wrapper collapse |
| `anim-morph` | morph targets, multi-track per-index blending (005 §3) | two morph indices driven independently by two tracks |
| `anim-blend` | animation blends / mixer layers, masks, transport | blended pose distinct from either source clip |
| `shadows-all` | directional cascades + spot + point/cube, denoise blur, world-ref bias | contact-tight shadows, no Peter-Pan gap, no donut/hole under lowered meshes |
| `alpha-cutoff` | masked materials, cutoff values, double-sided | hard-edged cutouts, back faces visible where double-sided |
| `transparent` | transparent pass ordering over opaque | correct through-glass layering, no popping |
| `prefab-static` | prefab duplication of static meshes | N clones, geometry uploaded ONCE (census-verified) |
| `prefab-skinned-morph` | prefab duplication with skins + morphs | independent animation per clone, shared geometry buffers |
| `dynamic-materials` | custom WGSL materials, live uniform edits, instance overrides | per-instance override visibly diverges from shared default |
| `builtin-overrides` | per-node built-in PBR param overrides | same material asset, visibly different tunings per node |
| `pbr-extensions` | transmission, diffuse transmission, clearcoat, sheen, iridescence, dispersion, anisotropy, volume, specular, ior, emissive_strength | one probe object per extension, each visually distinct from plain PBR |
| `env-ibl` | 3-slot environment (skybox/specular/irradiance), KTX2, built-in default | slots independently swapped; reflections track the specular slot |
| `ssr` | SSR on glossy floor (black glossy dielectric probe), half-res + MSAA edges | continuous reflections of emissive columns, clean silhouettes |
| `mirror` | perfect-mirror SSR (spread 0: spatially deterministic trace + 16-frame temporal supersampling, tight resolve AA kernel, full-res, bloom off; touching-sphere contact probe) | reflection pixel-identical in shape to geometry, no serration/noise/dashes |
| `bloom-post` | bloom knobs, tonemappers (aces vs khronos_neutral_pbr), exposure, DoF | halo scales with intensity; tonemapper switch visibly re-grades |
| `lights-many` | froxel culling under many point/spot lights | dozens of local lights, correct falloff, interactive frame rate |
| `particles` | particle emitter (existing instancing path) | emitter animates; instance colors apply |
| `decals` | decal projection | decal lands on geometry only, no skybox bleed |
| `lod-classic` | discrete LOD chain switching (incl. skinned) | visible simplification at far orbit, none at near |
| `lod-nanite` | cluster DAG cut, streaming budget, 2+ nanite meshes | watertight cut at every radius, stable under `?stream`/`?streambudget=N` |
| `lod-nanite-open` | cluster cut on a GENUINELY OPEN mesh (outer rim + 2 punched holes; A2 input class) | exactly the two authored holes at every radius/budget — any extra gap = a torn cut (fixture: `gen-open-sheet.py`, deterministic) |
| `instancing-stress` | N×1000s instanced meshes (axis-5 instancer NodeKind) | thousands of instances, ONE geometry upload, interactive frame rate |
| `kitchen-sink` | everything at once | the smoke test; also the startup-census scene |

## The optimization axes (plan 006)

The sweep this suite exists to measure. Every axis lands with before/after
numbers recorded here and in `docs/plans/006-optimizations.md`.

1. **Build only what we need** — a renderer instantiation compiles ONLY the
   pipelines/shaders/textures its scene requires; startup census (pipelines,
   shaders, textures at init + after first frame) recorded per scene; empty
   scene is the floor; nothing compiles speculatively.
2. **Concurrency at commit time** — transaction commit fans out async pipeline
   creation and concurrent texture decode/upload; editor consolidates
   per-node commits into one begin→declare-all→commit per user operation.
   Scoreboard: cold `kitchen-sink` bundle load, trace shows overlap.
3. **Compression** — lossless WebP is the bundle default for every texture
   class (data maps byte-exact; lossy never silently applies to them);
   project side-files evaluated for WebP; meshopt/quantization evaluated.
   Scoreboard: bundle bytes per scene, goldens pixel-identical.
4. **Prefabs: clone never clones data** — geometry/skin/morph data shared
   across clones; per-instance divergence = transforms, uniforms, animation
   state. `duplicate_skinned_with_new_skin`'s re-upload is the known offender.
   Scoreboard: census on `prefab-*` scenes grows by per-instance data only.
5. **Instancing as authoring** — explicit instancer NodeKind (mesh source +
   N owned instance transforms; 100k instances ≠ 100k nodes), MCP + UI +
   persistence + scene-loader. Scoreboard: `instancing-stress` census + fps.
6. **LOD robustness** — classic chains + nanite DAG cut/streaming verified
   across radii and budgets from a cold checkout; dynamic paging stays
   design-only unless the scenes prove the need.
7. **Shading code and math** — WGSL audit (redundant work, prep-liftable
   per-fragment ops, divergence, fetch counts, half-precision). Scoreboard:
   `?trace=sub-frame` per-pass table on `kitchen-sink` + stress scenes;
   goldens unchanged.
8. **Rust/wasm allocations** — zero per-frame heap allocs in steady state
   (pool/hoist/reuse; the standard applies even without a measured delta);
   known offenders: `sync_bones_to_skin` HashSet+Vec per frame. Scoreboard:
   traced steady-state frame allocation count + stress frame times.

## Scene status

All 23 scenes are authored and versioned (`mirror`, 2026-07-12, is the
perfect-mirror SSR acceptance scene). `lod-nanite-open` (2026-07-11)
locks the open-boundary cluster-cut class on-device; its source mesh is
generated, not sampled (`gen-open-sheet.py`, deterministic — regenerate
instead of editing the .glb). `instancing-stress` landed with
axis 5 (the explicit instancer NodeKind): 3000 per-instance-colored boxes
from ONE instancer node and ONE shared geometry at vsync.
`prefab-skinned-morph` renders three shared-geometry walkers and additionally
carries a hidden skinned prefab template (prefab=true + visible=false 4th
duplicate) for player-tests' `prefab-churn-skinned` joint-lifecycle check.
`lod-nanite`'s bake recipe (export-pipeline bake, no standalone CLI) is in
its author.js.

## Baselines

Recorded as axes land (scene → census / frame time / bundle bytes / load ms).
Census source: `memory_stats` query on a cold page reload (editor :9085,
2026-07-10, pre-axis-1).

| Scene | Render pipelines | Compute pipelines | Shaders | Pool tex | render_cpu ms |
|---|---|---|---|---|---|
| _empty_ (cold, PRE axis 1) | 68 | 31 | 49 | 2 | 1.5 |
| _empty_ (cold, POST axis 1) | 68 | **22** | **40** | 2 | 1.5 |
| kitchen-sink (pre) | 69 | 32 | 51 | 4 | 1.8 |

**Axis-1 result:** bloom (3), SSR (2), decal + classify (3) and cluster-LOD
(2) compute pipelines + 9 shader modules no longer compile on scenes that
don't use them — they land lazily on first enable (bloom/SSR), first decal
insert (render-loop kick/poll, same-frame bind), or first cluster-mesh
commit. AA flips on SSR-less sessions no longer recompile SSR. hzb /
occlusion / coverage stay eager by design (the optimization policy flips
GPU culling at runtime; lazy would hitch mid-session).
