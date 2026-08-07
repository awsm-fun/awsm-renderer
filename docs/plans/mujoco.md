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

- [x] mujoco component exists in **both** editor-protocol and scene (bundle) formats
      — one `EditorNode` type backs `EditorProject` (authoring) *and* `Scene`
      (bundle), so `EditorNode::mujoco` is in both by construction. It also had to
      be added to `NodeSpec`, which is the editor's real serialization pivot (see
      below). Guarded by `mujoco::tests::round_trips_through_toml_and_json` and
      `the_on_disk_shape_is_stable`.
- [x] survives `export_player_bundle` — the bundle's `scene.toml` comes from
      `to_editor_project` → `project_to_scene`. The first half was watched
      on-device (`editor_project_toml`, below); the second is guarded by
      `bake::tests::the_mujoco_component_survives_the_bundle_bake`, which asserts
      the fingerprint/geom_count/geom ids survive a real `scene_to_toml` →
      `scene_from_toml`. Re-checked for tendons on-device (Aug 8 2026): the
      exported `scene.toml` carries all 38 `tendon_segment` blocks and
      `tendon_capacity`, and `load_player_bundle` resolved them back to a
      `tendon_frame_len` of 138.
- [x] survives project save/load roundtrip — **verified in the browser**: a
      hand-written `project.toml` with an instance + geom loaded via
      `load_project_from_url`, and `editor_project_toml` read back every field
      (source fingerprint, `geom_count`, `visible_groups`, `geom_id`, `group`,
      `kind`, `body`). The first attempt came back EMPTY — see the `NodeSpec` note
      under "Decisions taken during implementation".
- [x] added to `node_sync` effective-def merge if any field behaves per-mesh —
      **N/A, checked not assumed**: `node_sync::builtin_merged`/`merged_builtin_def`
      merge a `MaterialInstance` onto a `MaterialDef`. Nothing in the mujoco
      component is a per-mesh material override, so there is no merge to join.
- [x] MCP command coverage for every editor action — `import_mujoco_from_url` is
      a dedicated typed tool, with a row in `docs/mcp-parity.md` and the wire tag
      in `packages/mcp/tests/parity.rs` (the CI guard fails both directions on
      drift, and did).

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
- **The seam formats get their own crate**, `packages/crates/mujoco-format`
  (`awsm-renderer-mujoco-format`): pure serde, zero MuJoCo dependency, therefore
  wasm-safe. Both ends need it — the native exporter writes it and the wasm-side
  editor/scene-loader read it — and neither should have to reach through the other.
  Enums serialize as documented strings (`"capsule"`, not `3`) so MuJoCo's internal
  numbering never leaks into a format third parties hand-write, unknown fields are
  ignored and absent ones default (additive changes never bump `VERSION`), and
  `Sidecar::validate()` checks the index references JSON itself cannot express.
- **The sidecar stores raw MuJoCo coordinates** (Z-up, metres, `[w,x,y,z]` quats),
  with no conversion. The Y-up flip stays exactly where the plan puts it — once, on
  the instance root node — because converting at both layers would apply it twice
  and would bake a second copy of the convention into a hand-writable format.
- **Geom order is never filtered or reordered**: the array index *is* the MuJoCo
  geom id the pose stream addresses. Collision geoms and invisible groups are
  exported with their real group; deciding what to render is the importer's job.
- **The exporter is not cargo-dist'd** (unlike `lod-bake-cli`/`env-bake-cli`): a
  prebuilt binary is useless without a matching local MuJoCo install, and shipping
  one would imply we redistribute MuJoCo. Users build it themselves.
- **The GLB is a flat geometry *library*** — one root node per MuJoCo mesh, in mesh
  order, identity transform, **no materials at all**. Not a scene: geoms are
  instances of meshes (many geoms share one mesh), and materials are minted in our
  system at import. Emitting glTF materials would mean a re-import silently
  inherits them instead. The sidecar's `Mesh::node` names its GLB node explicitly
  so a reader never reproduces our naming rule and a GLB that has been through
  another tool still binds.
- **De-indexing on export, not import.** MuJoCo stores meshes OBJ-style (separate
  index arrays for position/normal/texcoord), so one triangle corner can pull from
  three different slots; glTF has one index per vertex. Each distinct
  `(pos, normal, uv)` triple becomes one vertex, so sharing survives. UV V is
  flipped here too — the GLB is then a plain correct glTF any viewer opens right,
  rather than one that needs our importer to look correct.
