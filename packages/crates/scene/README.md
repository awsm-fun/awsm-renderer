# awsm-renderer-scene

The lean, canonical **runtime** scene schema for the awsm-renderer player: a
`Scene` (`scene.toml`) plus an `assets/` directory, all referenced by id. The
player and renderer touch only this crate.

Coordinate convention: right-handed, Y-up, meters. Rotations are unit quaternions
stored as `[x, y, z, w]`.

## What's here

The on-disk types for everything a baked scene contains, each in its own module
and re-exported at the crate root:

- Structure — `scene`, `tree`, `transform`, `instances`, `project_dir`.
- Renderables — `mesh`, `primitive`, `material`, `dynamic_material`, `sprite`,
  `line`, `decal`, `particle`.
- Stage — `light`, `shadows`, `camera`, `environment`.
- Motion / misc — `animation`, `curve`, `collider`.
- `assets` — the by-id asset table (`scene.toml` + `assets/<id>.*`).

## What's not here

Authoring. The modifier stack, per-vertex overrides, and the editor's
`Mesh = base + edits` model live in `awsm-renderer-meshgen` (recipe types) and
`awsm-renderer-editor-protocol` (the `EditorProject` document + `EditorCommand` /
`EditorQuery`), which depend on this crate and reuse its core types. The editor's
bake step lowers authoring → runtime (`MeshDef` → `mesh::MeshBlob`).

## Companion crates

- `awsm-renderer-scene-loader` — loads a runtime bundle described by this schema into the
  renderer.
- `awsm-renderer-editor-protocol` — the authoring layer that bakes down to this schema.
