# MuJoCo Rendering Support

> **Temporary planning doc.** Deleted once shipped; the permanent reference is
> `docs/mujoco.md`, written as the final phase (same lifecycle as the LOD
> plan → `docs/nanite-lod.md`).

Render MuJoCo simulations with full visual fidelity — the render should match what a
camera pointed at the real robot (e.g. a Unitree) would see, to the limit of the sim
itself. The renderer never runs physics: it ingests a compiled model once and applies
externally-produced pose state.

## Decisions (settled)

- **Mode**: external sim only ("mode b"). The sim runs elsewhere (native process, RL
  harness, robot telemetry); the browser renders. In-browser wasm sim is a possible
  future *producer* of the same inputs, not part of this plan.
- **No MJCF parsing by us, ever.** MuJoCo's own C code compiles MJCF → mjModel
  (defaults inheritance, procedural textures, mesh fitting). We only ever read the
  compiled model, via `libmujoco` linked into a native Rust CLI.
- **No Python dependency.** Exporter and reference streamer are Rust binaries linking
  `libmujoco` (bindgen against the tiny surface we need: `mj_loadXML`/`mj_loadModel`/
  `mj_deleteModel` + mjModel field reads; streamer adds `mj_step` + mjData reads).
  Prebuilt MuJoCo release dylibs. MJCF is the preferred input; URDF is accepted for
  free (libmujoco loads it natively into the same compiled mjModel, optionally
  augmented by an embedded `<mujoco>` block); `.mjb` accepted but secondary
  (version-locked).
- **Delivery = our editor/bundle pipeline, not glTF extras.** GLB carries geometry
  only (as our bundles already do); MuJoCo semantics live as a first-class component
  in our scene formats. The editor becomes a full MuJoCo tool: import, re-material,
  light, compose, save/load, bundle.
- **"Trajectory" is not a concept in our system — only a bake tool.** A recorded
  sim run (stream frames on disk) is transient interchange consumed by a converter
  that bakes it into standard animation clips (T+R track per bound node,
  keyframe-reduced). After the bake it's ordinary clips — composable, scrubbable,
  bundle-playable. No trajectory asset type, no trajectory UI; the capture file is
  disposable, like a source `.obj` after import.
- **Transforms of sim-bound geom nodes are stream-owned and locked** in the editor
  (gizmo disabled, "driven by sim" indicator). The **sim instance root is
  user-placeable** — its authored transform is the model's initial placement (attach
  frame) in the composed world. Users author placement at the instance level plus
  materials/lighting/surroundings; never individual geom poses.
- **No sim harness code in this repo.** The renderer ships the exporter, the editor
  support, and a mapping layer in scene-loader (binding + pose sink). Running
  MuJoCo, composing the world, and transport are the player's job — demonstrated by
  a `physics-mujoco` reference template in the templates repo, exactly like the
  existing `physics-*` templates.
- **Model visual features are all in scope** (geoms of every type, sites, spatial
  tendons, skins): fidelity is non-negotiable, including in player bundles. **Debug
  overlays** (contacts, force arrows, joint axes, inertia boxes) are **dev-tooling
  only** — they live in the reference template (the only place with live sim data),
  never in the editor and never in the bundle format.

## Architecture

```
MJCF/mjb ─▶ mujoco-export-cli (Rust + libmujoco)
              ├─ model.glb          geometry only (glb-export crate)
              └─ mujoco.json        sidecar: geoms, materials-as-data, groups,
                                    sites, tendon defs, skin bindings
                        │
                        ▼
              editor "Import MuJoCo" command (Rust, real serde types)
              ├─ mesh nodes + mujoco component {geom_id, group, geom_type}
              ├─ materials minted in our material system (Phong→PBR mapping)
              └─ scene-level component: sim instance list (schema supports N,
                 v1 implements 1) — stream config, geom_id→node_id map,
                 tendon/site/skin tables
                        │
        ┌───────────────┴───────────────┐
        ▼                               ▼
  recorded capture ─▶ bake to      third-party player runs MuJoCo
  animation clips (existing        ─▶ pose sink (scene-loader mapping layer)
  anim system: scrub/play/save)    ─▶ node transforms (world-space, via
                                      geom_id→node_id binding)
```

### Sidecar + capture formats (the stable seams)

Two dumb, documented, frozen-schema formats let third-party pipelines (Python RL
shops, CI) produce valid input without our workspace. Our Rust tools are just the
first producers.

- `mujoco.json`: flat description of the compiled model's visual content. Geoms
  (type, group, size params, mesh ref, material ref, initial pose), materials
  (rgba, specular, shininess, reflectance, texture refs), sites, spatial tendon
  definitions, skin bind data. Textures baked to real images at export (procedural
  checkers etc. become PNGs/KTX2 inputs).
