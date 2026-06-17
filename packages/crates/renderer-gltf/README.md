# awsm-renderer-gltf

glTF ingestion for `awsm-renderer`. Extracted from the renderer crate per the
editor-renderer overhaul plan so glTF becomes one consumer of the renderer's
public mesh-upload API rather than a privileged path with `pub(crate)` access
into renderer internals.

## What lives here

- `populate.rs` — entry-point `populate_gltf(&mut renderer, gltf_data, opts)`
  that walks a glTF document, uploads buffers + textures + materials, and
  returns a `GltfPopulateContext` whose `key_lookups` records per-node
  bookkeeping in `GltfKeyLookups`.
- `ext.rs` — `AwsmRendererGltfExt`, the public extension trait that attaches
  `renderer.populate_gltf(...)` (and `populate_gltf_under` / `populate_gltf_with`)
  to `AwsmRenderer`.
- `buffers/` — per-primitive byte-pack helpers (visibility + transparency
  vertices, attributes, indices, morph targets, skins).
- `loader.rs`, `data.rs`, `error.rs` — fetch + decode glue.

## What changed during extraction

- `AwsmRenderer.gltf` field was removed; glTF state is no longer baked into
  the renderer.
- `populate_gltf` moved out of `awsm-renderer` into this crate. It is now
  re-attached to `AwsmRenderer` via the `AwsmRendererGltfExt` extension trait,
  so callers still write `renderer.populate_gltf(...)` after a
  `use awsm_renderer_gltf::AwsmRendererGltfExt;`.
- Pub-surface items in `awsm-renderer` that glTF needed but were
  `pub(crate)` got promoted to `pub`.
