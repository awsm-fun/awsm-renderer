# awsm-renderer-gltf

glTF ingestion for `awsm-renderer`. Extracted from the renderer crate per the
editor-renderer overhaul plan so glTF becomes one consumer of the renderer's
public mesh-upload API rather than a privileged path with `pub(crate)` access
into renderer internals.

## What lives here

- `populate.rs` — entry-point `populate_gltf(&mut renderer, &mut cache, ...)`
  that walks a glTF document, uploads buffers + textures + materials, and
  records per-node bookkeeping in `GltfKeyLookups`.
- `buffers/` — per-primitive byte-pack helpers (visibility + transparency
  vertices, attributes, indices, morph targets, skins).
- `loader.rs`, `data.rs`, `cache.rs`, `error.rs` — fetch + decode glue.

## What changed during extraction

- `AwsmRenderer.gltf` field was removed. Callers now own a `GltfCache` and
  pass it explicitly to `populate_gltf`.
- `populate_gltf` is a free function in this crate (was previously
  `impl AwsmRenderer { pub async fn populate_gltf }`); callers call it like
  `awsm_renderer_gltf::populate_gltf(&mut renderer, &mut cache, ...)`.
- Pub-surface items in `awsm-renderer` that glTF needed but were
  `pub(crate)` got promoted to `pub`.
