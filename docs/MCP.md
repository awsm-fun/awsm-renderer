# Driving the editor via MCP

The editor ([`packages/frontend/editor`](../packages/frontend/editor)) can be
driven by any MCP-capable agent (Claude Code, Claude Desktop, Codex, …): insert
and transform nodes, author materials and edit WGSL, drive the animation
timeline, and read back editor state **and viewport screenshots**. This is the
reference for how it's wired and how to use it.

The editor was built for this: every mutation funnels through a serializable
`EditorCommand`, every read through a serializable `EditorQuery`, and both types
live in a shared crate the native server and the editor both depend on.

---

## Architecture

```
agent (MCP client) ──HTTP /mcp──▶ awsm-scene-mcp ──WebSocket /editor──▶ editor (browser tab)
                                  (packages/mcp)      editor dials out    → EditorController
                                  • rmcp tool layer   id-tagged req/resp   • src/remote.rs
                                  • /editor ws + link  + push events        • calls controller directly
                                  • /png side-channel                       • uploads PNG to /png/<id>
```

**The one hard constraint:** a browser tab can't be a server. So the **editor
dials out** to the native server's `/editor` **WebSocket** and serves the server's
requests against its `EditorController`. The link is one ordered channel: the
server tags each `Request` with an `id` and the editor replies with a `Response`
carrying the same id (ids correlate request↔response). Frames are JSON text;
rendered PNGs never ride the link — the editor POSTs the bytes to a `/png/<id>`
HTTP side-channel and returns a small handle, keeping the control link byte-light.

Three pieces:

| Piece | Where | Role |
|---|---|---|
| `awsm-editor-protocol` | [`packages/mcp/editor-protocol`](../packages/mcp/editor-protocol) | The serializable wire vocabulary — `EditorCommand` / `EditorQuery` / `EditorSnapshot` / `QueryResult` + the `Request` / `Response` envelope and the `WsServerMsg` / `WsClientMsg` WebSocket frames. Compiles for both wasm and native. |
| `awsm-scene-mcp` | [`packages/mcp`](../packages/mcp) | Native binary. rmcp tool layer over streamable-HTTP + the `/editor` WebSocket link + the `/png` side-channel. Per-tab isolation via pairing codes. `publish = false`. |
| editor remote module | [`packages/frontend/editor/src/remote.rs`](../packages/frontend/editor/src/remote.rs) | The WebSocket client: parse `?mcp=`/`?pair=`, dial `ws://<origin>/editor`, read `Request` frames → call `EditorController` → reply with `Response` frames; POST screenshots to `/png/<id>`. |

All editor mutation flows through `EditorController` (the editor's single
command/query authority), so an agent and a human watching the same tab stay in
sync, and undo/redo/coalescing all work as in the UI.

---

## Quick start

1. Start the editor **and** the MCP server together:

   ```bash
   task mcp-dev
   ```

   | Service | Address |
   | --- | --- |
   | Editor (Trunk) | `http://localhost:9085` |
   | MCP server (HTTP + WebSocket) | `http://127.0.0.1:9086` — `/mcp`, `/editor` (ws), `/png/<id>`, `/debug`, `/health`, `/boot-error` |

   (The single port lives in [`taskfiles/config.yml`](../taskfiles/config.yml):
   `PORT_MCP_HTTP_DEV`. Run the server alone with `task mcp:serve`, or the
   installed binary with `awsm-scene-mcp`.)

2. Attach the editor to the server, either way:

   - **Button** — open the editor normally (`http://localhost:9085`) and click the
     **link icon** in the top bar ("Connect to MCP server"). It connects to the
     default server (`http://127.0.0.1:9086`); click again to disconnect.
   - **URL param** — `http://localhost:9085/?mcp=http://127.0.0.1:9086` auto-connects
     on load (and points the button at that origin).

   Connect and disconnect show a toast, and the button reflects the live state
   (`Connecting…` → `MCP connected`). The server logs `editor attached` once the
   WebSocket link is up. With neither, the editor runs normally with zero remote
   overhead. When more than one tab/agent is connected the server asks for a
   **pairing code** — the agent prints it (`pairing_status`); enter it in the MCP
   modal or append `&pair=<code>` to the editor URL.

