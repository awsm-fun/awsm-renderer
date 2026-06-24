# AwsmRenderer

[![Crates.io](https://img.shields.io/crates/v/awsm-renderer.svg)](https://crates.io/crates/awsm-renderer)
[![Docs.rs](https://docs.rs/awsm-renderer/badge.svg)](https://docs.rs/awsm-renderer)
[![Scene Editor](https://img.shields.io/badge/scene_editor-live-brightgreen)](https://scene.awsm.fun)
[![Model Tester](https://img.shields.io/badge/model_tester-live-brightgreen)](https://model-tests.awsm.fun)

Rust/WASM/WebGPU renderer for the web.

It's specifically for the web in that it uses the WebGPU API directly via the `web-sys` bindings as opposed to wgpu. While this is somewhat unconventional in the Rust ecosystem, it allows for a more direct mapping to the WebGPU API for precise control and understanding of how things work under the hood in a web context.

# STATUS

See [ROADMAP](docs/ROADMAP.md) for details.

# PERFORMANCE

Should be pretty fast! Detailed performance annotations can be seen in the browser's devtools performance tab after recording a session.

# ARCHITECTURE

There's a lot to unpack here: render passes, buffers, shaders, pipelines... for the sake of brevity, here's the high-level overview of some of the key tradeoffs and design decisions:

## Render Passes

The core rendering is done in these main passes:

1. **Geometry Pass**: Renders all opaque geometry into a few targets with a minimal set of data needed for the next step. It's a draw call per mesh, but still _extremely_ fast: no texture lookups, no shading, no material-specific logic. Just geometry transformations (including morphs/skins) and writing out a few values per-pixel. Depth testing/writing is enabled here so it benefits from occlusion culling too. One of the key mechanics is that we pass barycentric coordinates per-texel, which allows us to reconstruct interpolated values in the next pass.

2. **Prep Pass**: A single compute pass, run before shading over the pixels classify marked as having geometry, that materializes the **material-independent** per-pixel work once into buffers (interpolated UV0 / vertex-color, and — the big one — per-light shadow visibility). The slim per-material shader then *reads* these instead of recomputing them, and the heavy code (notably the shadow-sampling block) drops out of every specialized material module. This is **not** a flag or a variant — it's just how opaque shading works. The one judgment call it embodies is the **prep-vs-recompute trade-off**: prep only materializes work that's expensive enough for the read to beat recomputing it (and/or that lets bulky code be evicted from the material modules). Trivially-cheap work is left to be re-derived in the shading wrapper instead — e.g. world-position (re-projected from depth on demand) and, at MSAA silhouette edges, per-sample UV/vertex-color (the edge shader already has the triangle + barycentric in hand, so the lerp is cheaper than a per-sample buffer's write + read + VRAM). Shadows clear that bar; those don't. Either way it's invisible to material authors, who just call an accessor. See [Shader Guidelines](docs/SHADER_GUIDELINES.md).

3. **Opaque Pass**: Uses the data from the geometry + prep passes, along with all the other available data (texture bindings, material info, etc.) to shade all the pixels in _one draw call_. Since this only shades visible pixels, it's much faster than traditional forward rendering. Since the "g-buffer" only contains geometry info, it's also much more flexible than traditional deferred rendering since it supports any number of materials in the single draw call.

4. **Transparent Pass**: Renders all transparent geometry via traditional forward rendering, on top of the opaque pass result. This is necessary since the opaque pipeline needs to know the exact identifer of a given pixel, and alpha blending breaks that. However, the transparent pass can still take advantage of early-z testing by using the same depth buffer from visibility pass, pipeline sorting to minimize state changes, etc. Also, the majority of renderables are typically opaque, so this is still a minor tradeoff overall. (Being forward, it has no visibility buffer to read prep from, so it recomputes its attributes inline — a different rendering model, not the prep flag.)

5. **Effects**: Applies several post-processing effects (bloom, dof, etc.) to the final image.

6. **Display Pass**: Applies tone-mapping to the final image before presenting it to the screen.

There's a few more implmentation details around msaa, hooks, and hud rendering as well, but those are the main passes.

## Buffers

The overall idea is that we load all the data we need for rendering into GPU buffers ahead of time, and then reference that data via offsets when issuing draw calls. 

Updating the data is easy, using "keys" (TransformKey, MeshKey, MaterialKey, etc.) and Rust-friendly structs. Under the hood, updates mark the GPU buffers as "dirty" so they get re-uploaded at the start of the next frame via one big memcpy per-buffer. This makes it very efficient to update data many times per-frame if needed (e.g. for physics).

Nearly all the data goes through one of two mechanisms:

  - **DynamicUniformBuffer**: not just for uniforms, but rather for any data of a predetermined size. We take advantage of that property to more efficiently manage the buffer. 
  - **DynamicStorageBuffer**: similar to above, but for heterogeneous data of varying size. We use more advanced techniques to manage the buffer efficiently while still keeping the API easy to use.

As the data grows, an occasional re-allocation is needed, but this is infrequent and handled automatically.

## Attributes

This is a bit involved since we explode the triangle vertices in the geometry pass and need to access the original per-vertex attributes in different ways throughout the renderer. For more info on how vertex attributes are handled and split into different buffers, see [Vertex Attributes](docs/VERTEX_ATTRIBUTES.md).

## Texture Pools

Textures are managed in texture pools, which are essentially arrays of textures of the same size and format. This allows for easy binding and staying under limits in shaders.

The pool can grow as needed, but it requires signaling the changes to shader generation, and so it's typically done infrequently like right after all images are downloaded.

## Bind Groups

Many things can cause a bind group to need to be re-created: resized buffers, new render views, texture pool changes, etc.

Instead of wiring all that logic directly, we broadcast various "events" that indicate what changed, and the relevant systems listen for those events and update their bind groups as needed at the start of the next frame.

## Shaders

Shaders are written with Askama templates, allowing for code reuse and easy-to-reason-about caching based on different variables exposed to the template. 

## Caching

Speaking of caching many things are cached to avoid redundant work and state changes, including pipelines, layouts, shaders, etc.

## GLTF Support

GLTF is supported as a first-class citizen, with support for PBR materials, skins, morphs, animations, and more.

It's de-facto _the_ format for AwsmRenderer assets, and extensions are used where appropriate to support features not in the core spec (e.g. texture transforms, unlit materials, etc.)

## Picking

Because the geometry pass writes out unique identifiers per-mesh, picking opaque meshes is as simple as reading back the pixel under the mouse cursor from that target, and mapping it back to the corresponding mesh. This makes picking opaque meshes extremely fast and efficient, even with complex scenes, without significant overhead during rendering.

# LIBRARY CRATES

`packages/crates/` is a modular WebGPU renderer + scene toolkit — 13 single-purpose
crates published to crates.io. The pure-CPU ones (curves, geometry, tangents,
meshgen, particles, glb-export, gltf-convert, scene) have no GPU or browser
dependencies and are usable in any Rust project; the rest are the engine you'd
build a WebGPU app on. Publishing the whole graph lets a downstream user write
`awsm-renderer = "…"` and pull the rest from crates.io (the crates reference each
other by version, kept in lockstep by `task bump`).

> **Crate rename (since 0.3.3).** The library crates moved from the bare `awsm-*`
> prefix to `awsm-renderer-*` — e.g. `awsm-meshgen` → `awsm-renderer-meshgen`,
> `awsm-materials` → `awsm-renderer-materials`, `awsm-scene` → `awsm-renderer-scene`
> (`awsm-renderer` itself is unchanged). The old names are yanked on crates.io;
> new projects must depend on the `awsm-renderer-*` names. There are no
> compatibility shims and no API changes — only the package names moved.

Everything **outside** `packages/crates/` is `publish = false`: the two frontends,
the `awsm-renderer-web-shared` glue, the render-worker example, the `awsm-renderer-debugging`
binaries, the `awsm-renderer-scene-mcp` server (ships as a binary via cargo-dist), and
`awsm-renderer-editor-protocol` (the internal editor↔server wire types, kept under
`packages/mcp/`).

Crates publish bottom-up (`→` = depends on; cargo orders the release so a crate is
on crates.io before anything that needs it).

### Foundations (pure, no internal deps)

| Crate | Depends on | What it is |
|---|---|---|
| **awsm-renderer-curves** | — | Pure-CPU curve math (3D paths + 1D parameter curves) |
| **awsm-renderer-geometry** | — | Pure-CPU geometry utils (AABB, ray/triangle, frustum) |
| **awsm-renderer-tangents** | — | MikkTSpace tangent generation over plain geometry arrays (no GPU) |
| **awsm-renderer-scene** | — | The lean canonical runtime scene schema (`scene.toml` + `assets/`) |
| **awsm-renderer-core** | — | The WebGPU renderer's core layer (a nicer Rust API over WebGPU) |

### Built on the foundations

| Crate | Depends on | What it is |
|---|---|---|
| **awsm-renderer-materials** | renderer-core | Pluggable material shaders behind a `MaterialShader` trait |
| **awsm-renderer-particles** | curves, geometry | Pure-CPU particle simulator (struct-of-arrays, GPU-shape-compatible) |
| **awsm-renderer-meshgen** | scene, curves, geometry | Pure-CPU mesh generators (primitives + sweep + procedural textures) |

### Renderer + IO

| Crate | Depends on | What it is |
|---|---|---|
| **awsm-renderer** | renderer-core, materials, scene, tangents | The WebGPU renderer engine |
| **awsm-renderer-glb-export** | meshgen, tangents | Scene-complete glTF/GLB export IR + writer (no GPU) |
| **awsm-renderer-gltf-convert** | glb-export, meshgen | Pure-data glTF → canonical-format normalizer (the shared import path) |
| **awsm-renderer-gltf** | renderer, renderer-core, materials, tangents | glTF ingestion into the live renderer |
| **awsm-renderer-scene-loader** | renderer, renderer-core, renderer-gltf, materials, meshgen, scene | Loads an awsm-renderer-scene bundle into the renderer (the player path) |

# DEVELOPMENT

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for details on setting up the development environment, building, and running the examples.

# EDITOR

The repo ships a WebGPU scene / material / animation **editor**
([`packages/frontend/editor`](packages/frontend/editor)) built on this renderer —
a node tree + transform gizmos, a custom-WGSL material studio, and an animation
timeline. Run it with:

```bash
task editor-dev      # serves http://localhost:9085
```

# DRIVING THE EDITOR FROM AN AI AGENT (MCP)

The editor can be driven programmatically by any MCP-capable agent (Claude Code,
Claude Desktop, Codex, …) — insert and transform nodes, author materials and edit
WGSL, drive the animation timeline, and read back editor state **and viewport
screenshots**. Useful for agent-in-the-loop scene authoring and visual checks.

## How it works

```
agent (MCP client) ──HTTP /mcp──▶ awsm-renderer-scene-mcp ──WebSocket /editor──▶ editor (browser tab)
                                  (packages/mcp)      editor dials out    → EditorController
```

A native server ([`packages/mcp`](packages/mcp), `awsm-renderer-scene-mcp`) exposes MCP
tools over streamable-HTTP and relays each one to a running editor tab over a
WebSocket the **editor dials out to** (a browser tab can't be a server). Every
mutation flows through the editor's single command/query authority, so the agent
and a human watching the same tab stay in sync.

## Install the MCP server

Prebuilt `awsm-renderer-scene-mcp` binaries are published on GitHub Releases for macOS
(arm64 + x86_64), Linux (x86_64), and Windows (x86_64):

```bash
# macOS / Linux
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/awsm-fun/awsm-renderer/releases/latest/download/awsm-scene-mcp-installer.sh | sh
```

```powershell
# Windows (PowerShell)
powershell -ExecutionPolicy Bypass -c "irm https://github.com/awsm-fun/awsm-renderer/releases/latest/download/awsm-scene-mcp-installer.ps1 | iex"
```

From source (needs Rust): `cargo install --git https://github.com/awsm-fun/awsm-renderer awsm-renderer-scene-mcp`, or `task mcp:install` from a local clone. Then run it — bare `awsm-renderer-scene-mcp` listens on `http://127.0.0.1:9086`.

## Quick start

1. Start the editor **and** the MCP server together:

   ```bash
   task mcp-dev
   ```

   | Service | Address |
   | --- | --- |
   | Editor (Trunk) | `http://localhost:9085` |
   | MCP server (HTTP + WebSocket) | `http://127.0.0.1:9086` (`/mcp`, `/editor`, `/png`) |

2. Attach the editor to the server — click the **link icon** ("Connect to MCP
   server") in the editor's top bar, or load it with the `?mcp=` param to
   auto-connect:

   ```
   http://localhost:9085/?mcp=http://127.0.0.1:9086
   ```

   Connect/disconnect show a toast and the button reflects the live state; the
   server logs `editor attached` once the link is up. (No connection → the editor
   runs normally with zero remote overhead.)

3. Point your agent at the MCP server. A ready-to-use [`.mcp.json`](.mcp.json) is
   included in the repo root:

   ```json
   {
     "mcpServers": {
       "awsm-renderer-scene": { "type": "http", "url": "http://127.0.0.1:9086/mcp" }
     }
   }
   ```

   - **Claude Code / Claude Desktop**: a project-root `.mcp.json` is picked up
     automatically — just restart the agent in this directory.
   - **Codex / other MCP clients**: register a streamable-HTTP MCP server pointing
     at `http://127.0.0.1:9086/mcp`.

## What the agent can do

~90 typed tools, including:

- **Discover / observe** — `get_snapshot` (scene tree, ids, selection, mode,
  materials, animation), `screenshot_scene` (PNG image block), `get_mode`,
  `canvas_stats`.
- **Scene** — `insert_primitive` / `insert_empty` / `insert_camera` /
  `insert_light`, `node_set_transform`, `rename_node`, `delete_node`,
  `duplicate_node`, `reparent_node`, `set_node_visible` / `_locked`,
  `set_selection`.
- **Materials** — `add_builtin_material`, `add_custom_material`,
  `set_material_wgsl` / `get_material_wgsl`, `register_material`,
  `assign_material`.
- **View / animation** — `switch_mode`, `snap_camera_to_axis`, `reset_camera`,
  `add_clip`, `set_playhead`, `set_playing`, …
- **Escape hatches** — `dispatch_command` / `run_query` accept any raw
  `EditorCommand` / `EditorQuery` JSON, so the *entire* command/query surface is
  reachable even without a dedicated tool.

For the full architecture, tool catalog, transport, and cert handling, see
[docs/MCP.md](docs/MCP.md).

# NON-GOALS

### ECS (or any other game framework)

This is a renderer, not a full game engine or framework. There is no entity-component-system (ECS) or any other opinionated way to organize game objects.

However, there is a transform-based scene graph, and all the data structures are designed to be very easy and efficient to manipulate and integrate with an ECS or other game framework by way of "keys" (TransformKey, MeshKey, MaterialKey, etc.)

Feel free to think of these keys as components and assign them to some EntityId of your choice.

### Physics

The renderer does include transformation, morphs, skins, and animation support, but does not include any physics engine or collision detection.

It's expected that another subsystem using this renderer would handle physics/collision detection separately, and provide the resulting transforms/animations to the renderer.

### Game world culling

This really depends on the specific needs of the project. Some examples:

* no culling at all (e.g. a fighting game)
* portal-based (e.g. a first-person shooter in an interior)
* space partitioning (e.g. in an open world game).
* quadtrees (e.g. in top-down view)

However, due to the visibility buffer optimization, the impact of rendering unnecessary geometry does not reach the shading stage. Also, frustum culling will eliminate other game world objects... so the only optimization would really be to reduce the frustum culling tests which are already very cheap.

# GRAVEYARD

I've taken some stabs at some variation of this sorta thing before, got a few battle scars along the way. Some projects got further than others:

* [Pure3d (typescript + webgl1)](https://github.com/dakom/pure3d-typescript)
* [Shipyard ECS (webgl2)](https://github.com/dakom/shipyard-webgl-renderer)
* [WebGL1+2 Rust bindings](https://github.com/dakom/awsm-web/tree/master/crate/src/webgl)