- **The mujoco component is a field on the node, not a table on the scene.**
  `EditorNode::mujoco: Option<MujocoComponent>` — an enum, `Instance` or `Geom`,
  because a node is one or the other and never both. On the node it travels with
  the subtree through copy/delete/reparent, and it needs no second structure to
  keep in sync. The plan's "scene-level sim instance list" is then derived (every
  node carrying an `Instance`), which supports N instances for free.
- **No stored geom_id→node_id map.** Each geom node knows its own `geom_id`, so
  the binding is resolved by walking the instance subtree at load — which is what
  the plan's pose-sink section already says ("resolves"). A stored map would be a
  second copy free to drift from the tree it describes, and drift there means
  poses applied to the wrong nodes, which reads as a physics bug rather than a
  data bug. `geom_count` *is* stored, because it cannot be recovered: an import
  that skips hidden groups leaves gaps, and a frame sized to the visible nodes
  would be misaligned from the first skipped geom onward.
- **`NodeSpec` is the editor's real serialization pivot, not `EditorNode`.**
  Adding the field to the shared scene schema was NOT enough: the editor's
  reactive `engine::scene::Node` converts through `NodeSpec` (`spec_from_node` /
  `node_from_spec`), so the component was silently dropped on save even though
  every schema round-trip test passed. Any future per-node field has to be added
  in four places — `EditorNode`, `NodeSpec`, both conversions, and the reactive
  `Node` — and only a browser round-trip catches missing any of them.
- **Heightfields are baked to meshes at export, so nothing downstream knows what
  a heightfield is.** MuJoCo's grid is static after compile, so there is nothing
  dynamic to preserve; an hfield geom arrives at the editor, the scene format and
  the pose sink as an ordinary mesh geom. The sidecar still records `type:
  "hfield"` (it is a faithful record of the compiled model) with `mesh` pointing
  at the baked entry, which is appended after the real meshes. The bake emits the
  top surface plus a skirt down to the model's base depth, on its own vertices,
  so the terrain reads as solid at a grazing angle and the rim fold stays sharp.
- **Sites are their own component and their own stream channel**, not a flag on
  `MujocoGeom`. MuJoCo indexes sites separately from geoms, so a shared id space
  would put a site's pose in a geom's slot — the exact class of silent failure
  the geom-id binding exists to prevent. `MujocoSite { site_id, group, kind,
  body }`, `MujocoInstance::site_count`, a second `sites` array in the sink's
  binding, and `apply_site_poses` alongside `apply_geom_poses` (both over one
  shared `write_poses`, so the two channels cannot drift in behaviour).
- **Collider physics params are a separate optional node field**, not a payload
  on `NodeKind::Collider` (which carries the SHAPE). Widening the variant would
  break every saved project and baked bundle for a field absent on almost every
  node; an additive `EditorNode::physics: Option<PhysicsParams>` costs nothing and
  is also where a future dynamic body's mass properties belong.
- **`SetPhysicsParams` is a whole-value replace, not a patch.** The params are a
  small fully-specified struct whose fields interact — MuJoCo's torsional and
  rolling friction are inert below `condim` 4 and 6 — so merging half of one
  authored block into another is more confusing than restating it. Omitting
  `params` clears the block back to engine defaults.
- **An omitted `mujoco` sub-field falls back to MuJoCo's OWN default**, not to
  zero, so authoring one knob doesn't silently re-specify the contact model.
  Layer/mask default to "everything", so an author who never touches them gets
  collisions rather than silence.
- **The pose sink's binding is a `Vec<Option<TransformKey>>` indexed by geom id**,
  sized to the model's full geom count — not a map. It is indexed once per geom
  per frame, so an array lookup is the right shape; geoms the scene chose not to
  render are simply `None`, and the id space stays the model's so a producer
  never has to re-index a frame.
- **A pose frame carries translation and rotation only; scale is preserved** by
  read-modify-write. An ellipsoid geom is a unit sphere scaled per-axis by its
  node, so overwriting scale would flatten it on the first frame.
- **A mis-sized frame is an error, never a truncation.** Applying a short frame
  would drive every geom past the mismatch from the wrong slot — motion that
  reads as a physics bug rather than a protocol one.
- **The editor gets two test seams** (`editor_apply_mujoco_poses`,
  `editor_mujoco_instances`), mirroring `editor_tick_animation`: after a
  `LoadPlayerBundle` reload the scene exists only in the renderer, exactly as it
  does for a player, so driving the sink there exercises the real path. The
  editor never calls them itself; a player calls
  `scene_loader::mujoco::apply_geom_poses` against its own `LoadedScene`.
