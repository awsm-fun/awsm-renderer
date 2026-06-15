# Development Guide

## Getting Started

It's really as simple as:

```bash
task dev            # every frontend dev server + media servers in parallel
```

Or run just what you need:

```bash
task editor-dev     # the editor on http://localhost:9085 (+ media)
task model-tests-dev # the glTF test viewer on http://localhost:9080 (+ media)
task mcp-dev        # editor + the MCP server, for agent-driven control (see below)
task --list-all     # everything available
```

However, that assumes you have all the prerequisites installed and media assets downloaded.

## Media

For the sake of keeping the repo clean, media files are referenced remotely on the release build, and can be downloaded locally to gitignored directories for dev builds.

Currently, these need to be manually cloned/downloaded (not via git submodules):

1. https://github.com/KhronosGroup/glTF-Sample-Assets.git
  - cloned into media/glTF-Sample-Assets
2. https://github.com/awsm-fun/awsm-test-assets.git
  - assumed to exist in ../test-assets
  - served from https://cdn.awsm.fun/test-assets in release builds

## Prerequisites 

There are a few prerequisites to set up the development environment.

### Trunk

This is used to build and run the frontend demo. Kinda hard to develop if you can't see the output :)

See https://trunkrs.dev/ for more info 

### Taskfile

Used to simplify common tasks like building and running the demo

### Cmgen

cmgen from filament is used to create environment maps and IBL textures

1. clone https://github.com/google/filament
2. build and install: `./build.sh -i release` (make sure cmake, ninja, xcode, etc. are installed)
3. add path to global path: `export PATH="path/to/filament-repo/out/release/filament/bin:$PATH"`

### KTX tools

Used to re-package into ktx2 containers

Use the releases: https://github.com/KhronosGroup/KTX-Software/releases

## Project layout

Everything lives under `packages/`:

**Library crates** (`packages/crates/`)

* [awsm-renderer](../packages/crates/renderer): the renderer in all its glory
* [awsm-renderer-core](../packages/crates/renderer-core): wraps the WebGPU API with very little opinion — just a nicer Rust API
* [awsm-renderer-gltf](../packages/crates/renderer-gltf): glTF loading on top of the renderer
* [awsm-scene-schema](../packages/crates/scene-schema): the authored scene / project schema (pure data; shared by the editor + any runtime)
* [awsm-editor-protocol](../packages/crates/editor-protocol): the serializable command / query / transport types the editor and the MCP server share
* [awsm-materials](../packages/crates/materials), [awsm-meshgen](../packages/crates/meshgen), [awsm-curves](../packages/crates/curves), [awsm-geometry](../packages/crates/geometry), [awsm-particles](../packages/crates/particles): supporting libraries

**Frontends** (`packages/frontend/`, WASM via Trunk)

* [editor](../packages/frontend/editor): the unified scene / material / animation editor (`awsm-editor`). Absorbs what used to be the separate `scene-editor` + `material-editor` + `awsm-renderer-editor` gizmo/grid crate. Deployed to https://scene.awsm.fun.
* [model-tests](../packages/frontend/model-tests): the glTF feature-test viewer. Deployed to https://model-tests.awsm.fun.
* [web-shared](../packages/frontend/web-shared): shared UI / theme primitives + the viewport gizmo / grid / free-camera helpers

**Native tooling**

* [mcp](../packages/mcp): the `awsm-renderer-mcp` that drives the editor from an AI agent (see below)
* [debugging](../packages/debugging): native debugging binaries

**Other**

* [docs](.): documentation
* [media](../media): media assets for the demo scenes
* [taskfiles](../taskfiles): Taskfile includes — ports, dev recipes, build/deploy (`config.yml` is the single source of truth for ports)

## Driving the editor from an AI agent (MCP)

