# awsm-renderer-tangents

MikkTSpace tangent generation over plain geometry arrays. Pure CPU — no GPU, no
`web-sys`. A tiny, dependency-light crate so the wasm-only renderer can reuse the
one implementation AND get native test coverage of the algorithm.

## What's here

- `generate_tangents(positions, normals, uvs, indices) -> Option<Vec<[f32; 4]>>`
  — per-vertex `vec4` tangents (xyz direction + handedness `w`), matching the
  glTF `TANGENT` convention. Returns `None` for unusable inputs (mismatched
  lengths, fewer than one triangle, an out-of-range index, or MikkTSpace
  failing).

Per-corner tangents from MikkTSpace are accumulated into per-vertex sums (UV
charts that meet at a shared vertex can emit differing tangents) and resolved to
a deterministic per-vertex basis, including correct handedness flips for mirrored
UV charts.

## Why it's its own crate

One implementation shared by every caller that bakes tangents:

- the renderer's raw-mesh upload path (`awsm-renderer`'s `raw_mesh`), and
- the glb exporter/converter (`awsm-renderer-glb-export`'s `write_glb`, via
  `awsm-renderer-gltf-convert`).

(The `awsm-renderer-gltf` populate path still has its own byte-buffer variant
tuned to its attribute-map representation; folding it in here is a follow-on.)

## Companion crates

- `awsm-renderer-glb-export` — bakes `TANGENT` accessors using this crate.
- `awsm-renderer-meshgen` — produces the plain `MeshData` arrays this consumes.
