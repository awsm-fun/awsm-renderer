# awsm-meshgen

Pure-CPU mesh + texture-pixel generators. No `awsm-renderer` dep.

## What's here

- `MeshData { positions, normals, uvs, indices, colors }` — plain data, the renderer's raw-mesh API input.
- Primitives: `plane`, `box`, `sphere`, `cylinder`, `cone`, `torus`, sprite quad.
- `sweep_along_curve` with cross-section variants: `Strip`, `Tube`, `Wall`, `Profile(Vec<Vec2>)`.
- Procedural texture helpers: `checker`, `gradient`, `noise`. Output is raw RGBA bytes — texture *upload* is the renderer's job.

## What's not here

- Materials. `MaterialDef` lives in `lockstep-game-data` because it references lockstep `AssetId`s. This crate has no material type.
- GPU buffer types. `MeshData` is `Vec<f32>` / `Vec<u32>` only.

## Companion crates

- `awsm-curves` — sweep generators take a `Curve3`.
- `awsm-geometry` — primitives use AABB for bounds computation.
