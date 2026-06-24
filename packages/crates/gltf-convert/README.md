# awsm-renderer-gltf-convert

Pure-data glTF → canonical AWSM-format normalizer. No GPU, no browser, no
renderer — it runs entirely on bytes, so it's exhaustively property-testable
(`cargo test -p awsm-renderer-gltf-convert`). This is the single import/convert path
shared by the editor and the player.

## What's here

- `convert(bytes) -> CanonicalImport` — normalizes arbitrary glTF/GLB bytes:

  ```text
  foreign glTF ──convert──▶ self-contained canonical glb (geometry + materials
                            + textures, AWSM_format-stamped)
                            + extracted material / animation / image specs
  our own glb (AWSM_format) ──convert──▶ passed through unchanged
  ```

- `CanonicalImport` — the result: the canonical glb bytes plus the
  material/animation/image data lifted out of the source into neutral specs.
- `extract_materials` / `extract_animations` / `extract_images` and their spec
  types (`MaterialSpec`, `AnimationSpec`, `ImageData`, plus the KHR extension
  shapes — `Clearcoat`, `Sheen`, `Volume`, `Iridescence`, …).
- `is_canonical` / `stamp_awsm_format` / `awsm_format_version` — the
  `AWSM_format` marker helpers.

## Idempotent by design

The `AWSM_format` document extension makes the round-trip idempotent: converting
our own export is a no-op (`convert(convert(x)) == convert(x)`). The marker
carries a version so a future canonical-form change is detectable rather than
silently mis-read.

The converter parses *without* decoding images and ships raw image bytes, so it
never depends on an image decoder accepting them — and it skips the decode cost.

## Companion crates

- `awsm-renderer-glb-export` — supplies `reexport_clean_scene` / `write_glb`, the
  geometry re-export this crate wraps.
- `awsm-renderer-gltf` — the GPU-side ingestion path that loads a (canonical) glb
  into the renderer.
