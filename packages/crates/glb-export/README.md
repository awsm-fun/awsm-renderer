# awsm-renderer-glb-export

Scene-complete glTF/GLB **export** IR + writer for `awsm-renderer`. No GPU,
editor, or wasm dependencies — only `awsm-renderer-meshgen` (for the plain-data
`MeshData`) and `gltf-json` (the model used to *build* a glTF). That keeps the
whole writer natively unit-testable (`cargo test -p awsm-renderer-glb-export`).

## What's here

- `GlbScene` — the one-way bake target: a node forest plus scene-wide
  animations, skins, an image pool, and an environment slot. An editor project
  (procedural recipes + raw-edited meshes + imported models) is flattened to
  triangles + materials and handed here.
- `write_glb` — serializes a `GlbScene` to a self-contained `.glb` byte vector
  (referenced-only images embedded in the `BIN` chunk).
- `ExportNode` / `ExtraPrimitive` / `ExportMaterial` / `ExportLight` /
  `ExportCamera` / `ExportAnimation` / `ExportSkin` — the IR types, with builder
  helpers on `ExportNode`.
- `extract_*` / `reexport_clean*` — pull baked geometry back out of glb bytes and
  re-export a cleaned, geometry-only glb (the basis for `awsm-renderer-gltf-convert`).
- `assemble_bundle` — assembles a player bundle from export inputs.

## Scene-complete by design

The IR carries node hierarchy + transforms, meshes, materials, lights, cameras,
animations, and an environment slot up front — even though the standalone Phase-1
export path only populates mesh + material. The player-bundle publish path reuses
the exact same IR + writer for the whole-runtime bake, so the shape is not
mesh-only.

## Material policy (lossless, portable)

- Built-in **PBR** → real glTF metallic-roughness PBR.
- **Unlit** → `KHR_materials_unlit`.
- **Non-PBR** (custom WGSL / Toon / anything not glTF-representable) →
  `ExportMaterial::None`: the primitive is emitted with the `AWSM_materials_none`
  extension and **no** embedded material, so a re-import leaves the slot empty for
  scene-level resolution. The importer (`awsm-renderer-gltf`) recognizes the same
  token.
- **Textures are referenced-only**: the writer embeds exactly the images present
  in `GlbScene::images` — the caller adds only the images the *assigned* materials
  use, so reassigning a lighter material drops the heavy textures with no special
  "slim" flag.

## Companion crates

- `awsm-renderer-meshgen` — supplies the plain-data `MeshData` this crate re-exports.
- Tangents: only AUTHORED `TANGENT` attributes are carried verbatim; derived
  (MikkTSpace) tangents are NOT baked — the runtime population path generates
  them at load (gated on normal-map usage), so baking would be redundant.
- `awsm-renderer-gltf-convert` — builds on the `reexport_clean*` path to normalize foreign
  glTF into the canonical AWSM form.
