# Isaac Sim / Isaac Lab

Render NVIDIA Isaac Sim and Isaac Lab robots with awsm-renderer.

Isaac models are USD. We read them **offline**, with a native exporter, into the
exact seam the MuJoCo pipeline already uses — a `<name>.mujoco.json` sidecar plus
a geometry-only `<name>.glb`. From there nothing is Isaac-specific: the editor
imports it with `ImportMujocoFromUrl`, a player bundle carries it, and a running
sim drives it through the same pose sink. Read [`mujoco.md`](mujoco.md) first;
this page covers only what is different.

Nothing here touches a runtime or wasm crate. The exporter is a `publish = false`
tool, and `openusd` is its dependency alone, so a renderer or player that never
sees Isaac carries none of it.

## Status

| piece | state |
|---|---|
| USD → sidecar + GLB exporter (`awsm-renderer-isaac-export`) | done, tested — geometry, materials and texture maps |
| Isaac 5.0 Franka + ANYmal-D, imported in the editor | verified by eye and against Pixar's USD (every mesh's world bounds match to f32 precision) |
| player bundle of an Isaac robot | verified: `isaac-robots` test scene, player-tests 33/33 |
| scripted motion by forward kinematics (`awsm-renderer-isaac-record`) | done, tested — every moving joint of both robots agrees with its bodies |
| sim-side frame builder for Isaac Lab (`isaac_geom_frame.py`) | verified offline against the fixtures; **not yet run inside Isaac Lab** |
| live streaming from Isaac Lab | designed below, not built — Isaac Sim needs Linux/Windows + an RTX GPU |

## The pipeline

```
Isaac asset (USD: franka.usd + its referenced layers, props, textures)
        │  fetch_isaac_asset.py       (download the file tree)
        │  awsm-renderer-isaac-export (native, pure-Rust `openusd`)
        ▼
<name>.mujoco.json + <name>.glb      ← the MuJoCo seam, unchanged
        │  editor: ImportMujocoFromUrl
        ▼
a sim INSTANCE: one root (Z-up→Y-up) + one locked node per geom
        │  export_player_bundle
        ▼
player bundle ──► apply_geom_poses(renderer, instance, &frame)  ← Isaac Lab, per step
```

Two facts make the seam fit with no format change:

- **Isaac Sim is Z-up, metres, quaternions `[w, x, y, z]`** — exactly MuJoCo's
  convention. The single convention rotation on the instance root covers both.
  The exporter refuses any other stage (`upAxis`/`metersPerUnit`), because a
  stage in another convention would need its sim's pose stream converted too.
- **Isaac Lab reports per-body world poses** (`Articulation.data.body_link_pose_w`,
  `(N, B, 7)`, w-first). The renderer does no kinematics; it needs world poses,
  and that is what the sim already has.

## Getting the assets

Isaac assets live on NVIDIA's public S3 bucket, no login or Nucleus needed:

```
https://omniverse-content-production.s3-us-west-2.amazonaws.com/Assets/Isaac/<version>/Isaac/...
```

Versions 4.5, 5.0, 5.1, 6.0 and 6.1 are live. The two exercised here (Isaac 5.0):

| robot | path under `.../Assets/Isaac/5.0/Isaac/` |
|---|---|
| Franka Panda | `Robots/FrankaRobotics/FrankaPanda/franka.usd` |
| ANYmal-D | `Robots/ANYbotics/anymal_d/anymal_d.usd` |

Isaac Lab's own robot configs (`isaaclab_assets`) point at
`<version>/Isaac/IsaacLab/Robots/...` — `ANYbotics/ANYmal-{B,C,D}/`,
`Unitree/{A1,Go1,Go2,H1,G1}/`, `Classic/Cartpole/cartpole.usd`, … — and Isaac Sim
6.1 moved some robots to `Isaac/Robots_Multiphysics/`. Those were not exercised,
but they are the same USD vocabulary.

An asset is a **tree** of files (the Franka is 27 USD layers + textures, 39 MB),
so fetch the whole tree:

```sh
pip install usd-core
python packages/tools/isaac-export-cli/fetch_isaac_asset.py \
  https://omniverse-content-production.s3-us-west-2.amazonaws.com/Assets/Isaac/5.0/Isaac/Robots/FrankaRobotics/FrankaPanda/franka.usd \
  /tmp/isaac/franka
```

