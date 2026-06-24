# awsm-renderer-meshgen

Pure-CPU mesh + texture-pixel generators. No `awsm-renderer` dep.

Feature-gated: `primitives` + `mesh_data` (glam-only) always compile, so a plain
`awsm-renderer-meshgen` dep is the player-lean build. Recipe **types** opt in via `recipes`;
the modifier/SDF/sweep/edit/texture **execution** + heavy deps opt in via `authoring`.

## What's here

- `MeshData { positions, normals, uvs, colors, indices }` — plain data, the renderer's raw-mesh API input. `normals`/`uvs`/`colors` are `Option`; positions are `[f32; 3]`, indices `u32`. Includes `compute_vertex_normals`.
- Primitives (always available): `plane`, `box`, `sphere`, `cylinder`, `cone`, `torus`, `sprite_quad`, plus `primitive_mesh` dispatching a scene `PrimitiveShape`.
- `sweep_along_curve` with cross-section variants: `Strip`, `Tube`, `Wall`, `Profile { points, closed }`. (`authoring`)
- Modifier stack + recipe eval: `apply_modifiers`, `lathe`, `superquadric`, `evaluate`, plus mesh `edit`/selection, `expr`, and `stats` (`mesh_stats`, `cross_section_profile`). (`authoring`)
- SDF → triangles via surface nets (`sdf`, `sdf_mesh`). (`authoring`)
- Procedural texture helpers: `checker_rgba`, `gradient_rgba`, `noise_rgba`. Output is raw RGBA bytes — texture *upload* is the renderer's job. (`authoring`)

## What's not here

- Materials. The runtime material schema (`MaterialDef`) lives in `awsm-renderer-scene`; authoring/recipe wiring is in `awsm-renderer-editor-protocol`. This crate has no material type.
- GPU buffer types. `MeshData` is plain `Vec`s only.

## Companion crates

- `awsm-renderer-curves` — `sweep_along_curve` takes a `Curve3`. (`authoring`-only dep)
- `awsm-renderer-geometry` — bounds computation. (`authoring`-only dep)
- `awsm-renderer-scene` — `PrimitiveShape` input to `primitive_mesh`.
