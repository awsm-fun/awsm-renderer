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
agent (MCP client) ──HTTP /mcp──▶ awsm-mcp-server ──WebTransport (QUIC/UDP)──▶ editor (browser tab)
                                  (packages/mcp)       editor dials out         → EditorController
                                  • rmcp tool layer    server.open_bi() per req  • src/remote/ module
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
| `awsm-mcp-server` | [`packages/mcp`](../packages/mcp) | Native binary. rmcp tool layer over streamable-HTTP + the WebTransport listener + the single editor link. `publish = false`. |
| editor remote module | [`packages/frontend/editor/src/remote/`](../packages/frontend/editor/src/remote) | The WebTransport client: parse `?mcp=`, fetch `/control`, connect, `accept_bi()` loop, decode `Request` → call `EditorController` → encode `Response`. |

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
   | MCP + control HTTP (TCP) | `http://127.0.0.1:9086` — `/mcp`, `/control`, `/debug` |
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

~40 typed tools. Each is a thin wrapper that builds an `EditorCommand` /
`EditorQuery` from typed (schema'd) parameters and relays it to the editor. Node
and asset references are UUID strings — get them from `get_snapshot`.

**Discover / observe**
- `get_snapshot` — scene tree (ids/names/kinds), selection, mode, undo/redo
  depth, animation library, custom materials. *Start here.*
- `get_mode` — current workspace (`scene` / `material` / `animation`).
- `get_material_wgsl { asset }` — a custom material's WGSL source.
- `canvas_stats { region? }` — mean/min/max luma over a region or the whole canvas.
- `screenshot_scene` / `screenshot_material` / `screenshot_texture { asset }` —
  PNG returned as an MCP **image** content block, so the agent sees its effect.

**Scene / nodes**
- `insert_primitive { shape, parent? }` (plane/box/sphere/cylinder/cone/torus),
  `insert_empty`, `insert_camera`, `insert_light { kind, parent? }`.
- `node_set_transform { node, translation, rotation, scale }` (rotation is a
  quaternion `[x,y,z,w]`).
- `rename_node`, `delete_node`, `duplicate_node`, `reparent_node`,
  `set_node_visible`, `set_node_locked`, `set_selection`.

**Project / import / history**
- `new_project`, `load_project_from_url { base_url }`,
  `import_model_from_url { url }`, `undo`, `redo`.

**Materials**
- `add_builtin_material { shading }` (pbr/unlit), `add_custom_material`,
  `register_material { asset }`, `assign_material { node, material? }`.
- `set_material_wgsl { material, wgsl }` (replace source; auto-recompiles),
  `get_material_wgsl { asset }`.

**View / camera**
- `switch_mode { mode }`, `snap_camera_to_axis { axis }`, `reset_camera`.

**Animation**
- `add_clip` (returns the new clip id), `delete_clip`, `set_current_clip`,
  `set_playhead { t }`, `set_playing { on }`.

**Generic escape hatches — full coverage**
- `dispatch_command { command }` — a raw `EditorCommand` as JSON (internally
  tagged by `"cmd"`), e.g. `{"cmd":"set_keyframe","clip":"<uuid>","track":0,...}`.
- `run_query { query }` — a raw `EditorQuery` as JSON (tagged by `"query"`), e.g.
  `{"query":"canvas_pixels","coords":[[100,100]]}`.

The escape hatches reach **every** `EditorCommand` / `EditorQuery` variant — even
the ones without a dedicated tool (keyframes, tracks, the NLA mixer, environment,
…). The authoritative inventory is the enums themselves:
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
  (tab reloaded/closed). Reload the editor tab to re-attach.
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
- **Request/reply only.** There's no editor→agent push channel (e.g. "the user
  clicked node X"). An editor-initiated stream + an MCP resource/subscription
  would add it.

---

## Source anchors

- Protocol crate: [`packages/crates/editor-protocol/src`](../packages/crates/editor-protocol/src)
  (`command.rs`, `query.rs`, `node_spec.rs`, `anim_ui.rs`, `transport.rs`).
- Server: [`packages/mcp/src`](../packages/mcp/src) — `mcp.rs` (tools), `quic.rs`
  (WebTransport listener), `link.rs` (editor link + framing), `http.rs`
  (`/control`, `/debug`, `/mcp` mount), `cert.rs`.
- Editor remote: [`packages/frontend/editor/src/remote/mod.rs`](../packages/frontend/editor/src/remote/mod.rs);
  `?mcp=` parsing in [`main.rs`](../packages/frontend/editor/src/main.rs).
- Controller surface: [`controller/command.rs`](../packages/frontend/editor/src/controller/command.rs),
  [`controller/query.rs`](../packages/frontend/editor/src/controller/query.rs),
  `controller/state.rs` (`dispatch` / `query` / `snapshot`),
  [`engine/query.rs`](../packages/frontend/editor/src/engine/query.rs) (PNG / canvas readback).
</content>