- capture file: sequence of stream frames (below) dumped verbatim. Interchange only;
  import bakes it to clips and the raw file is disposable.

### Pose sink API (the renderer's actual contract)

The renderer knows nothing about transport, formats, or timing sources. Its entire
surface is a mapping layer in **scene-loader**: after load it resolves each sim
instance's geom_id→node binding, and exposes a pose sink through the same frontend
API surface as everything else (wasm bindings / player API):

- apply a frame of world poses (f32) for an instance's geoms, addressed via the
  geom_id→node_id binding — this also drives skins, whose joints are body nodes;
- optional channels of the same shape: tendon waypoints and contact data (a
  dev-overlay concern for the feeding player; nothing in this repo consumes it).

Anything that can call this drives the renderer: our reference client, a
third-party sim with its own playback format, a test harness. The bake tool
(capture→clip) is likewise just another producer, offline.

### Stream convention (documented, not shipped as code in this repo)

This repo ships no networking at all — no server, no client, and the **editor
does nothing websocket-based**. The editor's MuJoCo role is entirely file-based:
import the model, bake capture files into clips, scrub clips. "Recording"
happens sim-side (the harness writes the capture file where the sim runs); the
user imports it like any asset.

The documented wire convention — timestamped frames, geom_poses required,
tendon/contacts channels optional, handshake carrying the instance→geom table —
is for *players*: the templates-repo `physics-mujoco` demo implements it
(connect/reconnect + a few-frame jitter buffer resampling sim rate → render
rate live in the template, not here). Players are free to ignore the convention
entirely and feed the sink their own way. Debug visualization (contacts, force
arrows, joint axes) accordingly lives as a dev overlay in the reference
template — the only place with live sim data — and never in the editor or the
bundle format.

### Scene owns composition; the sim process owns the physics source

We never archive or transport MJCF/URDF — the external sim process owns its model
files outright. Our scene owns *composition*: which model instances exist, their
authored initial placements (instance roots), and native colliders. The connection
between the two is a fingerprint: at import, the instance component records the
source model's identity (filename + content hash), so a harness can match its
loaded models to scene instances and fail loudly on mismatch instead of silently
driving the wrong robot.

World composition is the player's job, not this repo's. The recipe (documented
here, demonstrated in the templates-repo `physics-mujoco` template) for a
MuJoCo-side player:

- match each model to its scene instance by fingerprint,
- attach all instances into a **single world** with mjSpec (namespaced) so robots
  collide with each other, each placed at its instance root's authored transform
  (Y-up→Z-up/metres conversion at this seam, reversed from the pose path),
- inject the scene's native collider components (box/sphere/capsule/mesh) as
  static geoms in the world body. One authored floor: rendered pretty by us,
  collided with by the sim,
- each frame, feed geom poses to the scene-loader pose sink.

Players compose however they like — the only contract is poses into the sink,
addressed through the binding the mapping layer resolves at load.

Static colliders only for now. Moving colliders (our animations driving sim
geometry) map to MuJoCo mocap bodies but require streaming poses *into* the harness
per step — deferred (see Non-goals).

### What sim output maps to (renderer targets)

Everything the sim drives reduces to existing renderer concepts — no new
representation:

- **Rigid geoms, sites, mocap bodies → node transforms.** All of Unitree-class
  models. Materials are static, mapped at import; MuJoCo has no blend shapes.
- **Skins (mjSkin) → our skinned meshes.** Vertex-weighted to bodies with bind
  poses; imports as a normal skinned mesh whose joints are the body nodes — driven
  by the same transform stream.
- **Spatial tendons → a preallocated pool of capsule segment nodes** driven by
  transforms + visibility (waypoint count varies as tendons wrap/unwrap). Awkward
  but inside the existing model.
- **Flex/deformables (core MuJoCo 3.0+, cloth/ropes/soft volumes; absent from
  URDF entirely) → a dynamic-vertex mesh.** The only target outside the
  transforms/blendshapes/skins set — but not a hard one: flex topology is
  STATIC after compile (fixed buffer sizes at load); only vertex positions
  stream per frame (one more optional channel, `nflexvert × 3` f32) + normal
  recompute. Needs a modest renderer feature (per-frame vertex-position upload
  into an existing mesh — same family as the skin/morph deform passes), not a
  redesign. Deferred behind the rigid path only because no robot model needs
  it; testable day one with MuJoCo's own flex demos (`model/flex/`, e.g. the
  cloth flag) through the identical wasm load path.

### Collider physics params

Collider component schema = universal core + optional per-engine extension block,
exposed in the inspector and pushed all the way to the bundle:

- **Universal**: sliding friction, restitution, collision layer/mask (encoded to
  contype/conaffinity for MuJoCo); density/mass reserved for future dynamic bodies.
