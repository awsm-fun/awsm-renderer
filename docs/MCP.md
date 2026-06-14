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
agent (MCP client) ──HTTP /mcp──▶ awsm-renderer-mcp ──WebTransport (QUIC/UDP)──▶ editor (browser tab)
                                  (packages/mcp)       editor dials out         → EditorController
                                  • rmcp tool layer    server.open_bi() per req  • src/remote.rs
                                  • QUIC listener      editor replies on stream  • calls controller directly
                                  • holds editor link                            • returns PNG bytes
```

**The one hard constraint:** a browser tab can't be a server and can't open raw
UDP/QUIC. So the **native server is the QUIC listener and the editor dials out to
it**, and "QUIC from the browser" means **WebTransport** (HTTP/3 over QUIC) via
the browser's `WebTransport` API. The [`web-transport`](https://docs.rs/web-transport)
crate is the unifier — it's quinn on native and `web_sys::WebTransport` on wasm,
behind one API.

Why WebTransport rather than a websocket: server-initiated bidirectional streams
(the server drives — no client-side polling), multiplexed concurrent requests
with no manual id-correlation (one request per stream; stream identity *is* the
correlation), and binary frames that carry PNG bytes without base64 bloat.

Three pieces:

| Piece | Where | Role |
|---|---|---|
| `awsm-editor-protocol` | [`packages/crates/editor-protocol`](../packages/crates/editor-protocol) | The serializable wire vocabulary — `EditorCommand` / `EditorQuery` / `EditorSnapshot` / `QueryResult` + the `Request` / `Response` transport envelope. Compiles for both wasm and native; re-exports the heavy payloads from `awsm-scene-schema`. |
| `awsm-renderer-mcp` | [`packages/mcp`](../packages/mcp) | Native binary. rmcp tool layer over streamable-HTTP + the WebTransport listener + the single editor link. `publish = false`. |
| editor remote module | [`packages/frontend/editor/src/remote.rs`](../packages/frontend/editor/src/remote.rs) | The WebTransport client: parse `?mcp=`, fetch `/control`, connect, `accept_bi()` loop, decode `Request` → call `EditorController` → encode `Response`. |

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
   | MCP + control HTTP (TCP) | `http://127.0.0.1:9086` — `/mcp`, `/control`, `/debug`, `/health`, `/boot-error` |
   | WebTransport link (UDP) | `9087` |

   (Ports live in [`taskfiles/config.yml`](../taskfiles/config.yml):
   `PORT_MCP_HTTP_DEV` / `PORT_MCP_QUIC_DEV`. Run the server alone with
   `task mcp:serve`.)

2. Attach the editor to the server, either way:

   - **Button** — open the editor normally (`http://localhost:9085`) and click the
     **link icon** in the top bar ("Connect to MCP server"). It connects to the
     default server (`http://127.0.0.1:9086`); click again to disconnect.
   - **URL param** — `http://localhost:9085/?mcp=http://127.0.0.1:9086` auto-connects
     on load (and points the button at that origin).

   Connect and disconnect show a toast, and the button reflects the live state
   (`Connecting…` → `MCP connected`). The server logs `editor attached` once the
   WebTransport link is up. With neither, the editor runs normally with zero remote
   overhead.