- **Baked keyframes carry ZEROED tangents**, matching the editor's own
  `new_keyframe`. Echoing `value` into `in_tangent`/`out_tangent` (as the first
  cut did) both implies a cubic tangent that happens to equal the value and
  triples the clip's on-disk size — it cost 22% of a real scene's `project.toml`
  before it was caught by looking at the saved file.
- **The capture import rejects a fingerprint mismatch before baking anything.**
  A capture of a different model bakes into poses that are individually plausible
  and collectively nonsense — the wrong robot driven convincingly, which is close
  to undiagnosable. One string comparison prevents it, and the error names both
  models and both hashes.
- **The bake lives in `awsm-renderer-scene`, as pure data in / pure data out** —
  a capture plus a geom_id→node map to a `StoredAnimation`. So it is natively
  unit-testable, the editor command on top is a thin wrapper, and nothing about
  it needs a renderer or a filesystem. The binding is *derived* by walking the
  instance subtree (`bake::binding_of`), never stored, for the same
  no-second-copy reason as everywhere else.
- **Static geoms get no tracks at all.** A robot's world is mostly static —
  floors, fixtures, scenery — and keying those nodes flat every frame would both
  bloat the bundle and quietly fight anything else animating them.
- **Quaternions are made continuous before keying.** A simulator is free to emit
  `q` on one frame and `-q` (the same rotation) on the next; interpolating across
  that sign flip takes the long way round, so a limb visibly spins a full turn
  between two nearly-identical keys. This is the single most likely way baked
  physics *looks* broken, and it costs one dot product per frame to prevent. The
  "did this geom move?" test runs after the flip, so a pure sign change is
  correctly read as no motion rather than resurrecting a static geom.
- **Keyframe reduction measures against the last KEPT key**, not the original
  neighbours, so error cannot accumulate across a long run of dropped keys.
  Position and rotation share one keep-mask because they share the clip's `times`
  array — keeping a key for one and not the other would need two time bases per
  geom. Defaults: 1 mm and ~0.1°, which on a real 3 s humanoid run keeps 32% of
  the keys.
- **Track order is sorted, not HashMap order**, so two bakes of the same capture
  produce identical clips — otherwise golden comparison in the browser suite
  would be defeated by iteration order alone.
- **The capture format mirrors the live stream exactly** — a flat
  `7 * geom_count` `f32` array per frame, `[px,py,pz,qw,qx,qy,qz]` indexed by geom
  id, plus a time in seconds. A harness that can feed the pose sink records a
  capture by writing what it was already sending, with no translation step on
  either end. `f32` because that is what the sink takes: storing more precision
  than the sink accepts would be storing a difference nothing can observe.
- **The capture carries the model fingerprint**, and `geom_count` is stated once
  at the top rather than inferred per frame — a truncated frame is then caught
  instead of shifting every geom after it, which would read as a physics bug
  rather than a bad file.
- **`awsm-renderer-mujoco-record` is a second binary in the exporter crate**, not
  a second crate and not a mode of the exporter: recording is a different job
  (run a sim, sample poses) that happens to need the same model loading, and a
  capture producer is meant to be *copyable* rather than privileged. It samples
  on **step counts**, never an accumulated float clock, so re-running it
  reproduces a capture byte-for-byte — which is what makes the checked-in test
  fixtures worth checking in.
- **Re-import is explicit, not inferred.** `ImportMujocoFromUrl` takes an optional
  `reimport: NodeId`: omitted, it always ADDS an instance; given, it updates that
  one in place. Matching on the model's identity instead would have broken the
  plan's own `multi` scenario — two instances of the same robot in one composed
  world — by silently turning the second import into an update. A re-import
  refreshes everything the model owns (poses, geometry, fingerprint, the geom
  table, restoring geoms whose nodes were deleted) and preserves everything the
  user owns (the root's id/name/**placement**, and each surviving geom's material
  palette, matched by `geom_id`).
- **Re-import builds the fresh subtree and transplants**, rather than diffing two
  live trees: the build path is the one already exercised, and a separate "update"
  path would be free to drift from it.
- **Library materials dedupe on name AND definition.** Without it, every
  re-import minted a fresh "metal"/"black"/… and stranded the old set (8 materials
  after one re-import, observed in the browser). Matching the definition too means
  a user who has *edited* "metal" keeps their edit — the incoming def no longer
  matches, so the model's version arrives beside theirs instead of overwriting it.
