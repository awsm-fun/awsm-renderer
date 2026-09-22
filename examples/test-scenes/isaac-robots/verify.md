# verify: isaac-robots

Isaac Sim / Isaac Lab robots (USD) through the MuJoCo seam, with **no USD and no
simulator in the loop**. The fixtures under `fixtures/` were produced by
`awsm-renderer-isaac-export` from NVIDIA's public Isaac 5.0 assets (see
`docs/isaac.md` for the URLs and the exact commands); the editor imports them
with the ordinary `import_mujoco_from_url`.

| fixture | source | size |
|---|---|---|
| `panda.mujoco.json` + `panda.glb` | `Isaac/5.0/.../FrankaRobotics/FrankaPanda/franka.usd` (Performance variant) | 45 KB + 3.3 MB |
| `anymal.mujoco.json` + `anymal.glb` | `Isaac/5.0/.../ANYbotics/anymal_d/anymal_d.usd` | 55 KB + 2.0 MB |
| `textures/*.jpg` | ANYmal's 14 OmniPBR albedo maps, shipped byte-for-byte | 2.6 MB |

drive (replay author.js — the robot subtrees are import-minted, so re-run the
script rather than loading the project to re-drive it):
  1. Serve the suite: `task test-scenes` (:9084). The fixtures are fetched from
     `http://localhost:9084/isaac-robots/fixtures/`.
  2. Replay `examples/test-scenes/isaac-robots/author.js`: `new_project` →
     `import_mujoco_from_url` for `panda.mujoco.json` (wait for 44 children) and
     `anymal.mujoco.json` (wait for 38) → place the two instance roots
     (`set_transform`, keeping the -90° X convention rotation) → a grey floor
     plane → pinned `set_camera_orbit` → grid/gizmos off → `wait_render_settled`.
  3. Screenshot; compare with `golden.png`.
  4. Optional player-path check: `load_player_bundle` (bakes + reloads through
     `populate_awsm_scene`), re-pin the camera, screenshot — identical render.

expect:
  - Left: the **Franka arm** upright on the floor, white plastic links with
    dark-grey joint caps, a small **blue emissive** status strip on its base,
    the hand pointing down (Isaac's default configuration).
  - Right: **ANYmal-D** standing on four legs, feet on the floor, knees bent
    inward (its default "X" stance): **red** top/bottom shells, **black/dark
    navy** drives, hips and legs, a sensor head with a carry handle on top.
    It is TEXTURED: the **ANYbotics** wordmark on the shell's flank reads
    left-to-right (zoom in — a mirrored or upside-down word means the UV
    V-flip broke), bolt holes and panel seams on the drives, a red/yellow
    emergency stop on the back.
  - NO grey cylinders, boxes or spheres on ANYmal — those are its 26
    guide-purpose collider shapes, exported to hidden group 3.
  - Outliner: a `panda` root with 44 locked children (`panda_link3 (PlasticWhite)`,
    `panda_hand (PlasticWhite, subset_1)`, …) and an `anymal` root with 38 locked
    children (`base/mesh`, `LF_THIGH/visuals/drive/mesh`, …) — every name unique.
  - Soft contact shadows under both robots from the default directional light.

fail:
  - A robot lying on its side, or the floor vertical in the robot's frame — the
    Z-up → Y-up convention rotation is missing or doubled.
  - Parts scattered or stacked at the origin — the body-frame bake or a
    transform stack was composed wrong.
  - An all-white, all-grey or flat-red ANYmal — its albedo maps did not bind
    (the shells' colour comes ONLY from the texture: OmniPBR's constant is
    replaced by a bound `diffuse_texture`).
  - A mirrored or upside-down ANYbotics wordmark — the UV flip or a texture
    transform is wrong.
  - Magenta anywhere — a geom lost its material.
  - Visible collider primitives on ANYmal — the purpose → group mapping broke.
  - ANYmal floating well above the floor or sunk into it — the fixture or the
    root lift (0.6815 m) changed.