3. Point your agent at the MCP server. A ready-to-use
   [`.mcp.json`](../.mcp.json) lives in the repo root:

   ```json
   {
     "mcpServers": {
       "awsm-editor": { "type": "http", "url": "http://127.0.0.1:9086/mcp" }
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
- `get_node_details { nodes? }` — full per-kind config + material assignment.
- `resolve_node_material { node }` — the material a node actually RENDERS with
  (the direct answer, vs parsing the `NodeKind` blob).
- `get_node_bounds { nodes? }` — world-space AABB per node (for framing/sizing).
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
  `insert_empty`, `insert_camera`, `insert_light { kind, parent? }` — **return the
  new node id.**
- `node_set_transform { node, translation, rotation, scale }` (rotation is a local
  quaternion `[x,y,z,w]`), plus convenience: `set_translation`, `translate_by`,
  `set_scale`, `set_rotation_euler { euler, order? }`.
- `rename_node`, `delete_node`, `duplicate_node`, `reparent_node`,
  `set_node_visible`, `set_node_locked`, `set_selection`, `set_prefab` (mark/clear
  a node as a prefab root).

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
- Authoring: `set_material_alpha_mode`, `set_material_double_sided`,
  `set_material_debug_color`, `set_material_layout { uniforms, textures, buffers }`,
  `set_material_includes { keys }`, `set_material_fragment_inputs { keys }`,
  `set_material_uniform { material, name, value }`, `set_material_texture
  { node, slot, texture? }`, `set_builtin_param { node, param, value }`.

**Lighting / environment**
- `set_light_color`, `set_light_intensity`, `set_light_range`, `set_light_angles`.
- `set_environment { skybox?, ibl_prefiltered?, ibl_irradiance? }` (builtin or KTX).

**Textures**
- `add_texture_asset { proc }` (checker/gradient/noise) and
  `import_texture_from_url { url }` (PNG/JPEG/WebP, fetched + uploaded to the GPU)
  — both return the new id; bind with `set_material_texture`, or
  `set_node_texture { node, slot, texture? }` for a mesh node's built-in (inline
  PBR) slot (base_color | metallic_roughness | normal | occlusion | emissive).

**View / camera / time**
- `switch_mode { mode }`, `snap_camera_to_axis { axis }`, `reset_camera`.
- `set_camera_orbit { yaw, pitch, radius, look_at }`,
  `set_camera_projection { perspective, fov_y? }`, `frame_node { node, padding? }`.
- `set_frame_time { seconds }` / `clear_frame_time` — pin `frame_globals.time` for
  deterministic temporal-material screenshots.

**Animation**
- `add_clip` (returns the new id), `delete_clip`, `duplicate_clip`, `rename_clip`,
  `set_clip_duration`, `set_clip_speed`, `set_clip_loop`, `set_current_clip`,
  `set_playhead { t }`, `set_playing { on }`.
- Typed tracks/keys: `add_track { clip, target }`, `add_keyframe`, `set_keyframe`,
  `delete_keyframe`, `delete_track { clip, index }`.
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

**Rig / skin**
- `get_skin_data` (joints as node ids — see Discover), `get_skin_weights { node }`
  / `set_skin_weights { node, entries }` (per-vertex joints+weights, live re-deform),
  `solve_ik { end_node, target }` (analytic two-bone IK), `drop_skinning { node }`
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

One request travels per server-initiated bidirectional stream and the editor
replies on the same stream, so there is no request-id correlation and framing is
by stream-finish (write the whole message, `finish()`; read to end, decode).
Encoded as **JSON** at both edges (see note below).

```rust
// awsm-editor-protocol
pub enum Request {
    Dispatch(EditorCommand),   // mutate
    Query(EditorQuery),        // structured read (snapshot / timeseries / pixels / stats / wgsl)
    Undo, Redo,                // controller methods, not EditorCommands
    ScenePng, MaterialPng, TexturePng(AssetId),  // raw PNG bytes
    Mode,                      // current workspace mode
}

pub enum Response {
    Ok,
    Query(Box<QueryResult>),
    Png(Vec<u8>),              // raw PNG bytes (NOT a data: URL)
    Mode(EditorMode),
    Err(String),
}
```

**Why JSON, not a compact binary format.** `EditorCommand` / `EditorQuery` are
internally tagged (`#[serde(tag = "cmd")]` / `"query"`) and `QueryResult` is
untagged, which require a self-describing format (`deserialize_any`). Non-self-
describing codecs (bitcode, postcard, …) reject them with *"deserialize_any is not
supported"*. JSON handles all of them and is debuggable; PNG bytes ride as a
`Vec<u8>` and JSON-encode fine over localhost.