- **`mujoco` extension block** (optional, sensible defaults): torsional + rolling
  friction (with `condim`), `solref`/`solimp` (MuJoCo has no restitution parameter —
  bounce/softness lives in the soft-constraint model; the universal restitution
  value maps only approximately onto solref), contact `margin`/`gap`, geom
  `priority`.
- **Portability caveat**: engines combine pairwise friction differently (MuJoCo:
  element-wise max modulo priority; Rapier: average; Box2D: geometric mean) —
  identical authored values do not produce identical contact behavior across
  engines.

### Conventions

- MuJoCo is Z-up, right-handed, metres. A fixed convention rotation lives on the sim
  instance's root node; the stream applies raw MuJoCo world poses beneath it. No
  per-frame conversion math anywhere else.
- Geom **group visibility** follows MuJoCo convention (groups 0–2 visible by
  default), exposed as per-group toggles. Menagerie models split collision vs visual
  meshes by group — honoring this is required to render the visual robot and not its
  collision capsules.

## Editor UI

Same patterns as model import / material assignment:

- **Import MuJoCo** flow (GLB + sidecar pair), MCP command included. Re-import
  updates in place, preserving user material overrides (same story as model
  re-import).
- **Sim panel** per instance (file-based only — the editor does nothing
  networked): model identity/fingerprint readout, import-capture → bake-to-clip
  flow, geom group visibility toggles, site visibility.
- **Collider components** in the inspector: shape (box/sphere/capsule/mesh),
  universal params (friction, restitution, layer/mask), collapsible `mujoco`
  extension section for the engine-specific params.
- Bound nodes: transform gizmo disabled + "driven by sim" indicator in outliner and
  inspector.
- Baked clips appear as normal animation assets (scrub/play/mix as usual).

## Verification — the template IS the integration test

The `physics-mujoco` template is the end-to-end oracle for the whole flow.
**Every phase's exit criterion is the template running its scenarios in a real
browser, driven and inspected via chrome-devtools MCP** (console clean,
screenshot evidence, motion actually happening — the Phase 0 loop, repeated
forever). A phase that compiles but hasn't been watched driving real physics
on-device is not done.

**Model switcher**: the template grows a dropdown (+ `?model=` query param for
scripted runs) selecting between test scenarios, each proving a different slice:

- `humanoid` — rigid ragdoll (Phase 0 baseline: primitives, poses, groups)
- `go2` (menagerie) — mesh geoms + MjVFS assets; informs/then exercises the
  Phase 1 exporter's mesh path
- `flag` (mujoco `model/flex/`) — flex deformables via the dynamic-vertex path
- `multi` — two robot instances + scene colliders in one composed world
  (mjSpec attach, fingerprint binding, inter-robot collision)
- more as phases land (tendon-driven model for Phase 4, etc.)

**Scene → bundle → sim round-trip**: separately from the template's stock
scenarios, each phase that touches formats must be verified in-browser through
the REAL authoring flow: compose a scene in the editor (import via Phase 1
flow, place instances, add colliders) → `export_player_bundle` → the template
loads that fresh bundle, binds by fingerprint, and the sim drives it. This is
the test that the editor/bundle pipeline and the template agree — iterating it
is the test of whether the whole flow works.

## Pipeline parity checklist

Each is a known silent-failure spot — the scene renders fine but can't bind the
stream:

- [ ] mujoco component exists in **both** editor-protocol and scene (bundle) formats
- [ ] survives `export_player_bundle`
- [ ] survives project save/load roundtrip
- [ ] added to `node_sync` effective-def merge if any field behaves per-mesh
- [ ] MCP command coverage for every editor action

## Decisions taken during implementation

Implementation details the plan left open, settled as they came up. Each prefers the
smaller/reversible option, per the settled decisions above.

- **`mujoco-sys` dlopens rather than links** (bindgen `--dynamic-loading` over
  `libloading`; no `build.rs`, no link flags). The crate is a normal workspace
  member, so `cargo check --workspace` has to keep working on a machine with no
  MuJoCo installed — which a link-time dependency would break for everyone. Only
  *running* the tools needs the library. Discovery order: `$MUJOCO_LIB` (exact
  file) → `$MUJOCO_DIR` (unpacked release: macOS framework / Linux `lib` /
  Windows `bin`) → system loader path.
- **Version lock is enforced at load**, not assumed: `mjModel` is a plain C struct
  whose layout moves between releases, so `Library::load` compares `mj_version()`
  against the generated `mjVERSION_HEADER` and errors out on a mismatch. Reading a
  3.10 model through 3.11 bindings would produce plausible-looking garbage geometry,
  which is far worse than a hard failure. Pinned to **MuJoCo 3.11.0**.
- **Bindings are checked in**, not generated at build time — so no `libclang`/bindgen
  requirement for anyone building the workspace. `regen-bindings.sh` documents the
  exact invocation; `EXPECTED_VERSION` derives from the regenerated header constant
  so there is no second thing to bump.