3. Point your agent at the MCP server. A ready-to-use
   [`.mcp.json`](../.mcp.json) lives in the repo root:

   ```json
   {
     "mcpServers": {
       "awsm-scene": { "type": "http", "url": "http://127.0.0.1:9086/mcp" }
     }
   }
   ```

   - **Claude Code / Claude Desktop** — a project-root `.mcp.json` is picked up
     automatically; just (re)start the agent in this directory.
   - **Codex / other MCP clients** — register a streamable-HTTP MCP server at
     `http://127.0.0.1:9086/mcp`.

---

## Tool catalog

~130 typed tools plus MCP **resources** (the docs below) and **prompts** (workflow
templates). Each tool is a thin wrapper that builds an `EditorCommand` /
`EditorQuery` from typed (schema'd) parameters and relays it to the editor. Node
and asset references are UUID strings — get them from `get_snapshot`. This catalog
groups the tools by area; it isn't exhaustive — the escape hatches (bottom) reach
every command/query, and each tool self-describes over the MCP schema.

> **New to driving this over MCP?** Read the [Agent Guide](AGENT_GUIDE.md)
> (`awsm://docs/agent-guide`) first — it covers the mutate→settle→screenshot
> loop, an end-to-end scene walkthrough, lighting, batching, and
> troubleshooting. For custom materials see the
> [recipes cookbook](dynamic-materials/recipes.md)
> (`awsm://docs/material-recipes`); for animation see
> [Animation Authoring](ANIMATION_AUTHORING.md) (`awsm://docs/animation`).

**Connection / health**
- `ping` — confirm an editor is attached (fails fast otherwise).
- `pairing_status` — this session's pairing state (paired? this session's code?
  how many tabs/agents connected?) without performing an editor op. Call it after
  a `No editor is paired` error to surface the code for the human.
- `get_console_logs { limit? }` — recent editor notices (toasts) + raw tracing
  (WARN/ERROR from the render loop / bridges) from a ring buffer; surfaces
  runtime errors otherwise stuck in the browser.
- `get_memory_stats` — JS-heap bytes (Chrome) + renderer object counts (meshes /
  transforms / materials / lines / compiled render+compute pipelines), for leak
  / soak observability (sample over time — flat = healthy).
- `GET /health` (plain HTTP, not an MCP tool) — `{ editor_attached,
  last_boot_error }`. **Check this first when `/debug` / tool calls go silent**:
  it truthfully reports a detached/dead session (the relay drops a session on
  transport failure) and surfaces a renderer boot error the tab POSTed to
  `/boot-error` before any attach.

**Discover / observe**
- `get_snapshot` — scene tree (ids/names/kinds + visible/locked), selection,
  mode, undo/redo depth, animation library, custom materials (incl. `compile_ok`
  + `errors`), textures, project coordinate-system metadata. *Start here.*
- `get_mode` — current workspace (`scene` / `material` / `animation`).
- `get_skin_data { nodes? }` — rig discovery: joints as scene-node ids (+ name,
  live flag, current local TRS); pose joints with `set_node_transform`, animate
  them with `add_track` transform targets.
- `get_morph_data { nodes? }` / `set_morph_weight { node, index, value }` —
  live morph weights (+ target names via glTF `mesh.extras.targetNames`);
  set is a transient preview, tracks own the weights during playback.
- `get_node_transforms { nodes? }` — local TRS + world matrix per node (empty = all).
- `get_children { node }` — direct children as a lightweight `[{ id, name, kind }]`
  list. `get_subtree { node? }` — the id/name/kind subtree rooted at `node` (or
  EVERY scene root when omitted), with nested `children`. Both avoid the heavy
  whole-scene `get_snapshot` when you just need to navigate the hierarchy (e.g.
  find the descendants of a node you just created/duplicated).
- `get_node_details { nodes? }` — full per-kind config + material assignment.
- `resolve_node_material { node }` — the material a node actually RENDERS with
  (the direct answer, vs parsing the `NodeKind` blob).
- `get_node_bounds { nodes? }` — world-space AABB `{ min, max }` per node (for
  framing/sizing) **+ a facing hint** `{ forward, up, right }`: the node's local
  axes (−Z / +Y / +X) in world space, derived from its world matrix. `forward` is
  the project's −Z-forward convention — use it to place things relative to a
  node's orientation ("on the back" = −`forward`). NOTE: this is the node's
  *transform* orientation; an imported model's *geometry* may face a different way
  (the convention; verify visually).