It follows every `@asset@` reference, including textures. `OmniPBR.mdl` 404s
and is skipped on purpose: it is a built-in Omniverse module, and the exporter
never needs it (see Materials).

One asset quirk worth knowing: the 5.0 Franka's `Mesh=Quality` variant composes
its logo decals and cable meshes twice — one copy offset and rotated off the
base, one unscaled 1 mm copy of the decal. Pixar's own USD composes it
identically, so the export is faithful; the default `Performance` variant has
no such copies.

## Exporting

```sh
cargo run --release -p awsm-renderer-isaac-export-cli -- \
  /tmp/isaac/franka/franka.usd -o out/ -v
```

```
awsm-renderer-isaac-export <root.usd> [-o DIR] [-n NAME]
                           [--variant [PRIM:]SET=VALUE]... [--include-hidden-geometry] [-v]
```

- `--variant Mesh=Quality` selects a variant on the default prim;
  `/panda:Gripper=None` targets another prim. Selections go in a generated
  session layer, and a variant that does not exist is an error, not a silent
  fallback.
- Geoms that USD does not draw (below) always keep their slot in the table; their
  geometry ships only with `--include-hidden-geometry`.
- `-v` prints the counts (textures included) and every lossy step (dropped
  inputs, approximated shapes, dangling material bindings).

The output name defaults to the stage's default prim (`panda`, `anymal`).

## What the exporter reads