## Phases

0. **Proof-of-loop spike (templates repo) — DONE, on-device verified Aug 7
   2026.** Humanoid ragdoll steps live and renders composited into the stock
   bundle scene (screenshot proof; zero console errors). Repo:
   `templates/physics-mujoco`, first commit 40bafd7. Open stretch item:
   a mesh-geom model (menagerie Go2, OBJ assets via MjVFS) — that exercise
   directly informs the Phase 1 exporter's mesh path. A `physics-mujoco`
   template modeled on `physics-multithreaded` that proves the whole loop with
   ZERO renderer-repo changes: the official `@mujoco/mujoco` wasm build (3.11)
   in a JS module worker steps the DeepMind humanoid in real time and publishes
   geom world poses into a seqlock'd `SharedArrayBuffer`; the Rust render
   worker mirrors the model's geoms as meshgen primitives under a Z-up→Y-up
   convention root composited into the stock bundle scene, and applies the
   latest snapshot per frame via `set_local`. Renderer crates pinned to local
   paths; browser-verified via chrome-devtools MCP.

   Deliberately bypasses the plan pipeline (no exporter, no editor import, no
   scene-loader sink): the template builds its own mirror from mjModel fields
   read off the wasm module. That client-side mirror code is throwaway — it
   migrates behind the exporter (Phase 1) and the scene-loader sink (Phase 3)
   as those land, and the spike then becomes the real Phase-3 reference
   template. What it de-risks NOW: the wasm build's API surface, sim-rate SAB
   pose crossing, the convention root, geom→primitive mirroring, and
   bundle-scene compositing.

1. **Export + import + static render.** Bindings crate (`mujoco-sys`, native-only) +
   `packages/tools/mujoco-export-cli` (GLB + sidecar + mjpkg). Editor import command
   mints nodes/components/materials and stores the mjpkg on the instance. A
   menagerie Unitree model renders correctly (groups honored, materials mapped) in
   its initial pose; instance root placeable; model fingerprint (filename + content
   hash) recorded on the instance. Parity checklist green.
2. **Capture-to-clip bake.** Documented capture format (any harness can write it);
   bake tool converts capture → native animation clips; scrub/play in editor;
   player bundle plays the clip. Browser-test-suite scene + goldens replay a
   checked-in fixture capture — deterministic, no sim in CI.
3. **Pose sink + reference template.** Mapping layer in scene-loader (binding
   resolution + pose sink) on the player API — no networking in this repo; the
   stream convention (jitter buffer, reconnect, recording) lives in the
   template. In the **templates repo**: `physics-mujoco` template —
   modeled on `physics-multithreaded` (worker-based), running the MuJoCo **wasm
   build** in a worker to drive a real experiment with a real downloaded model
   (menagerie), implementing the composition recipe (fingerprint match, mjSpec
   attach, collider injection) and driving the sink — the worked example players
   copy. During development the template pins renderer crates to local paths and
   is browser-verified via chrome-devtools MCP; it doubles as the first producer
   exercising the sink API. Prereq: collider components exist in editor/scene
   formats (universal core + mujoco extension block).
4. **Full visual features.** Sites, spatial tendons (capsule chains from waypoint
   channel), skins. Heightfields baked to meshes at export (Phase 1 covers this in
   the exporter if trivial, else here). Flex/deformables: the dynamic-vertex
   mesh path + `flex_vertices` stream channel, verified against MuJoCo's own
   `model/flex/` demos (cloth flag) — last in this phase since no robot model
   needs it, but fully in scope (see "What sim output maps to").
5. **Template debug overlays.** Contacts/forces/joint axes/inertia via the dev-only
   channel + overlay toggles.
6. **Permanent reference doc.** Write `docs/mujoco.md`: how the feature works —
   architecture, the seam formats (sidecar, capture, stream convention, pose
   sink API), the mujoco component + fingerprint binding, collider param
   mapping, retarget taxonomy, conventions (Z-up→Y-up, groups), and the
   template's role — WITHOUT phases, statuses, or implementation history. Then
   delete this planning doc (its durable content lives there).

## Non-goals

- Running MuJoCo in the browser *in this repo* — the templates-repo `physics-mujoco`
  demo does exactly that (MuJoCo wasm build in a worker), but no MuJoCo runtime code
  ever lives in the renderer.
- Parsing MJCF or `.mjb` in our own code, anywhere.
- Sim-vs-reality fidelity: we render exactly what the sim reports; the sim-to-robot
  gap is MuJoCo's domain.
- User-editable transforms on sim-bound nodes.
- Moving/animated scene colliders in the sim (mocap-body path requires streaming
  our poses into the harness per step) — static colliders only for now.
