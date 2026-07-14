# fixtures/local/ — paid / local-only asset fixtures (never committed)

Files placed here are **git-ignored** (see `.gitignore` in this dir — only it and
this README are tracked). Use it for paid or otherwise unshareable assets needed
to verify features locally. Do not reference these paths from committed tests
that run in CI; they're for manual runs and the local dev harness only.

## Expected files — compression plan (`docs/plans/compression.md`)

Drop the **re-exported** robots here (meshopt + quantization, i.e. `gltfpack`
output — NOT the original Draco `-opt.glb`). Use exactly these filenames so the
plan / loop can find them:

| Filename | Source model | Required extensions |
|---|---|---|
| `police-meshopt.glb`   | ROBOT-FAB-POLICE   | `EXT_meshopt_compression`, `KHR_mesh_quantization`, `KHR_texture_basisu`, `KHR_texture_transform` |
| `astrabot-meshopt.glb` | ROBOT-FAB-ASTRABOT | (same) |

Re-export from the Blender sources (or run `gltfpack -i <src>.glb -o <name>.glb
-cc` for meshopt+quantization; keep KTX2/basisu textures). The Phase-1 meshopt
spike only needs **one** of these to prove the wasm decode path against real
`gltfpack` output; Phase 4 acceptance needs both.
