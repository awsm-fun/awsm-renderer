# awsm-scene-loader

Loads an `awsm-scene` runtime bundle (`scene.toml` + `assets/`) into the
renderer. The parallel to `awsm-renderer-gltf`'s `populate_gltf`: that loads
*foreign* glTF, this loads *our* format. They share the same renderer core — glb
meshes in a bundle go through `populate_gltf`'s machinery, primitives regenerate
via `awsm-meshgen`, and our materials / clips bind on top.

## What's here

- `populate_awsm_scene(renderer, scene, assets, on_phase)` — the entry point.
  Loads the node hierarchy (transforms), primitive + glb + skinned meshes, lights
  (with shadow params), cameras, textures, custom-WGSL materials, and animation
  clips + the NLA mixer. Returns a `LoadedScene` of the inserted handles for later
  teardown.
- Submodules: `material`, `texture`, `dynamic` (custom-WGSL), `light`, `camera`,
  `animation`.

`assets` is an in-memory map of bundle-relative paths → bytes, so loading never
touches disk. `on_phase` reports each `LoadPhase` boundary (through pipeline
compile) for live progress; pass `|_| {}` to ignore it.

## Batched, phased load

The work runs as one phased pass, efficient for the player's typical "load a
bundle then render" case:

1. **Build materials** — lower every node's authored material and insert once, so
   meshes (including glb meshes via `GltfMaterialSource::Single`) reference a
   ready key instead of letting the glTF loader mint a throwaway default.
2. **Upload textures** — one batched `finalize_gpu_textures` for the whole scene.
3. **Upload meshes** — transforms + geometry (+ skins) + lights. The scene's
   animation clips + NLA mixer are then lowered against the per-node keys built
   in the prior phases (this step reports no separate `LoadPhase`).
4. **Compile pipelines** — one drive-to-ready for all materials + shadows, so the
   first frame draws everything rather than trickling pipelines across frames.

The headline consumer is the **round-trip test**: in the MCP-controlled browser
session, `export_player_bundle` → `populate_awsm_scene` → screenshot, compared
against the source render. The loader only LOADS clips; the consumer drives the
clock (a player's `update_animations`, or the editor round-trip's playhead pin).

## Companion crates

- `awsm-scene` — the runtime schema this loads.
- `awsm-renderer` / `awsm-renderer-gltf` — the renderer core and the shared glTF
  upload path it reuses.