- `get_material_wgsl { asset }` — a custom material's WGSL source.
- `get_material_diagnostics { asset }` — `{ registered, ok, errors }` (tell a
  compile failure from a successful-but-dark shader).
- `get_material_contract { transparent? }` — the WGSL authoring ABI + legal keys.
- `get_track_data { clip, track }` — a track's full keyframes/sampler/mute/solo.
- `get_frame_globals` — renderer `time`/`delta_time`/`frame_count`/`resolution`.
- `canvas_stats { region? }` — mean/min/max luma over a region or the whole canvas.
- `wait_render_settled { max_ms? }` — block until recompiles drain + a frame
  presents. **Call between an edit and a screenshot.**
- `screenshot_scene { width?, height? }` / `screenshot_material { width?, height? }`
  / `screenshot_texture { asset }` — PNG as an MCP **image** block.

**Scene / nodes**
- `insert_primitive { shape, parent? }` (plane/box/sphere/cylinder/cone/torus),
  `insert_empty`, `insert_camera`, `insert_light { kind, parent? }`,
  `insert_particle { parent? }` (CPU particle emitter), `insert_decal { parent? }`
  (projection decal; transform = oriented unit-cube volume, projects down local -Z)
  — **return the new node id.** Other node kinds (Line, Sprite, Curve, Sweep,
  Instances) are created via `dispatch_command { command: { cmd: "insert", spec:
  "line" | "sprite" | "curve" | "sweep" | "instances", … } }`.
- `set_particle_emitter { node, spawn_rate?, burst_count?, max_alive?, one_shot?,
  space?, shape?, initial_speed?, lifetime?, size?, forces?, color_over_life?,
  size_over_life?, blend?, texture? }` — typed, **patch-style** emitter config
  (send any subset; only those change). `texture` = a billboard SPRITE asset id:
  author a soft radial-alpha disc with `create_texture` and bind it for
  disc-shaped (alpha-masked) particles instead of hard squares. `shape` is
  `{point}`/`{sphere:{radius}}`/`{cone:{
  angle_radians, direction}}` (cone `direction` is in the emitter's **local**
  space); `forces` is a list of `{gravity:{acceleration:[x,y,z]}}` /
  `{linear_drag:{coefficient_x1000}}`; `blend:true` routes through the
  transparent-blend pass for true alpha fades (smoke/glows). Errors if the node
  isn't an emitter.
- `set_mesh_shadow { node, cast, receive }` — toggle a Mesh / SkinnedMesh /
  InstancesAlongCurve node's shadow casting / receiving (read-modify-write of its
  `shadow` config via `SetKind`). `set_instance_colors { node, colors }` — set an
  InstancesAlongCurve node's per-instance linear-RGBA tints (empty clears them).
- `node_set_transform { node, translation, rotation, scale }` (rotation is a local
  quaternion `[x,y,z,w]`), plus convenience: `set_translation`, `translate_by`,
  `set_scale`, `set_rotation_euler { euler, order? }`.
- `rename_node`, `delete_node`, `duplicate_node` (deep clone as a following
  sibling — **returns the new clone's root node id**; descendants get fresh ids,
  found via `get_children`/`get_subtree`), `reparent_node`, `set_node_visible`,
  `set_node_locked`, `set_selection`, `set_prefab` (mark/clear a node as a prefab
  root).

**Project / import / history**
- `new_project` (seeds a key light + IBL), `load_project_from_url { base_url }`,
  `import_model_from_url { url }`, `undo`, `redo`.

