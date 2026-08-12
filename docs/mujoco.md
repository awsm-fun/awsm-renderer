# MuJoCo

Render MuJoCo simulations with awsm-renderer.

MuJoCo runs **somewhere else** — a native RL harness, a robot telemetry feed, a
wasm module in a worker — and this renderer draws what it reports. There is no
MuJoCo code in this repo outside the offline exporter, no networking, and no
timing source. The whole runtime contract is one function that writes a frame of
world poses onto node transforms.

That split is the design. Everything below follows from it.

## The pipeline

```
MJCF / URDF / .mjb
        │  awsm-renderer-mujoco-export  (native CLI, links libmujoco)
        ▼
<name>.mujoco.json   the sidecar: what the compiled model contains
<name>.glb           geometry only — meshes, baked heightfields, flex surfaces
        │  editor: ImportMujocoFromUrl
        ▼
a sim INSTANCE in the scene
  · one user-placeable root, carrying the Z-up→Y-up rotation and the model's
    content hash
  · one locked node per geom / site / tendon segment / flex surface beneath it
        │  export_player_bundle
        ▼
a player bundle
        │  scene_loader::mujoco::resolve_instances at load
        ▼
apply_geom_poses(renderer, instance, &frame)   ← a running simulation, each frame
```

Two things never cross this pipeline: the model file itself (the sim process owns
it outright — we archive a fingerprint, not an MJCF), and any MuJoCo semantics we
could not already express as renderer concepts.

## Never parse MJCF

MuJoCo's own C code compiles MJCF into `mjModel`: defaults inheritance,
procedural textures, mesh fitting, inertia derivation. Reimplementing any of that
is a bug factory, so the exporter links `libmujoco` and reads only the
**compiled** model. URDF comes free — libmujoco loads it natively into the same
`mjModel`. `.mjb` works too but is version-locked.

`mujoco-sys` binds the small surface we need via bindgen with
`--dynamic-loading`, so the crate is an ordinary workspace member that builds and
tests with no MuJoCo installed; it `dlopen`s the library at runtime from
`$MUJOCO_LIB`, `$MUJOCO_DIR`, or the system soname. The struct layout is
version-locked to the headers the bindings were generated from (3.11), and
`Library::load` checks the runtime version against them.

## The two seam formats

Both are plain serde, documented and frozen, so a third-party pipeline — a Python
RL shop, a CI job — can produce valid input without touching this workspace. Our
Rust tools are just the first producers.

Additive changes never bump `VERSION`: unknown fields are ignored and absent ones
default, so a v1 reader survives a later producer.

### `<name>.mujoco.json` — the sidecar

A flat description of the compiled model's visual content. Every table's **index
is the MuJoCo id**, which is what lets a stream address nodes without a map:

| table | contents |
|---|---|
| `source` | filename, sha256, MuJoCo version — the fingerprint |
| `bodies` | parent, pose |
| `geoms` | kind, group, size, world pose at `qpos0`, mesh/material refs, rgba |
| `materials` | rgba, specular, shininess, reflectance, emission — MuJoCo's Phong-ish set, mapped onto PBR at import |
| `meshes` | name + the GLB node that holds the geometry |
| `sites` | massless marker frames, in their own id space |
| `tendons` | group, width, rgba, `max_waypoints`, initial waypoints |
| `flexes` | deformables: dim, radius, vertex count, vertex→body map, mesh ref |

`validate()` checks every index reference. That matters more than it sounds: a
dangling index produces a sidecar that parses cleanly, renders something, and
binds the stream to nothing.

### `<name>.capture.json` — a recorded run

A sequence of stream frames dumped verbatim, plus the source fingerprint.
Interchange only — the editor bakes it into ordinary animation clips and the raw
file is disposable, like an `.obj` after import.

There is **no trajectory asset type**. A baked capture is a normal clip:
composable, scrubbable, bundle-playable, with a translation+rotation track per
bound node and keyframe reduction applied.

## Coordinates

MuJoCo is Z-up, right-handed, metres, with quaternions as `[w, x, y, z]`.

