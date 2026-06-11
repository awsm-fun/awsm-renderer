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

| Material pass            | Geometry built    |
|--------------------------|-------------------|
| opaque / **mask**        | visibility only   |
| transmission / blend     | transparency only |

`mask` (glTF `alphaMode = MASK`) is alpha-tested **opaque**, so it takes the
visibility path (see "Masked materials" below) — *not* the transparency path.
Only order-dependent **blend** and framebuffer-sampling **transmission** are
transparency.

Emitting visibility geometry for a transparency-pass mesh would rasterize it into
the opaque buffer as a solid occluder *in front of* its own transmission — i.e. a
glass surface reads opaque. (This was a real bug; see the historical note.)

## Masked (alpha-tested) materials

A `MASK` material is opaque with a per-fragment cutout: fragments whose alpha is
below the material's `alphaCutoff` are discarded so the cutout is see-through.
Because it's opaque, it lives in the visibility path — it lands in `opaque_tex`
(so transmission samples it), casts shadows, and is deferred-shaded — and the
cutout is applied by a **masked variant of the visibility raster** (a per-
`shader_id` pipeline that only masked meshes pay for): it reconstructs the
fragment UV from the merged geometry pool, samples the masking alpha (PBR
base-color alpha, or a custom material's *alpha-only* WGSL), and rejects sub-
cutoff fragments. The discard must happen in the **raster** (not the later
opaque compute) because the raster writes depth — a hole has to write *no* depth
so geometry/shadows/transmission behind it show through.

### MSAA-anti-aliased cutout edges (without TAA)

A binary `discard` kills all of a pixel's MSAA samples at once, so the cutout
boundary gets no sub-pixel coverage — classic jaggy alpha-test edges. Forward
renderers fix this with hardware **alpha-to-coverage**, but that's unavailable in
a visibility-buffer pipeline: the geometry pass writes integer visibility IDs
(`RGBA16Uint`), not a float color the hardware can convert. So deferred /
visibility-buffer renderers normally fall back to **TAA** for cutout AA, with its
ghosting and blur.

Instead, the masked raster emits a **`@builtin(sample_mask)`** of true
per-sample coverage: it evaluates the masking alpha at each of the MSAA sample
sub-positions (offset from the pixel center via the barycentric screen-space
derivatives) and sets that sample's bit when it passes the cutoff. A boundary
pixel writes *k of N* samples as the surface and leaves the rest as the cleared
skybox sentinel, so the existing **compute MSAA edge-resolve** (which already
shades per-sample at silhouette edges) blends them. The result: **crisp,
temporally-stable, ghost-free anti-aliased alpha cutouts under true MSAA in a
deferred pipeline** — a property most visibility-buffer/deferred renderers don't
have (they lean on TAA).

Per-*sample* evaluation (rather than deriving coverage analytically from
`fwidth(alpha)`) is what makes it robust: it works for **any** alpha, including a
**binary/procedural** cutout (`select(1.0, 0.0, …)`) whose alpha is a hard step
with no usable gradient. Zero coverage still discards (a true hole writes no
depth); single-sampled falls back to the binary discard (where a post-process AA
would complement it). Cost is scoped to the MSAA-on × cutout-material combo —
opaque non-cutout materials never enter this path.

### Hole-shaped (cutout) shadows

A cutout caster has to cast a *cutout* shadow — the holes must let light through,
not a solid rectangle. The shadow pass is depth-only with no fragment, so it gets
a **parallel masked variant** (`render_passes/shadow_masked/` +
`shadows/shader/masked_*`): a masked shadow vertex forwards the
triangle-index/barycentric/material-meta offset, and a depth-only fragment reuses
the *same* `shared_wgsl/masked_alpha.wgsl` helper as the geometry masked fragment
to `discard` sub-cutoff fragments (binary discard — the shadow atlas is
single-sampled, so there's no `sample_mask`; PCF/PCSS softens the edge at sample
time). To stay within the macOS `maxBindGroups = 4` ceiling the variant augments
**group 0** (the per-view `shadow_view` uniform) with the fragment-only material /
mesh-pool / texture-pool bindings — mirroring the geometry masked group-0
augmentation — so the vertex's transforms/meta/animation groups (1–3) are
untouched. The lazy pipeline pool (keyed `shader_id × instancing × cube_face`)
builds in the texture-finalize flow alongside the geometry masked pipelines, and
the shadow render pass binds the augmented group + masked pipeline for any caster
whose material has an `alpha_cutoff` — **independent of opaque/transparent
routing**, since a `MASK`+refractive material is transparent-routed but must still
cast a hole-shaped shadow. Until a caster's masked variant is compiled it falls
back to the plain solid-shadow pipeline, so a cutout mesh always casts *some*
shadow.

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