It opens the stage with [`openusd`](https://github.com/mxpv/openusd), a pure-Rust
USD with full composition: sublayers, references, payloads, variants and
instancing. Every Isaac asset puts each link's visuals under an **`instanceable`**
prim, and a default USD traversal stops at instances — so the exporter walks
instance proxies, or it would find no meshes at all.

| USD | sidecar |
|---|---|
| prim with `PhysicsRigidBodyAPI` | a **body**, named by its prim name (= Isaac Lab's `body_names`); body 0 is a synthetic `world` |
| `Mesh` | one **geom** per material `GeomSubset` (plus one for faces no subset claims) |
| `Cube` / `Sphere` / `Cylinder` / `Capsule` / `Plane` | a primitive geom in MuJoCo's size terms; a `Cone` is approximated as a cylinder and reported |
| `purpose = guide` or `proxy`, or `invisible` | geom **group 3**, hidden by the importer (MuJoCo's convention: 0–2 visible) |
| bound material | a sidecar **material** (below) |
| anything else | ignored: lights, cameras, render settings, the `OmniverseKit_*` editor cameras |

**Why purpose is the visibility rule.** It is USD's own answer to "is this drawn",
the analogue of MuJoCo's groups. It is also the only rule that works on both
assets: ANYmal's 26 collider primitives are `guide`, while the Franka's collision
*meshes* are its visual meshes (they carry `PhysicsCollisionAPI` and purpose
`default`). Filtering on `PhysicsCollisionAPI` would delete the Franka.

**Mesh geoms are baked into their body's frame.** Each mesh's transform relative
to its rigid body — including any scale; the Franka's meshes are authored in
centimetres under a `0.01` scale — goes into the vertices, so the geom's
`pos`/`quat` are the identity and its world pose *is* its body's pose.
Primitive geoms keep a real body-relative offset, because their size is a
parameter rather than vertices.

**Geometry:** polygons are fan-triangulated; `normals` / `primvars:normals` and
the first UV set (`st`, `st_0`, …) are read at any interpolation, indexed or
not; vertices are welded by value (face-varying normals otherwise triple the
vertex count); a left-handed or mirrored mesh has its winding flipped; missing
normals are generated. Identical baked meshes (a quadruped's matching parts) are
stored once.

**Names** are the prim name, lengthened with ancestor names only where needed to
be unique — ANYmal has 38 prims literally called `mesh`, which become
`base/mesh`, `LF_THIGH/visuals/drive/mesh`, …

### Materials

Isaac assets author **MDL** materials, almost always NVIDIA's `OmniPBR`, with no
`UsdPreviewSurface` fallback. We never need the `.mdl`: OmniPBR is a
metallic-roughness model whose inputs are plain named attributes, and the
exporter reads them with OmniPBR's own defaults (from `OmniPBR.mdl` v2.1) for
anything left unauthored.

| shader | base colour | roughness / metallic | normal | opacity | emission |
|---|---|---|---|---|---|
| `OmniPBR` | `diffuse_texture` (replaces the constant) else `diffuse_color_constant`; × `diffuse_tint` × `albedo_brightness` | `reflectionroughness_texture` / `metallic_texture` lerped over their constants by `*_texture_influence` (default 0 = map ignored, as in OmniPBR); or `ORM_texture` | `normalmap_texture` × `bump_factor` | `enable_opacity` + `opacity_texture` (by `opacity_mode`) or `opacity_constant`; `opacity_threshold` > 0 = cutout | `enable_emission`: `emissive_color` or `emissive_color_texture`, × `emissive_mask_texture` |
| `UsdPreviewSurface` | `diffuseColor` (value or `UsdUVTexture`, × its `scale`) | `roughness` / `metallic` (value or any texture channel) | `normal` | `opacity` (value or channel); `opacityThreshold` > 0 = cutout | `emissiveColor` |
| `OmniSurface` | `diffuse_reflection_color` (or its image) × `diffuse_reflection_weight` | `specular_reflection_roughness` / `metalness` | — | `geometry_opacity` | `emission_weight` > 0 → `emission_color` |
| `OmniGlass` | `glass_color`, alpha 0.3, blended | 0.05 / 0 | — | — | — |

The sidecar material has glTF semantics, and the importer maps it as
`roughness = 1 - shininess`, `metallic = reflectance`; the exporter writes the
exact inverse. Occlusion (`ao_texture`, or ORM's red channel) is exported only
when `ao_to_diffuse` > 0, which is OmniPBR's own switch for it. Unbound
geometry uses `primvars:displayColor`. Identical materials — Isaac repeats each
look per link file — are deduplicated, so the Franka's 32 bindings become 10
library materials and editing "PlasticWhite" repaints the whole arm.

**Textures.** Where one USD image IS a glTF slot, it ships byte-for-byte (a
JPEG stays a JPEG). Where glTF packs channels USD keeps apart, the exporter
packs a PNG:

- **metallic-roughness**: roughness → G, metallic → B, each already blended
  with its constant by its influence, so both factors are 1;
- **base colour + opacity**: the opacity map, reduced by `opacity_mode`
  (average, luminance, maximum or alpha), goes into A — unless colour and
  opacity are the same RGBA file, which ships untouched;
- **normal**: OmniPBR's default tangent flips (`flip_tangent_v = true`) read an
  OpenGL-convention (+Y) map, which is glTF's; a non-default flip inverts that
  channel.

Images land in `textures/<name>-<hash>.<ext>` beside the sidecar,
content-addressed, so two robots exported into one directory share identical
images and never overwrite different ones. In the editor they become ordinary
texture assets — deduplicated by content, so a re-import or a second instance
adds none — uploaded with each slot's colour space, persisted on save, and
compressed into the player bundle like any other texture.

**UV transforms.** A `UsdTransform2d` (and OmniPBR's `texture_scale` /
`texture_rotate` / `texture_translate`) becomes a glTF `KHR_texture_transform`.
The GLB's V axis runs top-down (the exporter flips it, as glTF expects), so the
transform is conjugated by that flip: scale and rotation angle carry over, the
offset becomes `(t_u − sinθ·s_v, 1 − t_v − cosθ·s_v)`. `UsdUVTexture`'s
`wrapS` / `wrapT` carry over (`black` approximated as clamp).

**UV sets.** A material may sample more than one UV set: OmniPBR picks one with
`uv_space_index` (set `n` is the mesh's `primvars:st{n}` or `st_{n}`), and a
UsdPreviewSurface texture names one through its `UsdPrimvarReader_float2`
`varname`. Each material records the sets it samples, set 0 first, and every
mesh piece carrying it writes exactly those as `TEXCOORD_0..n`; a texture's
sidecar `uv` is its index into that list, so it means the same on every mesh
the material is on. A set the mesh lacks falls back to set 0, reported.

**What does not carry,** each reported by `-v`: OmniPBR detail and clearcoat
normal maps, `albedo_desaturation` / `albedo_add`, `project_uvw` world-space
projection, `emissive_intensity` (the
renderer has no physical light units; the colour ships at full strength), and a
`UsdUVTexture` bias on anything but a normal map.

### The fingerprint

`source.filename` + `source.sha256` are the **root layer** file, which is what a
harness loads, so it can reproduce the hash with one line. Referenced layers are
not folded in — matching them would make the harness re-implement composition.
`source.mujoco_version` reads `none (USD, awsm-renderer-isaac-export)`: the field
names the compiler that produced the model, nothing compares it, and so the
sidecar format needed no change.

## Placing a robot

The sidecar's poses are the **asset's** default pose. Isaac Lab usually spawns a
robot above its asset origin (ANYmal's origin is its base, so its feet are
0.68 m below it). Place the instance root where the robot stands in your world,
as for any sim instance; `examples/test-scenes/isaac-robots/author.js` lifts
ANYmal by 0.6815 m, its lowest foot vertex.

## Moving without a simulator

`awsm-renderer-isaac-record` makes a robot move with no Isaac at all. It reads
the stage's UsdPhysics joints — revolute, prismatic and fixed; their two body
frames, axis and limits (degrees in USD) — into a tree over the sidecar's
bodies, sweeps every movable joint around its rest position, runs forward
kinematics, and writes the geom poses as a `<name>.capture.json`: the same
capture format, and the same fingerprint, a live stream records.

```sh
cargo run --release -p awsm-renderer-isaac-export-cli --bin awsm-renderer-isaac-record -- \
  /tmp/isaac/franka/franka.usd -o out/ -v    # --seconds 8 --fps 30 --amplitude 0.4 --cycles 2
```

- **Rest positions are solved, not assumed.** Assets do not reliably store
  joint positions (ANYmal authors none), so each joint's rest position is the
  relative motion between its two frames at the authored pose, projected on
  its axis. FK at rest reproduces the export exactly, so playback starts where
  the import is. Where an asset's joint frames disagree with its bodies — the
  ANYmal's fixed foot joints sit 3 cm from the foot bodies — the authored pose
  is kept and the joint reported.
- **The sweep** stays within each joint's limits (≤ 45° or the slider's
  travel, times `--amplitude`, on each side of rest), gives joints golden-angle
  phases so none move in lockstep, and runs a whole number of cycles per loop,
  so the capture loops without a seam. A rest pose outside its limits (the
  Franka's joint 4 is authored past its stop) only swings back toward them.
- **It is joint animation, not physics:** nothing collides, nothing falls, and
  a floating base (a quadruped's) stays where it was authored.

The capture replays through the pose sink exactly like a live stream (a player
loops its frames into `apply_geom_poses`), or bakes into an ordinary clip in
the editor (`ImportMujocoCapture`).

## Streaming from Isaac Lab

The contract is the MuJoCo one: one frame of `7 × geom_count` floats,
`[px, py, pz, qw, qx, qy, qz]` per geom in the model's own geom order, applied
with `apply_geom_poses`. Isaac Lab reports **bodies**, so the harness scatters:

```
geom world = body world ∘ (geom.pos, geom.quat)     (identity for mesh geoms)
```

A geom whose body the sim does not report keeps its rest pose (`world_pos`,
`world_quat`). This composition is pinned by the exporter test
`a_body_pose_frame_reproduces_every_geom_pose`, on the fixture and on both real
robots: at rest it lands exactly on the imported pose, so the first streamed
frame continues without a jump.

`packages/tools/isaac-export-cli/isaac_geom_frame.py` is that scatter, in numpy
only, so it drops into an Isaac Lab process:

```python
from isaac_geom_frame import GeomFrame

robot = env.scene["robot"]                                      # isaaclab.assets.Articulation
frame = GeomFrame("panda.mujoco.json", robot.data.body_names)   # maps Isaac body order by name

def payload(env_id=0):
    pose = robot.data.body_link_pose_w[env_id].cpu().numpy()    # (B, 7), world, wxyz
    pose[:, :3] -= env.scene.env_origins[env_id].cpu().numpy()  # env-local
    return frame(pose)                                          # 7 x geom_count little-endian f32
```

Send `payload()` each step over any transport (a WebSocket is the obvious one;
Isaac Lab has no built-in scene-data stream — `--livestream` is video only, and
the ROS 2 bridge publishes TF, which would need the same scatter on the far
side). On the player: a `Float32Array` view of the bytes → `apply_geom_poses`.

Things a harness must get right:

- **Match the model first.** Hash the root USD the sim loaded and compare with
  the instance's `source.sha256`; refuse to drive on a mismatch, as for MuJoCo.
- **Environment origins.** Isaac Lab clones N environments in one world;
  `body_link_pose_w` includes each env's offset. Subtract it (as above) to drive
  one instance, or place one instance root per env origin to show them all.
- **Body names are prim names**, which is what `data.body_names` returns; a sim
  body with no sidecar body (an extra rigid prop) is simply ignored.

What was verified: the frame builder, offline, against both fixtures, with the
Isaac body order deliberately shuffled. What was not: running it inside Isaac
Lab, which needs a Linux or Windows machine with an RTX-class GPU (Isaac Sim 6.1:
Ubuntu 22.04/24.04 or Windows 11, RTX 4080-class, driver ≥ 580;
`nvcr.io/nvidia/isaac-sim:6.1.0` runs headless). Isaac Sim does not run on macOS.

## Not done, and what each would take

| gap | what it would take |
|---|---|
| **Live streaming test** | a Linux/RTX box (or cloud instance) with Isaac Lab; wire `isaac_geom_frame.py` into an env loop behind a WebSocket; a player template like `physics-mujoco` with the sim replaced by that socket |
| **Recorded captures from Isaac Lab** | dump `payload()` per step into a `<name>.capture.json`; `ImportMujocoCapture` then bakes it into a clip exactly as for MuJoCo — no renderer work |
| **`UsdSkel` / deformables** | not used by the Isaac Lab robots; would map onto the existing flex → skinned-mesh path |
| **`PointInstancer`** | not used by robot assets; needed for Isaac *scenes* (warehouses, props) |
| **Isaac Lab scenes** (terrains, props, lights) | out of scope here: robots are sim instances; a static scene is better imported as geometry through the ordinary glTF path |
| **Time-sampled transforms** | read at frame 0 today; not used by robot assets |
| **Y-up or non-metre stages** | refused today; supporting them means converting the sim's pose stream the same way |
| **Prebuilt binaries** | the CLI is pure Rust with no native deps, so unlike the MuJoCo exporter it *could* opt into cargo-dist |

## Alternatives considered

- **Python + `usd-core`** — Pixar's reference implementation, verified to read
  these assets. Rejected for the shipped tool only because a Rust exporter fits
  the workspace, the GLB writer and the tests; it is what the download script
  and the offline cross-checks use.
- **Blender** — imports the geometry, but ignores MDL (every material comes out
  default grey), suffixes names (`panda_link4.001`) and emits instanced
  prototypes at the root, breaking the body mapping.
- **Omniverse `asset_converter`** — exports glTF from OmniPBR, but only inside
  Kit, which needs Isaac Sim's platform.
- **MuJoCo Menagerie** — most Isaac Lab robots (Panda, ANYmal B/C, Unitree
  A1/Go1/Go2/H1/G1, Shadow, Allegro) have Menagerie MJCF that the existing
  MuJoCo exporter already renders. Those are look-alikes, though: different
  meshes, materials and body ids from the Isaac asset a sim is running.

## Reference

| path | what |
|---|---|
| `packages/tools/isaac-export-cli/` | the exporter (`stage`, `geometry`, `material`, `texture`, `primitive`, `lib`) and the recorder (`kinematics`, `src/bin/record.rs`) |
| `packages/tools/isaac-export-cli/tests/` | `fixtures/robot.usda` (two bodies, a hinge, every export rule) with `export.rs`, `kinematics.rs`; `textures.rs` generates its own images |
| `packages/tools/isaac-export-cli/fetch_isaac_asset.py` | downloads an asset tree |
| `packages/tools/isaac-export-cli/isaac_geom_frame.py` | Isaac Lab body poses → geom frame |
| `examples/test-scenes/isaac-robots/` | Franka + ANYmal-D fixtures, project, bundle, golden, `verify.md` |

The real-asset test is `#[ignore]`d (the assets are tens of MB); after
downloading them:

```sh
AWSM_ISAAC_FRANKA=/tmp/isaac/franka/franka.usd \
AWSM_ISAAC_ANYMAL_D=/tmp/isaac/anymal_d/anymal_d.usd \
  cargo test -p awsm-renderer-isaac-export-cli -- --ignored
```