The renderer is Y-up. **The conversion happens exactly once**, as a rotation on
the sim instance's root node. Everything beneath it — geom poses, site poses,
tendon waypoints, contact markers a player draws — is written as a *local*
transform in raw MuJoCo world coordinates.

So there is no per-frame conversion math anywhere. Moving the instance root moves
the whole robot, and the stream never has to know.

## The pose sink

`awsm_renderer_scene_loader::mujoco` is the entire MuJoCo runtime surface.

```rust
// At load: walk the scene, resolve every instance's bindings.
let mut instances = mujoco::resolve_instances(&scene.nodes, &handles);

// Per frame, from wherever your poses come from:
mujoco::apply_geom_poses(&mut renderer, &instances[0], &poses)?;
```

`MujocoInstance` carries the resolved binding — `geom_id → TransformKey`, sized to
the model's full geom count, with `None` for geoms the scene chose not to render.
It is resolved by walking the loaded tree, never read from a stored map that could
drift from it.

### Channels

Each is `&[f32]`, addressed by the relevant MuJoCo id, and each is exactly the
layout an `mjData` read already produces — a producer never reshapes anything.

| channel | frame layout | notes |
|---|---|---|
| `apply_geom_poses` | `7 × geom_count`: `[px,py,pz,qw,qx,qy,qz]` | the only required one |
| `apply_site_poses` | `7 × site_count` | sites have their own id space |
| `apply_tendon_waypoints` | per tendon: a live count, then its full waypoint capacity as xyz triples | see below |
| `apply_body_poses` | `7 × body_count` | drives a flex's skin joints |

A mis-sized frame is an error, never a silent truncation: applying one would
drive every node past the mismatch from the wrong slot, which reads as a physics
bug rather than a protocol one.

**Scale is preserved.** A frame carries translation and rotation only, and the
sink read-modify-writes so authored scale survives — an ellipsoid geom is a unit
sphere scaled per axis, and overwriting scale would flatten it on frame one.

## Tendons

A tendon's waypoint count *changes at runtime* as it wraps and unwraps around
geometry, and a pose sink can only write transforms — it cannot create nodes. So
the importer mints the whole chain up front and hides the unused tail.

- **The pool bound comes from the compiled model, not a sampled frame.**
  `max_waypoints = 2 × tendon_num` is MuJoCo's own rule for sizing `wrap_xpos`.
  Sizing to the initial pose would look right and then run out of segments the
  first time a tendon wrapped.
- **Segments are cylinders, not the capsules MuJoCo draws.** Length lives in the
  node's Z scale, and a cylinder is the one shape that scales along its axis
  without distorting. At tendon widths the missing end-caps are invisible.
- **Fixed tendons get a slot but no pool.** A model's tendon list mixes spatial
  tendons with fixed ones — joint-coupling constraints with no path through
  space. They keep their index, because that index *is* the tendon id, but export
  `max_waypoints: 0`. Told apart by `wrap_type`, the compiled truth, not by a
  runtime waypoint count of zero.
- Segment visibility is **edge-triggered**, which is why this channel takes `&mut`
  on the instance while the others take `&`: hiding a mesh re-syncs the spatial
  index, so re-asserting it every frame would churn the BVH for nothing.

## Heightfields, flexes, skins

**Heightfields are baked to meshes at export.** MuJoCo's grid is static after
compile, so there is nothing dynamic to preserve — and baking at the exporter
means the editor, the scene format and the pose sink never learn what a
heightfield is. The bake emits the top surface plus a skirt down to the base
depth, so terrain reads as solid at a grazing angle.