- **Primitive geoms become captured meshes, not `PrimitiveShape` recipes.** Two of
  MuJoCo's shapes — capsule and ellipsoid — have no `PrimitiveShape` equivalent at
  all, and sim-bound geometry is stream-owned and not meant to be edited, so one
  uniform captured path beats a recipe/capture split that would only make some
  geoms editable. Shapes are deduped by (kind, size) so the humanoid's 16 capsules
  mint a handful of assets, not 16. Ellipsoid is the one exception to "geometry
  carries the size": it is a unit sphere with per-axis **node scale**, which is
  safe because a pose frame writes translation and rotation only.
- **MuJoCo axis conventions are honoured in the geometry, not worked around.**
  MuJoCo capsules and cylinders run along local **Z** and our `meshgen` primitives
  along Y, so those meshes are rotated +90° about X at build time — a proper
  rotation, so winding and normals stay valid. MuJoCo planes have a +Z normal and
  are often authored infinite (a `0` half-extent); those fall back to a finite
  10 m quad, which is what MuJoCo's own viewer effectively shows.
- **MuJoCo Phong → our PBR is a documented approximation**, tuned to look right
  on the menagerie models rather than to be theoretically pure: `rgba` → base
  colour (alpha < 1 turns on blending), `roughness = 1 - shininess` (MuJoCo's
  shininess is normalized gloss), `metallic = reflectance` (MuJoCo's only
  mirror-like term; it has no metalness concept and most models leave it at 0),
  `emissive = rgba * emission`. **`specular` is deliberately dropped**: in a
  metallic-roughness model a dielectric's specular intensity is fixed, and
  folding MuJoCo's value in would make every menagerie part — they all default
  to `specular = 0.5` — read as half-metal.
- **Materials go into the assignable library, not inline per node**, exactly as
  the glTF importer does, so editing "metal" once repaints every metal part and a
  MuJoCo material can be reused on non-MuJoCo geometry. Material-less geoms get a
  material minted from their own `geom_rgba`, deduped by colour, so a geom is
  never left on the magenta unassigned sentinel.
- **Geom nodes are flat under the instance root, not nested by MuJoCo body.**
  MuJoCo reports every geom's *world* pose every frame, so a body hierarchy would
  only be a second place for those poses to compose — and composing twice is the
  drift the flat layout avoids. `MujocoGeom::body` still records the owning body
  for the skin path and for harness-side body queries.
- **The sidecar records each geom's WORLD pose at `qpos0`**, evaluated by MuJoCo
  itself (`mj_makeData` + `mj_forward`, never `mj_step` — we want where the model
  starts, not where it falls). Two reasons: composing `pos` up the body chain
  reproduces the *joint-zero* pose, which is not `qpos0` for any robot with a
  reference configuration; and a world pose is exactly the shape a stream frame
  carries, so the simulation's first frame continues from the initial render
  instead of jumping. The body-local `pos`/`quat` stay in the sidecar as the
  model's authored data.
- **The sidecar names its own GLB** (`Sidecar::glb`, relative to the sidecar).
  The pair is then self-describing and a consumer never has to know our naming
  convention.
- **Face indices are bounds-checked per corner.** MuJoCo's face indices being
  mesh-relative rather than absolute into the shared pool is a convention read off
  the library, not a header guarantee. Unchecked, a change there would read a
  neighbouring mesh's vertices and produce geometry that looks *almost* right.

### Tendon segments are a preallocated pool of unit cylinders (Aug 8 2026)

A tendon's waypoint count *changes at runtime* as it wraps and unwraps around
geometry, and a pose stream can only write transforms — it cannot create nodes.
So the importer mints the whole chain up front and hides the unused tail:

- **The pool bound comes from the compiled model, not from a sampled frame.**
  `max_waypoints = 2 * tendon_num` is MuJoCo's own rule for sizing `wrap_xpos`,
  so it is the true ceiling. Sizing to the qpos0 routing instead would leave a
  tendon short of segments the first time it wraps (arm26's `BF` routes through
  7 waypoints at rest but is allowed 10).
- **Segments are cylinders, not the capsules MuJoCo draws.** Length lives in the
  node's Z scale, and a cylinder is the one shape that scales along its axis
  without distorting; a scaled capsule squashes its caps. At tendon widths
  (3–10 mm) the missing end-caps are not visible.
- **Fixed tendons get a slot but no pool.** A model's tendon list mixes spatial
  tendons with FIXED ones (joint-coupling constraints with no path through
  space). They keep their index — that index *is* the MuJoCo tendon id, which a
  stream binds against — but export `max_waypoints: 0` and no waypoints, which
  the importer already skips. Told apart by `wrap_type`, the compiled truth,
  rather than by a runtime `ten_wrapnum` of 0.