**Cert handling (dev).** The browser must pin the server's self-signed cert
hash before connecting. The server generates a fresh **ECDSA P-256** cert at
startup (in memory, no disk persistence, 10-day validity — a WebTransport
`serverCertificateHashes` requirement) and serves
`base64url(SHA-256(DER))` from a CORS-open `GET /control`:

```json
{ "quic_url": "https://127.0.0.1:9087", "cert_hash": "<base64url-sha256>" }
```

The editor fetches `/control`, pins the hash, and connects. A server restart mints
a new cert; the editor re-fetches on its next connect, so it "just works" — no cert
files to manage.

**`POST /debug`.** The same control server exposes a raw-request seam: POST a JSON
`Request` and it's relayed to the editor, returning the `Response` as JSON (PNGs
are written to a temp file and summarized). Handy for `curl`-driving the pipeline
without an MCP client. Example:

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
  its own, hence axum.
- **`web-transport`** unifies quinn (native) + the browser WebTransport API
  (wasm). The wasm side needs the `web_sys_unstable_apis` cfg, already set in
  [`.cargo/config.toml`](../.cargo/config.toml).

---

## Troubleshooting

- **`no editor attached`** — no editor tab is connected. Open
  `http://localhost:9085/?mcp=http://127.0.0.1:9086` and wait for `editor attached`
  in the server log. If you restarted the server, the editor must reconnect
  (reload the tab) because the cert hash changed.
- **Tool call fails with `open_bi: … closed`** — the editor's session dropped
  (tab reloaded/closed/frozen). The relay detaches the dead session, so
  `GET /health` now reports `editor_attached:false` (and `last_boot_error` if the
  page failed to init). Reload the editor tab to re-attach.
- **Black `screenshot_scene`** — `requestAnimationFrame` (and thus the WebGPU
  draw loop) pauses for hidden/background tabs, and a WebGPU `toDataURL` read can
  come back black if it lands between presents. Make sure the editor tab is the
  **visible, foreground** tab; see [DEBUGGING-PREVIEW.md](DEBUGGING-PREVIEW.md).
- **Verify two ways** — `screenshot_scene` lets the agent see its own effect
  through MCP; a human (or the Claude-in-Chrome extension) can watch the same live
  tab to confirm visually.

---

## Known limitations / future

- **Single editor link.** The server holds one attached editor; the last tab to
  connect wins. Multi-tab routing (a `link_id` selector) is not implemented.
- **Editor→agent push** is implemented for toasts (warning/error) and selection
  changes: the editor opens a unidirectional stream per event
  ([`remote::notify_event`](../packages/frontend/editor/src/remote.rs)), the
  server fans them out ([`link::EditorLink`](../packages/mcp/src/link.rs)) and each
  MCP session forwards them as `notifications/message` logging notifications
  ([`on_initialized`](../packages/mcp/src/mcp.rs)). Other event kinds (and an
  MCP resource-subscription model) are future work.

---

## Source anchors

- Protocol crate: [`packages/crates/editor-protocol/src`](../packages/crates/editor-protocol/src)
  (`command.rs`, `query.rs`, `node_spec.rs`, `anim_ui.rs`, `transport.rs`).
- Server: [`packages/mcp/src`](../packages/mcp/src) — `mcp.rs` (tools), `quic.rs`
  (WebTransport listener), `link.rs` (editor link + framing), `http.rs`
  (`/control`, `/debug`, `/mcp` mount), `cert.rs`.
- Editor remote: [`packages/frontend/editor/src/remote.rs`](../packages/frontend/editor/src/remote.rs);
  `?mcp=` parsing in [`main.rs`](../packages/frontend/editor/src/main.rs).
- Controller surface: [`controller/command.rs`](../packages/frontend/editor/src/controller/command.rs),
  [`controller/query.rs`](../packages/frontend/editor/src/controller/query.rs),
  `controller/state.rs` (`dispatch` / `query` / `snapshot`),
  [`engine/query.rs`](../packages/frontend/editor/src/engine/query.rs) (PNG / canvas readback).
</content>