**Flex surfaces are baked the same way**, into the geometry GLB with a synthetic
mesh entry, so a deformable is just another mesh asset downstream. Only the
*surface*: a 2D flex's elements already are triangles, but a 3D flex's are
tetrahedra whose visible boundary is the shell, and drawing the tets would fill
the inside with invisible faces. Vertices come from `mjData` in world space
rather than `flex_vert` (which is each vertex in *its own body's* frame), so the
node transform is identity — a deformable has no rigid frame to place it by.

**A flex deforms by linear blend skinning, exactly** — verified to the nanometre
against MuJoCo's own `flexvert_xpos` across a real simulation
(`tests/flex_skin.rs`). So it exports and imports as an ordinary **skinned
mesh**, driven by the bodies that move it, and no vertex ever crosses the wire.

One mechanism covers both kinds:

- **body-attached**: each vertex rides its own body, so it has ONE influence at
  weight 1 and the joint list is the vertex list.
- **interpolated**: every vertex is a fixed blend of a regular cage lattice.
  MuJoCo already stores each vertex's position inside its cell as normalized
  coordinates in `flex_vert0`, so the weights are products of numbers it
  computed: no cell search, no geometry of ours. The lattice-to-node mapping is
  *derived* from rest positions, because MuJoCo promises no ordering and a wrong
  guess yields a mesh that looks right at rest and inverts the moment it moves.
  Two orders exist, and they are the same code at different `order`:
  - `trilinear` — a 2×2×2 cage, eight influences, two glTF joint sets.
  - `quadratic` — a 3×3×3 cage, 27 influences, seven joint sets (the last has
    one spare slot at weight 0).

`joint_bodies` names the bodies in the skin's joint order; a stream drives them
through [`apply_body_poses`]. The interpolated case is the cheapest to drive, not
the hardest: eight joint matrices per frame deform 2,503 bunny vertices.

**A quadratic cage's weights go negative.** Its 1D basis is quadratic *Lagrange*
on nodes at `t = 0, ½, 1` — it interpolates the mid node, unlike Bernstein. The
two are indistinguishable while the cage stays affine, so only a *bent* cage
tells them apart (measured: Bernstein is 0.99 mm wrong on `bunny_quadratic.xml`,
Lagrange exactly zero). Lagrange dips to −⅛ outside the middle third, so these
weights partition unity without lying in `[0, 1]` — which is all linear blend
skinning actually requires, but it does mean **a quadratic flex's weights cannot
be quantized to unorm8**. `glb-export`'s `weights_fit_unorm8` guard keeps those
accessors at f32; without it the player-bundle export would clamp the negative
lobes to zero and deform subtly wrongly with no error anywhere.

A cage that is neither 8 nor 27 nodes, or that is degenerate along an axis (so
its nodes do not fill the lattice), is refused loudly rather than approximated.

**`mjSkin` is not supported, on purpose.** It is MuJoCo's legacy deformable:
`nskin` is 0 for every model shipped with 3.11 and every menagerie robot, and the
one model in the tree that defines one needs a plugin dylib. Flex is the
deformable that exists.

## Composing a world

The scene owns *composition* — which model instances exist, where their roots
sit, what the static colliders are. The sim process owns the physics.

The connection is the fingerprint. At import the instance records the source
model's filename and content hash, so a harness can match its loaded models to
scene instances and **fail loudly on mismatch** rather than driving the wrong
robot with individually plausible, collectively nonsense poses.

For a MuJoCo-side player, the recipe (demonstrated by the templates repo's
`physics-mujoco`) is:

1. match each model to its scene instance by fingerprint;
2. attach all instances into a single world with mjSpec, namespaced, each at its
   instance root's authored transform — the Y-up→Z-up conversion happens at this
   seam, the mirror of the pose path;
3. inject the scene's collider components as static geoms, so one authored floor
   is rendered by us and collided with by the sim;
4. each frame, feed poses to the sink.

Players are free to ignore all of this and feed the sink their own way. The only
contract is poses in, addressed through the resolved binding.

Static colliders only. Moving colliders map to MuJoCo mocap bodies but require
streaming poses *into* the harness per step, which is not implemented.

## Transforms of sim-bound nodes are locked

Geom, site and tendon nodes are stream-owned: the editor disables their gizmos.
The **instance root is user-placeable** — its transform is the model's initial
placement in the composed world. Users author placement at the instance level,
plus materials, lighting and surroundings; never individual geom poses.

## Collider physics params

`PhysicsParams` on a collider is a universal core plus an optional per-engine
block:

- **Universal**: sliding friction, restitution, collision layer/mask (encoded to
  MuJoCo's `contype`/`conaffinity`), density.
- **`mujoco` block**: torsional and rolling friction with `condim`, `solref` /
  `solimp`, contact `margin` / `gap`, geom `priority`.

MuJoCo has no restitution parameter — bounce lives in its soft-constraint model,
so the universal value maps onto `solref` only approximately. And engines combine
pairwise friction differently (MuJoCo takes the element-wise max modulo priority;
Rapier averages; Box2D uses the geometric mean), so identical authored values do
not produce identical contact behaviour across engines.

## Group visibility

MuJoCo's convention is that groups 0–2 are visible and higher groups are
collision or debug geometry. The importer honours it, and the instance records
which groups it imported.

This is not cosmetic: menagerie models split collision and visual meshes by
group, so ignoring it renders the robot's collision capsules instead of the
robot.

## Debug overlays live in the player

Contacts, contact forces, joint axes and inertia boxes are **dev tooling that
never enters this repo**, the editor, or the bundle format. They belong to the
only side with live sim data — the player.

The templates repo's `physics-mujoco` implements all four behind URL flags
(`?contacts`, `?joints`, `?inertia`) and is the worked example. Two shapes from
it are worth reusing:

- publish overlay data in the **same seqlock as the poses**, so an overlay can
  never draw contacts from a different step than the bodies they touch;
- for anything whose count varies per step, preallocate the region with a live
  count in the header — the same reason the tendon pool is preallocated.

`MujocoInstance::root_transform` is exposed so a player can parent its own
overlay nodes into the sim's frame and write raw MuJoCo world coordinates,
instead of re-deriving the convention rotation and drifting from it.

## Reference

### CLIs

```
awsm-renderer-mujoco-export <model> [-o DIR] [-n NAME] [-v]
```
Compiles a model and writes `<name>.mujoco.json` plus `<name>.glb`. Needs
libmujoco at runtime (`MUJOCO_DIR=/path/to/mujoco-3.11.0`).

```
awsm-renderer-mujoco-record <model> [-o DIR] [-n NAME]
                            [--seconds 3.0] [--fps 60] [--keyframe N]
```
Steps the model and writes `<name>.capture.json`. Sampling is on step counts, so
reruns are byte-identical; the sim always steps at the model's own timestep, and
`--fps` only controls how often a step is sampled.

### Editor commands (also MCP tools)

| command | effect |
|---|---|
| `ImportMujocoFromUrl { sidecar_url, reimport }` | `reimport: None` adds a new instance — importing the same model twice is never second-guessed, since composing two robots into one world needs exactly that. `Some(node)` refreshes an existing instance in place, keeping the user's placement and material assignments. |
| `ImportMujocoCapture { capture_url, instance, name, position_epsilon, rotation_epsilon }` | bakes a capture into a clip on that instance. Refuses a fingerprint mismatch. |
| `SetPhysicsParams { node, params }` | the collider block above. |

### Crates

| crate | role |
|---|---|
| `awsm-renderer-mujoco-sys` | runtime-loaded FFI to libmujoco (`publish = false`) |
| `awsm-renderer-mujoco-format` | the two seam formats, pure serde, wasm-safe |
| `awsm-renderer-mujoco-export-cli` | the two CLIs + the sidecar/GLB builders |
| `awsm_renderer_scene::mujoco` | the `MujocoComponent` on a node, plus capture→clip bake |
| `awsm_renderer_scene_loader::mujoco` | binding resolution + the pose sink |

### Adding a per-node field

`EditorNode` is not the only place a node field lives. A new one must be added in
**four**: `EditorNode`, `NodeSpec` (the editor's real serialization pivot), both
`to_editor_node` / `from_editor_node` conversions, and the editor's reactive
`engine::scene::Node`. Miss `NodeSpec` and every schema round-trip test still
passes while the field silently vanishes on save.

### Test scene

`examples/test-scenes/mujoco-capture/` exercises the whole pipeline with no
simulator in the loop: a checked-in sidecar and capture are replayed, baked, and
compared against a golden.