- `MujocoInstance::tendon_capacity` carries the per-tendon pool sizes so a
  consumer laying out a stream frame never has to re-derive them from the tree.

### The tendon channel carries waypoints, and hiding is edge-triggered (Aug 8 2026)

Unlike the geom and site channels, a tendon frame is not a list of poses:

- **Layout is `[live_count, then the full capacity as xyz triples]` per tendon,
  in tendon-id order.** Fixed size even though the live count varies, so a
  producer can publish it into a preallocated `SharedArrayBuffer` — the same
  reason the pool itself is preallocated. A producer reads it straight out of
  `ten_wrapadr`/`ten_wrapnum`/`wrap_xpos`.
- **A count above the pool size is an error, not a clamp.** It means the
  producer and the imported model disagree about which model this is, so the
  rest of the frame is untrustworthy too — the same reasoning as `WrongLength`.
- **Segment visibility is edge-triggered**, which is why
  `apply_tendon_waypoints` takes `&mut MujocoInstance` while the other two
  channels take `&`. `set_mesh_hidden` bumps the TLAS revision and re-syncs the
  spatial index; re-asserting it every frame for every segment would churn the
  BVH for nothing. The instance remembers how many segments it last showed.
- **The authored RADIUS survives**, the same read-modify-write rule the geom
  channel uses for ellipsoid scale: a frame carries no width, so only the
  segment's Z scale (its length) comes from the stream.

### A flex is a mesh asset, and how it deforms is a later decision (Aug 8 2026)

MuJoCo's flex is a soup of vertices — each rigidly attached to a body — plus
elements. The exporter reduces that to something the rest of the pipeline
already understands:

- **The surface is baked into the geometry GLB** and gets a synthetic
  `Sidecar::meshes` entry, appended after the real meshes and the heightfields.
  A flex is therefore just another mesh asset downstream, and the importer
  needed no flex-specific GLB plumbing at all — the same trick heightfields use.
- **Only the surface.** A 2D flex's ELEMENTS already are the triangles; a 3D
  flex's elements are tetrahedra whose visible boundary is `flex_shell`. Drawing
  a solid's tetrahedra would fill its inside with invisible faces. A 1D flex (a
  rope) has no surface and bakes nothing.