**Materials**
- `add_builtin_material { shading }` (pbr/unlit), `add_custom_material` — **return
  the new id.** `register_material`, `assign_material { node, material? }`,
  `delete_custom_material`, `copy_material_instance { from, to }`,
  `update_builtin_material` (replace a built-in's variant `MaterialDef` wholesale).
- `set_material_wgsl { material, wgsl }` — replace source + synchronous recompile;
  **answers truthfully** (errors carry the compiler diagnostics, no silent `ok`).
  The WGSL is validated against the contract for the material's CURRENT alpha mode
  (Blend → transparent `TransparentShadingOutput`; Opaque/Mask → opaque
  `OpaqueShadingOutput`), so **set `set_material_alpha_mode blend` BEFORE pushing a
  transparent body** — otherwise that one call reports a transient contract error
  (the final state self-corrects once both are set, in either order, since each
  re-validates). Tool calls in one message aren't ordered — sequence them.
- Authoring: `set_material_alpha_mode`, `set_material_double_sided`,
  `set_material_debug_color`, `set_material_layout { uniforms, textures, buffers }`,
  `set_material_includes { keys }`, `set_material_fragment_inputs { keys }`,
  `set_material_uniform { material, name, value }`, `set_material_texture
  { node, slot, texture? }`, `set_builtin_param { node, param, value }`
  (`base_color` accepts 3 floats RGB **or 4 = RGBA** with the 4th = alpha),
  `set_builtin_alpha_mode { node, mode: opaque|mask|blend, cutoff? }` — typed
  alpha mode for a built-in/inline material (glass = `blend` + base_color alpha<1),
  no whole-`MaterialDef` resend.

**Lighting / environment**
- `set_light_color`, `set_light_intensity`, `set_light_range`, `set_light_angles`.
- `set_environment { skybox?, ibl_prefiltered?, ibl_irradiance? }` (builtin or KTX).

**Textures**
- `add_texture_asset { proc }` (checker/gradient/noise) and
  `import_texture_from_url { url }` (PNG/JPEG/WebP, fetched + uploaded to the GPU)
  — both return the new id; bind with `set_material_texture`, or
  `set_node_texture { node, slot, texture? }` for a mesh node's built-in (inline
  PBR) slot (base_color | metallic_roughness | normal | occlusion | emissive).
- `set_node_texture_transform { node, slot, offset?, scale?, rotation?, flow?,
  wrap_u?, wrap_v?, uv_set? }` — patch the UV transform / flow / sampler-wrap of a
  built-in slot that already has a texture bound (patch-style: only the fields you
  pass change). `scale>1` tiles; `flow=[u,v]` auto-scrolls the texture in
  UV-units/sec (conveyors/water/lava; `[0,0]` stops it); `wrap_*` =
  repeat|clamp_to_edge|mirrored_repeat. Applying to an empty slot is rejected, not
  silently ignored. For a directional/keyframed scroll, use a `texture_transform`
  animation track instead.