The editor can be driven by an MCP-capable agent (Claude Code, Codex, …) over a
WebTransport link. Start both sides with `task mcp-dev`, open
`http://localhost:9085/?mcp=http://127.0.0.1:9086`, and point your agent at the
`.mcp.json` in the repo root. Full details, the tool catalog, and the wire
protocol are in [docs/MCP.md](MCP.md). The renderer README's
"Driving the editor from an AI agent" section has the quick start.


# Create maps

Assuming you have some exr file from a site like PolyHaven

1. Create the raw EXR faces

High-res for skybox
```bash
cmgen -s 2048 -f exr -x skybox myHDR.exr
```

Or, for a simple PNG:
```bash
cmgen -s 2048 -f png -x skybox myimage.png
```

Lower res for specular (roughness-prefiltered faces) IBL
```bash
cmgen -s 512 -f exr --ibl-ld ibl-env myHDR.exr
```

Even lower res for diffuse irradiance faces IBL
```bash
cmgen -s 64 -f exr --ibl-irradiance ibl-irradiance myHDR.exr
```

After all these are done, you probably want to move the created subdirectories into the parent directories

2. Package as KTX2 ([GpuTextureFormat::Rg11b10ufloat](https://docs.rs/web-sys/latest/web_sys/enum.GpuTextureFormat.html#variant.Rg11b10ufloat) in webgpu jargon, B10G11R11_UFLOAT_PACK32 for the tool)

_if your EXRs come in flipped, use --convert-texcoord-origin top-left (rarely needed with cmgen output)_

Skybox (simple PNG, no mipmaps)

```bash

ktx create \
    --cubemap \
    --encode uastc --uastc 2 \
    skybox/px.png skybox/nx.png skybox/py.png skybox/ny.png skybox/pz.png skybox/nz.png \
    skybox.ktx2
```
Skybox (HDR EXR with mipmaps)

```bash

ktx create \
    --cubemap \
    --format B10G11R11_UFLOAT_PACK32 \
    --assign-tf linear \
    --assign-primaries bt709 \
    --generate-mipmap \
    skybox/px.exr skybox/nx.exr skybox/py.exr skybox/ny.exr skybox/pz.exr skybox/nz.exr \
    skybox.ktx2
```

Specular
```bash
ktx create --cubemap --format B10G11R11_UFLOAT_PACK32 --levels 6 \
  ibl-env/m0_px.exr ibl-env/m0_nx.exr ibl-env/m0_py.exr ibl-env/m0_ny.exr ibl-env/m0_pz.exr ibl-env/m0_nz.exr \
  ibl-env/m1_px.exr ibl-env/m1_nx.exr ibl-env/m1_py.exr ibl-env/m1_ny.exr ibl-env/m1_pz.exr ibl-env/m1_nz.exr \
  ibl-env/m2_px.exr ibl-env/m2_nx.exr ibl-env/m2_py.exr ibl-env/m2_ny.exr ibl-env/m2_pz.exr ibl-env/m2_nz.exr \
  ibl-env/m3_px.exr ibl-env/m3_nx.exr ibl-env/m3_py.exr ibl-env/m3_ny.exr ibl-env/m3_pz.exr ibl-env/m3_nz.exr \
  ibl-env/m4_px.exr ibl-env/m4_nx.exr ibl-env/m4_py.exr ibl-env/m4_ny.exr ibl-env/m4_pz.exr ibl-env/m4_nz.exr \
  ibl-env/m5_px.exr ibl-env/m5_nx.exr ibl-env/m5_py.exr ibl-env/m5_ny.exr ibl-env/m5_pz.exr ibl-env/m5_nz.exr \
  env.ktx2
```

irradiance

```bash
ktx create \
  --cubemap \
  --format B10G11R11_UFLOAT_PACK32 \
  ibl-irradiance/i_px.exr ibl-irradiance/i_nx.exr \
  ibl-irradiance/i_py.exr ibl-irradiance/i_ny.exr \
  ibl-irradiance/i_pz.exr ibl-irradiance/i_nz.exr \
  irradiance.ktx2
```