- **Vertices are world-space at the initial pose**, read from `mjData` rather
  than `flex_vert` (which is each vertex in ITS OWN body's frame). So the node
  transform is identity: a deformable has no rigid frame to place it by.
- **Normals are computed here**, area-weighted, because MuJoCo carries none —
  its own visualizer derives them every frame. They are correct at the bind pose
  and go stale under deformation, which is the streaming half's problem.
- **`vertex_bodies` is all-or-nothing.** A flex with interpolation (`trilinear`,
  `quadratic`) drives its vertices from a smaller cage of nodes and MuJoCo
  reports no body per vertex; a partial list would let a consumer skin some
  vertices and strand the rest at the bind pose. Empty means "streaming is the
  only way to deform this".

That last field exists because the renderer route is genuinely open: a
body-attached flex could be **skinned** to its vertex bodies (one joint per
vertex, weight 1) using the existing GPU skinning path and the existing
transform channels, with no new renderer capability at all; an interpolated one
can only be driven by uploading vertex positions, which the renderer has no path
for today. Deciding that is the next flex increment, and the exported data
serves either way.

### Forward-compat fixtures need a name that is still hypothetical (Aug 8 2026)

`sidecar.rs`'s "a later producer added fields" test has now been broken twice by
the placeholder table becoming real (`sites`, then `tendons`). Whenever that
happens, rename the placeholder — otherwise the test quietly stops testing
forward compatibility and starts testing the new table's schema.

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

   Progress: `mujoco-sys` ✅, `mujoco-format` (sidecar schema) ✅, exporter
   sidecar ✅ + GLB ✅, `mujoco` node component ✅ (parity checklist green),
   `import_mujoco_from_url` editor command + MCP tool ✅, materials ✅.

   **Stated exit criterion MET on-device, Aug 7 2026**: the menagerie Unitree
   Go2 renders as the actual robot in the editor — white shell with the "GO2"
   logo legible, black feet, black sensor grille, standing on four legs at its
   qpos0 pose. Groups honoured (33 visible-group geoms; collision group 3
   skipped), materials mapped (metal/black/white/gray in the library, 4 buckets),
   fingerprint on the instance root, console clean.

   Primitive geoms ✅ (Aug 7 2026): the DeepMind humanoid imports and renders as
   a humanoid — head/hand spheres, capsule limbs and torso, both feet, standing
   on its MuJoCo ground plane, 21 nodes / 20 meshes, console clean.

   Re-import in place ✅ (Aug 7 2026, browser-verified: two consecutive
   re-imports keep one root with its id and authored placement `[3,0,-2]`,
   restore a geom node the user had deleted (33 children again), keep a
   user-added+selected material variant, and hold the library at 4 materials
   instead of accumulating).

   **PHASE 1 COMPLETE.** (Heightfield and SDF geoms still import as empty nodes
   with correct geom ids — heightfields are explicitly Phase 4 in this plan.)

   Discovered while reading real models:
   - **MuJoCo re-frames mesh geoms at compile time.** A mesh geom's `geom_pos`/
     `geom_quat` are relative to the *mesh's own inertial frame*, not the frame
     the MJCF author wrote, because the compiler recentres mesh vertices. Go2's
     visual geoms accordingly carry non-identity pos/quat. This is self-consistent
     as long as the GLB's vertices come from `mjModel`'s `mesh_vert` (post-compile)
     and never from the original OBJ/STL — which is another reason we only ever
     read the compiled model.
   - **`geom_dataid` is overloaded**: mesh index for mesh geoms, heightfield index
     for hfield geoms. Only ever dereference it against the geom's type.
   - Go2's collision geoms carry no material, so a material-less geom must fall
     back to `geom_rgba` rather than being treated as an error.
2. **Capture-to-clip bake.** Documented capture format (any harness can write it);
   bake tool converts capture → native animation clips; scrub/play in editor;
   player bundle plays the clip. Browser-test-suite scene + goldens replay a
   checked-in fixture capture — deterministic, no sim in CI.

   Progress: capture format ✅ (`mujoco-format::capture`) + reference recorder
   ✅ (`awsm-renderer-mujoco-record`) + the bake ✅
   (`scene::mujoco::bake`, capture → `StoredAnimation`). Verified natively against real physics —
   the humanoid ragdoll's torso falls 1.28 m → 0.26 m over 3 s, every quaternion
   stays unit to 7e-8, two runs are byte-identical, and frame 0 matches the
   sidecar's `world_pos` to 1e-5 so the first simulated frame continues from the
   imported pose rather than jumping. The bake was verified end-to-end on that
   same real capture: the torso's baked translation track still falls 1.28 m →
   under 0.5 m, reduction keeps 2470 of 7638 keys (32%), the static floor plane
   contributes no track, and the clip round-trips through the project/bundle TOML
   as ordinary animation data.

   Editor command ✅ + MCP tool ✅ (`import_mujoco_capture`, parity row added).
   **Scrub/play verified on-device Aug 7 2026**: imported the humanoid, baked a
   4 s / 267-frame capture into a "ragdoll fall" clip (tracks reduced to 45–98
   keys each), and scrubbed it — the torso's world Y reads 1.282 → 1.040 →
   0.271 → 0.261 at t = 0 / 0.8 / 1.6 / 3.9, and the screenshots show the
   humanoid standing at t=0 and collapsed in a heap on the floor at t=3.9. The
   fingerprint guard was exercised too: baking a Go2 capture onto the humanoid
   instance is refused with both model names and hashes, and adds no clip.

   **Player bundle plays the baked clip ✅ (Aug 7 2026, on-device).** Composed
   humanoid + baked clip → `load_player_bundle` (bake to an in-memory bundle,
   reset, reload through `populate_awsm_scene` — the runtime/player path). The
   editor tree goes EMPTY (0 objects) while the renderer reports
   `clip_groups: 1, per_clip: [{channels: 38, name: "ragdoll fall"}],
   resolved_channels: 38` — all 38 baked channels resolved through the player
   loader. Ticking the renderer's own animation clock (39 x 100 ms, the call a
   game makes each frame) plays the fall: the humanoid starts standing and ends
   collapsed on the floor, shadowed by the bundle's directional light. Nothing in
   the editor could be driving it — there is no editor scene left.

   **Browser-test-suite scene ✅ (Aug 7 2026, on-device).**
   `examples/test-scenes/mujoco-capture/` — `fixtures/` (a checked-in sidecar +
   a 2 s / 58-frame capture, 112 KiB together, so CI never runs physics),
   `author.js`, `project/`, `bundle/`, `golden.png`, `verify.md`, and a row in
   the suite README. Authored through the real MCP pipeline (`save_project` /
   `export_player_bundle` / `screenshot_scene`); the golden shows the humanoid
   frozen mid-collapse at t=1.2 s on its own ground plane, grid/gizmos off.

   **PHASE 2 COMPLETE.**