- `create_texture { data, width?, height?, format?, linear? }` — the generic
  "author **any** texture" primitive: the agent ships the pixels itself instead
  of picking a procedural preset. Two modes: **raw pixels** — `format="rgba8"` +
  `width` + `height`, `data` = base64 of `width*height*4` RGBA8 bytes (row-major,
  top-left origin); or **encoded image** — `data` = a `data:` URI
  (`data:image/png;base64,…`) or bare base64 of a PNG/JPEG/WebP (dims/format
  derived). Set `linear=true` for data/normal/roughness/height maps (skips the
  sRGB→linear conversion). Returns the new id; bind with `set_material_texture`.
  Use it for soft particle sprites, fbm height/normal maps, gradients, cubemap
  faces — no built-in generator required. (Session-local, like
  `import_texture_from_url`.) Invalid payloads are **rejected loudly** (e.g. an
  `rgba8` byte count that doesn't match `width*height*4`).

**View / camera / time**
- `switch_mode { mode }`, `snap_camera_to_axis { axis }`, `reset_camera`.
- `set_camera_orbit { yaw, pitch, radius, look_at }`,
  `set_camera_projection { perspective, fov_y? }`, `frame_node { node, padding? }`
  (padding 0 = tight; fits the node's bounds to fill the view).
- `reset_pose { node }` — restore a node + all descendants to their scene base
  transforms; reverts a clip's last-previewed pose left baked after clearing the
  current clip (pass a rig root to reset a skeleton). Transient, not undoable.
- `set_frame_time { seconds }` / `clear_frame_time` — pin `frame_globals.time` for
  deterministic temporal-material screenshots. Also pins texture **UV flow** scroll
  (`set_node_texture_transform flow=`) to that absolute time (`offset =
  base + velocity*t`), so a scrolling texture screenshots the same phase every call.

**Animation**
- `add_clip` (returns the new id), `delete_clip`, `duplicate_clip`, `rename_clip`,
  `set_clip_duration`, `set_clip_speed`, `set_clip_loop`, `set_current_clip`,
  `set_playhead { t }`, `set_playing { on }`.
- Typed tracks/keys: `add_track { clip, target }`, `add_keyframe`, `set_keyframe`,
  `delete_keyframe`, `delete_track { clip, index }`. `target.kind`: transform |
  morph | uniform | builtin_param | light | camera | **texture_transform**
  (`node` + `slot` [base_color|metallic_roughness|normal|occlusion|emissive] +
  `prop` [offset(vec2) | scale(vec2) | rotation(scalar)] — keyframe a built-in
  texture's UV transform, e.g. a directional/reversible conveyor scroll per clip).
- Track flags + transport: `set_track_mute`, `set_track_solo` (any solo ⇒ only
  soloed tracks pose), `set_track_sampler { sampler: step|linear|cubic }`,
  `step_playhead { to: home|prev|next|end }`.

**Mesh editing (procedural stacks + raw vertices)**
- Every procedural node is an editable `Mesh` backed by a `ModifierStack`
  (`MeshDef`). `get_mesh_modifiers { mesh }` reads the recipe `{ base, modifiers }`
  (null if none yet); `set_mesh_modifiers { mesh, stack }` replaces it;
  `add_modifier` / `set_modifier { index }` / `remove_modifier { index }` edit it
  incrementally (mesh refs are **asset UUIDs**, not node ids).
- `collapse_mesh_stack { mesh }` bakes the stack to frozen-topology raw triangles
  (undoable); `bake_all` does it project-wide (finalize). `get_mesh_layers` shows
  live-vs-locked layers; `get_mesh_stats` / `get_node_bounds` / `get_mesh_cross_section`
  measure resolved geometry.
- Raw-vertex editing (after collapse, or on captured meshes):
  `get_vertex_data { node, indices }`, `select_vertices_where { node, predicate }`
  → indices, `set_vertex_positions`, `set_vertex_normals`, `paint_vertex_colors`,
  `soft_transform_vertices { falloff }` (radial falloff), `set_vertex_selection`
  (viewport highlight).
- **Fused select-and-act (scales to full-res meshes — the index array stays
  server-side, never round-trips):** `paint_where { node, predicate, color }`
  (= select_vertices_where + paint_vertex_colors in one call) and
  `transform_where { node, predicate, translation, falloff }` (= select +
  soft_transform). Prefer these over the select→indices→act pattern when a
  predicate matches thousands of verts (a real terrain's height band), which
  overflows the tool-result token cap if returned. Painted colors still only
  DISPLAY under a vertex-color-reading material (built-in PBR with
  `vertex_colors_enabled`).
  - ⚠️ **Splat-weight footgun:** unpainted vertex color is **`(1,1,1,1)` white**,
    not 0 — `mix(base, snow, vColor.r)` reads full weight everywhere until you
    paint. **Clear-to-0 first:** `paint_where { node, predicate:
    {"kind":"within_aabb","min":[-1e9,-1e9,-1e9],"max":[1e9,1e9,1e9]},
    color:[0,0,0,1] }` zeroes every vertex in one call, then paint the band.

**Rig / skin**
- `get_skin_data` (joints as node ids — see Discover), `get_skin_weights { node }`
  / `set_skin_weights { node, entries }` (per-vertex joints+weights, live re-deform),
  `solve_ik { end_node, target, root_node? }` (analytic two-bone IK; `root_node`
  pins the chain root when the auto end→parent→grandparent walk picks wrong
  bones), `drop_skinning { node }`
  (bake a skinned mesh to a static editable Mesh).

**Bake / export / bundle**
- `export_scene_glb` / `export_node_glb` — bake to binary glTF (base64); PBR→glTF
  PBR, Unlit→`KHR_materials_unlit`, custom/Toon→`AWSM_materials_none`.
- `export_player_bundle` — bake the project to a runtime bundle dir (`scene.toml`
  + `assets/`). `load_player_bundle` — round-trip self-test: bundle the current
  project in-memory, reset, reload through `populate_awsm_scene`.

**Batch + generic escape hatches — full coverage**
- `dispatch_batch { commands }` — a list of raw `EditorCommand`s applied
  atomically as one undo step (one round-trip).
- `dispatch_command { command }` — a single raw `EditorCommand` (tagged by `"cmd"`).
- `run_query { query }` — a raw `EditorQuery` (tagged by `"query"`).
- `patch_kind { node, patch }` — edit a node's kind with an **RFC 7386 JSON
  merge-patch** instead of resending the whole `NodeKind` via `SetKind`. Only the
  fields in `patch` change; `null` removes a key; nested objects merge; arrays
  replace. The result must still be a valid `NodeKind` (rejected loudly). The
  ergonomic pattern for escape-hatch edits without a typed tool: `get_node_details`
  to see the exact shape + field names, then send just the delta.

**Resources** (read-only docs): `awsm://docs/mcp`,
`awsm://docs/material-contract-opaque`, `awsm://docs/material-contract-transparent`.

**Prompts** (workflow templates): `author_lit_material`, `setup_rotation_clip`,
`import_and_frame_model`.

**Push channel** — the editor relays toasts (warning/error) and selection changes
to the agent as MCP `notifications/message` logging notifications, so an agent can
react to compile errors or a human clicking a node.

The escape hatches reach **every** `EditorCommand` / `EditorQuery` variant. The
authoritative inventory is the enums themselves:
[`controller/command.rs`](../packages/frontend/editor/src/controller/command.rs)
and [`controller/query.rs`](../packages/frontend/editor/src/controller/query.rs)
(which re-export from `awsm-editor-protocol`).

---

## Wire protocol

The link is one ordered WebSocket. The server tags each `Request` with an `id`;
the editor replies with a `Response` carrying the same id (ids correlate
request↔response). Frames are the `WsServerMsg` / `WsClientMsg` envelopes,
serialized as **JSON** text. A single writer on each side owns the socket so
concurrent replies/events never interleave a half-written frame.

```rust
// awsm-editor-protocol
pub enum Request {
    Dispatch(EditorCommand),       // mutate
    DispatchBatch(Vec<EditorCommand>), // atomic multi-command (one undo entry)
    Query(EditorQuery),            // structured read (snapshot / timeseries / pixels / stats / wgsl)
    Undo, Redo,                    // controller methods, not EditorCommands
    ScenePng, MaterialPng, TexturePng(AssetId),  // rendered PNGs (returned as a handle)
    Mode,                          // current workspace mode
}

pub enum Response {
    Ok,
    Query(Box<QueryResult>),
    Png(PngHandle),                // { id, byte_len, width, height } — bytes are at /png/<id>
    Mode(EditorMode),
    Err(String),
}

pub enum WsServerMsg { Request { id, req }, PairingRequired, Detached }
pub enum WsClientMsg { Pair { code }, Response { id, resp }, Event(EditorEvent) }
```

**Why JSON.** `EditorCommand` / `EditorQuery` are internally tagged
(`#[serde(tag = "cmd")]` / `"query"`) and `QueryResult` is untagged, which require
a self-describing format (`deserialize_any`). JSON handles all of them and is
debuggable in the browser devtools. Since PNG bytes ride the `/png` side-channel
(not the link), the control frames stay small and human-readable.

**The `/png/<id>` side-channel.** A `screenshot_*` request renders the PNG, the
editor POSTs the raw bytes to `POST /png/<id>` (a separate HTTP connection, off
the control link), and returns only a `PngHandle`. The rmcp tool reads the bytes
back from the temp file the upload landed in and returns them to the agent as an
MCP image block. Retained files are LRU-capped on disk.

**No certificates.** The link is a plain `ws://` (loopback). For a TLS-terminated
remote server, tick "Use TLS" in the connect modal (or set it via the modal) for
`wss://`. There is no cert-pinning / `/control` handshake anymore.

**`POST /debug`.** The server exposes a raw-request seam: POST a JSON `Request`
and it's relayed to the editor, returning the `Response` as JSON (a PNG request
returns the handle; fetch the bytes at `/png/<id>`). Handy for `curl`-driving the
pipeline without an MCP client. Example:

```bash
curl -s -X POST http://127.0.0.1:9086/debug -H 'content-type: application/json' \
  -d '{"Dispatch":{"cmd":"insert","spec":{"primitive":{"box":{"dims":[1,1,1]}}},"parent":null}}'
```

---

## Toolchain notes

- **rmcp 1.x** (`rmcp = "1"`, features `server, macros, schemars,
  transport-streamable-http-server`). It's edition-2024, so the workspace
  `rust-version` is **1.85** — with that floor a plain `rmcp = "1"` resolves
  cleanly (no pinning). `ServerInfo` is `#[non_exhaustive]` in 1.x; build it from
  `Default` + field assignment. `StreamableHttpService` is a tower service mounted
  on the axum router via `nest_service("/mcp", …)` — rmcp ships no HTTP listener of
  its own, hence axum. Loopback agents sit idle between tool calls, so the rmcp
  session `keep_alive` is set to a day (the 5-minute default would reap a
  live-but-idle session).
- **The link** is `axum`'s built-in WebSocket (the `ws` feature) on the server and
  `gloo-net`'s WebSocket on the wasm editor — no extra transport stack, no certs.

---

## Troubleshooting

- **`no editor attached`** — no editor tab is connected. Open
  `http://localhost:9085/?mcp=http://127.0.0.1:9086` and wait for `editor attached`
  in the server log. The editor auto-reconnects with backoff, so a server restart
  re-attaches on its own (no tab reload needed).
- **`No editor is paired with this MCP session`** — more than one tab/agent is
  connected, so the server can't auto-bind. Call `pairing_status` to get this
  session's code, then enter it in the editor's MCP modal (or open the editor with
  `&pair=<code>`).
- **The editor's socket dropped** (tab reloaded/closed/frozen) — the server forgets
  the connection, so `GET /health` reports `editor_attached:false` (and
  `last_boot_error` if the page failed to init). The tab re-attaches when it's back.
- **Black `screenshot_scene`** — `requestAnimationFrame` (and thus the WebGPU
  draw loop) pauses for hidden/background tabs, and a WebGPU `toDataURL` read can
  come back black if it lands between presents. Make sure the editor tab is the
  **visible, foreground** tab; see [DEBUGGING-PREVIEW.md](DEBUGGING-PREVIEW.md).
- **Verify two ways** — `screenshot_scene` lets the agent see its own effect
  through MCP; a human (or the Claude-in-Chrome extension) can watch the same live
  tab to confirm visually.

---

## Known limitations / future

- **Per-tab isolation.** Each editor tab (one `/editor` socket) and each MCP agent
  get their own identity, bound to each other. Binding is automatic when exactly
  one unbound tab and one unbound agent exist; otherwise the agent surfaces a
  4-char pairing code the tab presents (via the modal or `?pair=`). Requests,
  responses, and events never cross between sessions
  ([`link::EditorLink`](../packages/mcp/src/link.rs)).
- **Editor→agent push** is implemented for toasts (warning/error) and selection
  changes: the editor sends a `WsClientMsg::Event`
  ([`remote::notify_event`](../packages/frontend/editor/src/remote.rs)), the server
  tags it with the originating connection id and fans it out, and each agent's
  forwarder keeps only its bound tab's events, relaying them as
  `notifications/message` logging notifications
  ([`on_initialized`](../packages/mcp/src/mcp.rs)). Other event kinds (and an MCP
  resource-subscription model) are future work.

---

## Source anchors

- Protocol crate: [`packages/mcp/editor-protocol/src`](../packages/mcp/editor-protocol/src)
  (`command.rs`, `query.rs`, `node_spec.rs`, `anim_ui.rs`, `transport.rs`).
- Server: [`packages/mcp/src`](../packages/mcp/src) — `mcp.rs` (tools), `ws.rs`
  (`/editor` WebSocket, single-writer), `link.rs` (connections / agents / pairing),
  `http.rs` (`/editor`, `/png`, `/debug`, `/health`, `/mcp` mount).
- Editor remote: [`packages/frontend/editor/src/remote.rs`](../packages/frontend/editor/src/remote.rs);
  `?mcp=` parsing in [`main.rs`](../packages/frontend/editor/src/main.rs).
- Controller surface: [`controller/command.rs`](../packages/frontend/editor/src/controller/command.rs),
  [`controller/query.rs`](../packages/frontend/editor/src/controller/query.rs),
  `controller/state.rs` (`dispatch` / `query` / `snapshot`),
  [`engine/query.rs`](../packages/frontend/editor/src/engine/query.rs) (PNG / canvas readback).
</content>
