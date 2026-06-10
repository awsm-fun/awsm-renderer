# Mesh data, buffers, and the import/convert pipeline

This explains how mesh geometry flows from a glTF file to the GPU, why the editor
and the player take *different* front-ends to the *same* packer, and why the
player deliberately never touches the editor's `MeshData`. If you're wondering
"why does the player need to know about an editor concept?" — it doesn't; read on.

## The three representations

There are three distinct ways geometry exists in this codebase. Keeping them
separate is the whole point.

1. **glTF / glb data** — encoded accessors (interleaved/strided bytes in a binary
   chunk). This is the *transport* form: compact, self-describing, what we load
   and save. It is **not mutable** — you cannot sculpt a byte-packed vertex
   buffer in place.

2. **`MeshData`** (`awsm_meshgen::MeshData`) — plain geometry arrays:
   `positions: Vec<[f32;3]>`, `normals`, `uvs`, `colors`, `indices`. This is the
   *mutable* form. Editing operates here — you can move a vertex, repaint a
   color, recompute normals.

3. **The editor's editable *model*** — `MeshData` **plus** the modifier stack,
   per-vertex override layers, and edit history. This is the heavy, **editor-only**
   representation that makes a mesh authorable. The player has no concept of it.

## The asymmetry: editor vs player

The editor and the player want different things, so they take different paths to
the GPU — and that's correct, not an oversight:

```
editor:   glb ──decode──▶ MeshData ──▶ pack_mesh_buffers ──▶ GPU
player:   glb ──────────────────────▶ pack_mesh_buffers ──▶ GPU
```

- The **editor** decodes to `MeshData` because it needs the mutable form to edit.
  (Sculpting then re-packs the mutated `MeshData`.)
- The **player** never edits, so building a `MeshData` — let alone the editable
  *model* (3) — would be pure wasted work. It reads accessors straight into the
  packed GPU buffers. This is exactly what `populate_gltf` does today: it never
  constructs a `MeshData`.

### "Why not just standardize on `MeshData` everywhere?"

Because it would force the player to materialize an **editor-only** form it never
uses. The player thinking only in glb is the efficient choice — it skips an
allocation + copy per mesh and never carries editing machinery it can't act on.
`MeshData` is the editor's tool; the player's tool is the glb.

### "Then why does the player 'know about' the packer at all?"

It doesn't know about `MeshData` — both front-ends funnel into **one shared
packer**, `pack_mesh_buffers`. The player feeds it decoded accessor data; the
editor feeds it `MeshData`. **Same bytes out either way.** That single shared
packer is what guarantees an edited mesh in the editor and the exported glb of it
in the player produce *identical* GPU buffers — so the two paths cannot drift.

> Historical note: before the shared packer, the editor packed via
> `add_raw_mesh` and the player/foreign-glTF via `populate_gltf` — two
> independent implementations of the same byte layout. They drifted (transmission
> meshes rendered opaque, tangents went flat). Unifying them on `pack_mesh_buffers`
> removes the divergence *by construction*; the parity proptest is then just a
> cheap regression guard.

## The GPU buffer layout (what `pack_mesh_buffers` produces)

Two geometry streams, chosen per mesh by its material's pass:

- **Visibility geometry** (56 bytes / vertex, *exploded* — one record per triangle
  corner): `position(12) | triangle_index(4) | barycentric(8) | normal(12) |
  tangent(16) | original_vertex_index(4)`. Feeds the visibility-buffer opaque
  pipeline.
- **Transparency geometry** (40 bytes / vertex, *non-exploded*, shared via the
  index buffer): `position(12) | normal(12) | tangent(16)`. Feeds the forward
  transparent pass.

A mesh gets visibility geometry, transparency geometry, or — never *both* as a
duplicate of the same surface:

| Material pass        | Geometry built        |
|----------------------|-----------------------|
| opaque               | visibility only       |
| transmission / blend / mask | transparency only |

Emitting visibility geometry for a transparency-pass mesh would rasterize it into
the opaque buffer as a solid occluder *in front of* its own transmission — i.e. a
glass surface reads opaque. (This was a real bug; see the historical note.)

## Tangents

Most glTF assets ship **no** `TANGENT` attribute even when they have normal maps —
tangents are expected to be generated. We generate them with MikkTSpace
(`bevy_mikktspace`, pure CPU) when a material samples a normal map. The canonical
glb (below) **bakes** generated tangents so population is a dumb upload and the
generation is covered by pure-data tests; editing regenerates them.

## The convert pipeline (`awsm-gltf-convert`)

There should be **one** way to load a mesh. Foreign glTF is normalized to our
canonical form *before* any GPU work, by a pure-data converter (no browser, no
renderer — fully property-testable):

```
convert(bytes) -> CanonicalImport {
    glb,                  // canonical, geometry-only, tangents baked, primitives un-merged
    materials,            // extracted MaterialDefinitions (ours)
    images,               // texture image byte-blobs (upload is population's job)
    animations,           // extracted clips (ours)
    is_already_canonical, // true if the input already carried AWSM_format
}
```

- A glb carrying the **`AWSM_format`** extension (versioned) is already canonical
  (it came from our exporter) → passed through untouched. Anything else → stripped
  of materials/animations/cruft, tangents baked, normals ensured, **multi-primitive
  nodes preserved** (not merged — merging is lossy for per-primitive materials),
  and stamped with `AWSM_format`.
- This makes the round-trip **idempotent**: `convert(convert(x)) == convert(x)`,
  and re-importing our own export is a no-op on the geometry.

Both the editor and the player call `convert`. The editor additionally decodes the
canonical glb to editable `MeshData` (eager — imports are immediately editable,
which is safe precisely because the packer is shared) and writes the outputs to
disk on save. The player populates the canonical glb directly.

## Where the code lives

- `pack_mesh_buffers` — `packages/crates/renderer` (called by `add_raw_mesh*` and
  `renderer-gltf`'s vertex builders).
- `populate_gltf` — `packages/crates/renderer-gltf`.
- `convert` / `AWSM_format` — `packages/crates/awsm-gltf-convert`.
- `MeshData` — `packages/crates/meshgen`.
- Player load — `packages/crates/scene-loader` (`populate_awsm_scene`).
- Editor import — `packages/frontend/editor/src/engine/bridge/gltf.rs`.