3. **Pose sink + reference template.**

   Pose sink ✅ (Aug 7 2026): `scene_loader::mujoco` — `MujocoInstance` (root,
   fingerprint, geom_id→TransformKey binding), `resolve_instances` (walks the
   tree at load; `LoadedScene::mujoco`), `apply_geom_poses`. **Verified
   on-device**: imported the humanoid with NO clip, ran `load_player_bundle`
   (editor tree empty, 0 clips — nothing but the sink can move anything), and
   fed all 58 frames of the recorded fixture capture straight into
   `apply_geom_poses`. The humanoid went from standing to collapsed on the
   floor; 58/58 frames applied with zero errors; console clean. Both guards
   fired: a 100-float frame → *"pose frame has 100 floats, this instance needs
   140"*, and instance index 3 → *"no sim instance 3 (the last bundle reload
   resolved 1)"*.

   Collider components ✅ (Aug 7 2026) — `collider::PhysicsParams` (universal
   friction / restitution / layer / mask / density) + `MujocoPhysics`
   (torsional + rolling friction, `condim`, `solref`/`solimp`, `margin`/`gap`,
   `priority`), on `EditorNode::physics`, with the `SetPhysicsParams` command and
   the `set_physics_params` MCP tool. Parity: it is one field on the shared
   `EditorNode`, so **both** formats carry it; the bundle bake is guarded by
   `bake::tests::collider_physics_params_survive_the_bundle_bake`; **save/load was
   verified in the browser** (`set_physics_params` on a collision box →
   `reload_project_in_memory` → the `[nodes.physics]` + `[nodes.physics.mujoco]`
   blocks come back byte-identical, with the un-authored MuJoCo sub-fields filled
   from MuJoCo's own defaults); `node_sync` is N/A for the same reason as the
   mujoco component; MCP coverage is the dedicated tool + parity row.

   Template migrated onto the sink ✅ (Aug 7 2026, templates-repo commit
   b6d4042). The Phase-0 client-side mirror is gone: the robot is authored
   content shipped in `media/bundle`, and the render worker only takes
   `LoadedScene::mujoco` and hands the seqlock'd snapshot to `apply_geom_poses`.
   `render_thread.rs` 693 → 460 lines. The worker's SAB pose block now writes
   quaternions in MuJoCo's `[w,x,y,z]` order, so the block **is** a stream frame
   in the documented convention and nothing is reshaped on either side — the
   worker could dump it verbatim as a capture. **Browser-verified**: HUD reads
   *"sim linked (20/20 geoms) — running"*, the humanoid is collapsed on the
   ground under live physics from a bundle whose authored pose is standing,
   console clean.

   **PHASE 3 COMPLETE.**

   Original scope: mapping layer in scene-loader (binding
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
4. **Full visual features.**

   Sites ✅ (Aug 7 2026, on-device): sidecar `sites` table, `MujocoSite`
   component, importer nodes, and the sink's site channel. Verified with MuJoCo's
   `tendon_arm/arm26.xml` (11 sites, 5 geoms) — the editor shows all 11 named
   site nodes (`s0`, `x0`, `s1`…`s8`, `x1`) rendered as white/green spheres at
   their MuJoCo world poses along the arm, `[nodes.children.mujoco.site]` blocks
   carry `site_id` 0–10 in their own id space beside the 5 geom blocks, console
   clean.

   Heightfields ✅ (Aug 7 2026, on-device): baked at export.
   `google_barkour_vb/scene_hfield_mjx.xml` exports its 20x20 m terrain as a
   69,616-vertex mesh with **256 distinct elevation levels** (an 8-bit PNG
   heightfield read correctly) spanning z −0.100 → +0.050, exactly the model's
   `size` z-scale and base depth. It renders in the editor as undulating ground
   under the Barkour robot, console clean.

   Spatial tendons, static half ✅ (Aug 8 2026, on-device): sidecar `tendons`
   table, `MujocoTendonSegment` component, `segment_transform` (shared by the
   editor and — next increment — the sink), and the importer's **preallocated
   segment pool**. Verified with `tendon_arm/arm26.xml`: 6 tendons mint 38
   segment nodes (`SF 0`…`BE 8`), 14 of them starting hidden as spares, and the
   viewport shows the muscle cables routed through the arm's site spheres and
   bending at each waypoint. `tendon_capacity = [6, 6, 6, 6, 10, 10]` survives a
   real save → `load_project_from_url` → re-save roundtrip (proved live by a
   marker rename in the served `project.toml`), with all 38 `tendon_segment`
   blocks and 14 `visible = false` intact. MuJoCo's `humanoid.xml`, whose only
   tendons are FIXED (joint coupling), imports 0 segment nodes and
   `tendon_capacity = [0, 0]`. Console clean throughout.
   Spatial tendons, stream half ✅ (Aug 8 2026, on-device):
   `apply_tendon_waypoints` in the sink, plus the `editor_apply_mujoco_tendons`
   test seam. Verified through the REAL player path — author in the editor →
   `export_player_bundle` → `load_player_bundle` → drive: replaying the model's
   own qpos0 waypoints reproduces the imported placement pixel-for-pixel (the
   editor and the sink share `segment_transform`, and this is what proves it);
   bowing the waypoints outward visibly plucks the six cables off the arm with
   their site endpoints still pinned; filling every tendon to its full capacity
   makes the hidden spare segments appear as smooth many-segment arcs; and
   shrinking back to two waypoints collapses each tendon to one chord and
   re-hides the rest. Both error paths fire (`WrongLength`, `TooManyWaypoints`).
   Console clean.

   Flex surfaces ✅ (Aug 8 2026, on-device): sidecar `flexes` table, the surface
   baked into the geometry GLB as an ordinary mesh asset, `MujocoFlex`
   component, and importer nodes. Verified with MuJoCo's own `model/flex/`
   demos: `flag.xml` imports as a flat 171-vertex / 288-triangle cloth sheet
   hanging at its qpos0 height, and `bunny.xml` imports as a recognisable
   2,503-vertex Stanford bunny — a wrong index rebase or the wrong
   element/shell choice would give a mess, not a bunny. `body_attached` reads
   `true` for the flag and `false` for the interpolated bunny. Console clean.
   **Flex DEFORMATION is deferred** — see the open-items list at the top of the
   log. The surface ships and renders at its bind pose; making it move needs
   either a new renderer dynamic-vertex capability or a skinned-import path
   through the editor's MuJoCo importer, and both are multi-increment renderer
   work for the item this plan itself ranks last. The exported data
   (`vertex_bodies`, `vertex_count`) serves either route unchanged.

   **Skins: dropped, on evidence.** `mjSkin` is MuJoCo's older deformable and is
   dead in 3.11 — `nskin == 0` for every model shipped with the release and for
   every menagerie robot (checked, not assumed). The one model in the tree with
   a `<skin>` element, `plugin/elasticity/belt.xml`, does not even compile
   without a plugin dylib we do not load. Flex is the deformable path that
   actually exists, and it is in scope below. If a skinned model ever turns up,
   the flex surface path generalises to it.

   Original scope: Sites, spatial tendons (capsule chains from waypoint
   channel), skins. Heightfields baked to meshes at export (Phase 1 covers this in
   the exporter if trivial, else here). Flex/deformables: the dynamic-vertex
   mesh path + `flex_vertices` stream channel, verified against MuJoCo's own
   `model/flex/` demos (cloth flag) — last in this phase since no robot model
   needs it, but fully in scope (see "What sim output maps to").
5. **Template debug overlays.**

   Contacts ✅ (Aug 8 2026, on-device): the sim worker publishes contact
   positions + normals in the SAME seqlock as the poses (so an overlay can never
   draw contacts from a different step than the bodies they touch), and the
   render thread draws a preallocated pool of red spikes parented under the sim
   instance's root transform. Opt in with `?contacts`. Verified in the template
   at :9000: the collapsed humanoid reports 10-13 live contacts, drawn as four
   spikes along the resting foot plus more at the hip, each standing along its
   `+Z` floor normal. Console clean. Templates commit `ac44524`.

   Contact FORCES ✅ (Aug 8 2026, on-device): the worker publishes each
   contact's normal force alongside its position and normal, and the spike's
   LENGTH is that force. Verified on the collapsed humanoid: 13 live contacts
   with visibly different spike lengths at foot, hip, pelvis and arm, growing
   and shrinking as it settles; forces read 20-175 N at rest and ~3600 N on
   landing impact. Templates commits `2c5a1e4` + `43634cd`.

   Joint axes ✅ (Aug 8 2026, on-device): a blue bar through each hinge/slide
   joint's world anchor along its world axis (`mjData`'s `xanchor`/`xaxis`),
   opt-in with `?joints`. No pool needed — a model's joint count is fixed.
   Ball and free joints get no bar, since neither has a single axis. Verified
   on the humanoid alongside the contacts: bars at every shoulder, elbow, hip,
   knee, ankle and abdomen hinge, each along its own axis (a knee's
   perpendicular to its hip's). Templates commit `b596a45`.

   Remaining: inertia boxes.
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
