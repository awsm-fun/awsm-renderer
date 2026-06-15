# MCP Rewrite — porting the `audio` MCP design into the scene editor

Status: **not started**. This is a living checklist — implementers tick `[ ]` → `[x]`
as items land, and may add notes inline. Work it phase-by-phase, top to bottom.

## Context

The sibling repo `../audio` (`github.com/awsm-fun/awsm-audio`) rebuilt its MCP
(Model Context Protocol) integration and learned a lot. This repo
(`github.com/awsm-fun/awsm-renderer`, the WebGPU **scene/material/animation
editor**) needs the same treatment. `audio` is the **reference / blueprint**;
copy and adapt from it rather than inventing.

The three-piece topology (identical in both apps):

```
MCP client (Claude/Codex/Cursor)  --rmcp /mcp (HTTP)-->  native MCP server  <--/editor WebSocket--  browser editor (WASM)
                                                          (stateless bridge)        (dials OUT, holds document truth)
```

### Decisions locked in (from the planning conversation)

1. **Binary rename**: `awsm-renderer-mcp` → **`awsm-scene-mcp`** everywhere
   (package name, `[[bin]]`, taskfiles, install scripts, help text, README,
   dist `precise-builds` package).
2. **Transport**: drop WebTransport/QUIC entirely, use **WebSockets** (axum `ws`
   on the server, `gloo_net` WebSocket in the browser) — mirror `audio`.
3. **PNG payloads**: use an **HTTP side-channel** (`/png/{id}` POST/GET) like
   audio's `/renders` — the control link never carries image bytes, only a small
   handle. The WS control link therefore becomes clean JSON text frames.
4. **Tool-layer lessons**: research current best practice; adopt what genuinely
   improves the scene editor (apply judgment — change either side as warranted).
   Renderer already has ~200 working tools, so this is surgical, not a rewrite.
5. **Notifications**: a Settings toggle **"Show MCP notifications"** (default on)
   gating MCP toasts, **plus** a **dismiss-all** control.

### Reference files in `../audio` (copy/adapt from these)

| Concern | audio path |
|---|---|
| Server entry / single port | `packages/mcp/src/main.rs` |
| HTTP routes (`/mcp`, `/debug`, `/editor` ws, `/renders`, `/assets`, health) | `packages/mcp/src/http.rs` |
| WebSocket frame handler, single-writer task | `packages/mcp/src/ws.rs` |
| Isolation: `Connection` / `AgentSession`, Weak binding, pair codes | `packages/mcp/src/link.rs` |
| rmcp tool layer, tool-doc conventions | `packages/mcp/src/mcp.rs` |
| Browser WS client, reconnect, pairing submit | `packages/frontend/editor/src/remote.rs` |
| Live work display (chip, label, auto-follow, feed) | `packages/frontend/editor/src/mcp_activity.rs` |
| Connect/disconnect modal | `packages/frontend/editor/src/ui/mcp_modal.rs` |
| Help modal + "Using the MCP" tab | `packages/frontend/editor/src/ui/help_modal.rs` |
| Top bar (MCP button + Help button) | `packages/frontend/editor/src/ui/transport.rs` |
| cargo-dist config | `Cargo.toml` → `[workspace.metadata.dist]`, `packages/mcp/Cargo.toml` → `[package.metadata.dist]` |
| Release CI | `.github/workflows/release.yml` |
| `bump` + `publish` tasks | `Taskfile.yml`, `taskfiles/mcp.yml` |
| Release docs | `docs/RELEASING.md` |
| Install one-liners | `README.md` |

### Renderer current state (what we're changing)

- Server `packages/mcp/src/`: `main.rs`, `http.rs`, `link.rs`, `mcp.rs`,
  **`quic.rs`** (delete), **`cert.rs`** (delete).
- Transport deps to remove from `packages/mcp/Cargo.toml` &
  `packages/frontend/editor/Cargo.toml`: `web-transport`, `rustls`, `rcgen`,
  `sha2`, the cert-hash `base64` use. (Audit `Cargo.lock`/`Cargo.toml` for
  `quinn`, `h3`, `webtransport*` and confirm they're gone after.)
- Two ports today: `9086` (HTTP) + `9087` (QUIC/UDP). Collapse to **one port
  `9086`** (HTTP + `/editor` ws). `taskfiles/config.yml` holds
  `PORT_MCP_HTTP_DEV`/`PORT_MCP_QUIC_DEV`.
- Protocol `packages/crates/editor-protocol/src/transport.rs`: `Request` has
  `Dispatch`, `DispatchBatch(Vec<EditorCommand>)`, `Query`, `ScenePng`,
  `MaterialPng`, `TexturePng`; `Response::Png(Vec<u8>)` (→ becomes a handle).
  Encoding: `serde_json` + `bitcode`. Move the WS control link to **JSON text**
  (PNG bytes now go out-of-band), keep `bitcode` only where it already serves
  `.mesh.bin`.
- Frontend `packages/frontend/editor/src/`: `remote.rs` (WebTransport client,
  rewrite), MCP modal **inline in `app.rs`** (`open_mcp_modal()` ~L827), Settings
  drawer in `app.rs` (~L483), an existing `engine::activity_feed`, an activity
  chip in `app.rs` (~L790). **No help modal** (only an About dialog ~L555).
- `.mcp.json` points agents at `http://127.0.0.1:9086/mcp` — unchanged by this work.
- Toasts come from `awsm_web_shared::prelude::Toast` (`Toast::render()` mounted in
  `main.rs`); MCP toasts fire from `remote.rs`.

### Settled before implementation (no open questions)

- **GitHub org**: everything lives under **`awsm-fun/awsm-renderer`** — install
  one-liners, CI artifacts, and `Cargo.toml repository`. (`Cargo.toml` already
  fixed in plan prep: `repository = https://github.com/awsm-fun/awsm-renderer`,
  and the `homepage` field was removed to match audio.)
- The deployed editor is the Cloudflare Pages project **`awsm-scene-editor`**
  (`awsm-scene-editor.pages.dev`) — use that if a homepage/editor URL is ever
  needed; do **not** reintroduce a `dakom.github.io` homepage.

---

## Phase 0 — Rename + single port (foundation)

- [x] Rename package + bin `awsm-renderer-mcp` → `awsm-scene-mcp` in
      `packages/mcp/Cargo.toml` (`[package] name`, `[[bin]] name`). Also renamed
      the crate-name occurrences in `main.rs` (`//!` header, clap `name`, the
      `awsm_scene_mcp=debug` env filter). Left the `description`'s WebTransport
      wording for Phase 1.
- [x] Update every reference: `taskfiles/mcp.yml` (run/build/install `-p` + echo),
      `README.md` (diagram + prose binary name), `docs/MCP.md`, `docs/DEVELOPMENT.md`,
      and the editor's MCP-modal blurb (`app.rs`). NOTE: `.env.example` has **no**
      MCP refs (nothing to change); root `Taskfile.yml`'s `mcp-dev` desc references
      only port 9086, not the binary name, so it needed no change. The WebTransport
      *architecture prose* in README/docs/MCP.md is intentionally left for Phase 1.
- [x] Collapse to one port: dropped `PORT_MCP_QUIC_DEV` from `taskfiles/config.yml`;
      `PORT_MCP_HTTP_DEV: 9086` is the single MCP port; `mcp:serve` now passes a
      single `--port`. (`.mcp.json` unchanged at `…:9086/mcp`.) NOTE: `main.rs`
      still has `--client-port`/`--browser-port` (removed in Phase 1), so
      `task mcp:serve` won't successfully launch until Phase 1 wires `--port` —
      `cargo check`/`build` are unaffected. Transient by design.
- [x] `cargo fmt --all` + `cargo check -p awsm-scene-mcp` green (still WebTransport
      at this point — rename/port only). Side-effect of the homepage-removal flag
      fix: 12 member crates had `homepage.workspace = true`; stripped it from each
      so the workspace still loads after the root `homepage` field was removed.
- [x] Commit: `mcp: rename awsm-renderer-mcp -> awsm-scene-mcp, single port`.

## Phase 1 — Transport: WebTransport/QUIC → WebSocket

**Split decision (deviation from the original wording):** to keep Phase 1 a pure
*transport swap* and avoid throwaway scaffolding, the pairing/isolation envelope
(`Pair`/`PairingRequired`/`Detached`) and the `Connection`/`AgentSession` model
are **deferred to Phase 2**. Phase 1 keeps the existing single-attached-editor
semantics (last-attach wins; every MCP agent shares the one editor), just over a
WebSocket. The WS envelope here is therefore `WsServerMsg::Request{id,req}` +
`WsClientMsg::{Response{id,resp}, Event}` only. Phase 2 extends the envelope and
the link with pairing + the frontend pairing UI.

**Server (`packages/mcp`)**
- [x] Deleted `src/quic.rs` and `src/cert.rs`.
- [x] `Cargo.toml`: removed `web-transport`, `rustls`, `rcgen`, `sha2`, plus the
      cert-only `time` + `bytes`; added `futures`; `axum` now `features = ["ws"]`.
      Kept `base64` (glb/png encode) + `uuid` (id parsing; pair codes in Phase 2).
- [x] New `src/ws.rs` adapted from audio: `/editor` upgrade + **single-writer
      task** owning the sink; reader demuxes `Response`/`Event`.
- [x] Rewrote `src/http.rs`: kept `/mcp`, `/debug`, `/health`, `/boot-error`;
      **replaced `/control`** with the `/editor` ws route; kept PNA CORS. (`/png`
      side-channel added in 1b below.)
- [x] `src/main.rs`: one axum server on one port; QUIC listener gone; single
      `--port` (default 9086); dropped the rustls provider install.
- [x] Idle session timeout: **day-long** rmcp `session_config.keep_alive` so a
      live-but-idle agent isn't reaped (audio's lesson).

**Protocol (`packages/crates/editor-protocol/src/transport.rs`)**
- [x] Added `WsServerMsg::Request{id,req}` + `WsClientMsg::{Response{id,resp},
      Event(EditorEvent)}` (exported from `lib.rs`). `Pair`/`PairingRequired`/
      `Detached` deferred to Phase 2 (see split decision).
- [x] Link is now **JSON text frames** over the WebSocket; `bitcode` stays only
      for the `.mesh.bin` side file (untouched).
- [x] `Response::Png(Vec<u8>)` → `Response::Png(PngHandle { id, byte_len, width,
      height })` (bytes ride the side-channel — see 1b).
- [x] Added `ws_envelope_roundtrips` test (ser→de→ser byte-stable for the new
      frames + the PNG handle); existing Request round-trip tests still pass.

**Editor (`packages/frontend/editor`)**
- [x] `Cargo.toml`: removed `web-transport`; `gloo-net` now `["http",
      "websocket"]`. (`uuid` v4 already present — used to mint png ids.)
- [x] Rewrote `src/remote.rs` on audio's model: dial `ws://<origin>/editor`
      (derived from the http origin), single read loop dispatching `Request` →
      `EditorController`, single writer (mpsc) for responses/events,
      exponential-backoff reconnect (kept the existing retry-forever dev
      behaviour), `disconnect()`. All cert-hash/`/control` fetch gone.
- [x] Kept the reactive state the UI binds (`status`, `working`, `origin`).
      `pairing_needed`/pair-code state is Phase 2 (TLS toggle is Phase 4).

### Phase 1b — PNG HTTP side-channel

- [x] Server `http.rs`: `POST /png/{id}` (editor uploads raw PNG → temp file),
      `GET /png/{id}` (download), with **LRU eviction** (cap 32, delete oldest),
      256 MiB body cap. `png_path()` is `pub(crate)` so the tool reads it back.
- [x] Editor `remote.rs`: on `ScenePng`/`MaterialPng`/`TexturePng`, render as
      today, decode the data-url, **POST bytes to `/png/{id}`**, return only the
      `PngHandle` (with PNG-IHDR-parsed width/height) over the link.
- [x] rmcp tool layer (`mcp.rs`): `png()` reads the bytes back from
      `crate::http::png_path(&handle.id)` (same process; no HTTP self-call) and
      returns `Content::image()` — no base64 PNG over the control link.
- [x] Verify: `cargo fmt --all`, `cargo clippy --all --all-features --tests -D
      warnings`, `cargo test --all-features` (incl. `ws_envelope_roundtrips`) all
      green; `cargo build -p awsm-scene-mcp` ok. Live screenshot/two-tab checks
      are **manual** (Phase 6) — needs a browser + agent session.
- [x] Grep guard: `web-transport`/`rcgen`/`/control`/`browser-port` gone from the
      tracked source tree. Residue is out of scope: the gitignored
      `dist/awsm-editor-*.js` (regenerated build glue with web_sys WebTransport
      bindings) and `quinn` in `Cargo.lock` — the latter was already present at
      HEAD via `reqwest` ← `awsm-debugging` (http3 optional dep, not activated),
      unrelated to our removed WebTransport stack.
- [x] Commit: `mcp: replace WebTransport with WebSocket + PNG side-channel`.

## Phase 2 — Isolation: multi-tab + pairing codes

Today the server holds a single editor session (last-connect-wins, established in
Phase 1). Adopt audio's per-tab binding so multiple tabs never cross streams.

- [ ] **Envelope extension (deferred from Phase 1):** add `WsServerMsg::{PairingRequired,
      Detached}` and `WsClientMsg::Pair{code}` to `transport.rs` (+ exports), and
      handle them in server `ws.rs` and frontend `remote.rs`.
- [ ] `link.rs`: model `Connection` (one `/editor` ws = one tab, own request-id
      space + pending map) and `AgentSession` (one MCP client, own 4-char
      **Crockford base32** pair code), bound to each other via **`Weak`** pointers
      (a dropped agent auto-frees its tab — self-healing).
- [ ] `resolve(agent)`: return the live binding; else **auto-bind** when exactly
      one unbound tab **and** one unbound agent exist; else
      `Err(PairingRequired(code))`.
- [ ] `bind_by_code(conn, code)`: tab claims a specific agent; case-insensitive;
      frees any prior binding.
- [ ] Events are tagged with their originating connection id so only the bound
      agent receives them. On tab drop, drain pending requests so awaits resolve.
- [ ] rmcp layer surfaces `PairingRequired` to the agent (clear message telling it
      a code is needed) and shows the code so the human can type it.
- [ ] Frontend: per-tab id in `sessionStorage`; honor `?pair=<code>` URL param and
      a modal pair-code field; `submit_pair_code()` sends `Pair{code}` on the live
      socket (or stashes + connects). Show the pair-code field only when
      `pairing_needed`.
- [ ] Verify two-tab disambiguation (Phase 6). Commit:
      `mcp: per-tab isolation via pairing codes`.

## Phase 3 — CI / release (cargo-dist), installable binaries

- [ ] Root `Cargo.toml` `[workspace.metadata.dist]` (match audio):
      `cargo-dist-version` (use audio's or newer stable), `ci = "github"`,
      `installers = ["shell", "powershell"]`,
      `targets = ["aarch64-apple-darwin","x86_64-apple-darwin","x86_64-unknown-linux-gnu","x86_64-pc-windows-msvc"]`,
      `pr-run-mode = "plan"`, **`precise-builds = true`** (build only
      `awsm-scene-mcp`, never the wasm-only crates), `install-path = "CARGO_HOME"`,
      `install-updater = false`.
- [ ] `packages/mcp/Cargo.toml`: `[package.metadata.dist] dist = true`.
- [ ] Generate `.github/workflows/release.yml` via cargo-dist (`dist init` /
      `dist generate`), tag trigger `**[0-9]+.[0-9]+.[0-9]+*`. Keep the existing
      `test.yml` untouched.
- [ ] `Taskfile.yml`: add a `bump` task (set `[workspace.package].version`, sync
      internal dep versions, refresh `Cargo.lock`) and a full `publish` task
      (bump → commit-if-changed → annotated tag → `crates:publish` → editor deploy
      → `git push` + push tag). Adapt from audio.
- [ ] `README.md`: install one-liners under **`awsm-fun/awsm-renderer`** with the
      **`awsm-scene-mcp-installer.sh` / `.ps1`** names + `cargo install --git … awsm-scene-mcp`.
- [ ] `docs/RELEASING.md`: document the three tracks (editor → Cloudflare Pages,
      crates → crates.io, MCP binary → GitHub Releases via tag).
- [x] `Cargo.toml` `repository` set to `awsm-fun/awsm-renderer` and `homepage`
      removed (done in plan prep).
- [ ] Verify `dist plan` succeeds locally (no live release). Commit:
      `ci: cargo-dist release for awsm-scene-mcp + install one-liners`.

## Phase 4 — UI improvements

- [ ] Extract the MCP modal out of `app.rs` into `ui/mcp_modal.rs` (adapt audio).
      When **connected**, the MCP button **opens the modal** (showing Disconnect),
      not an immediate disconnect.
- [ ] Modal contents: live status banner, origin input, **pair-code field** (only
      when pairing needed), **TLS checkbox** (`wss`/`&tls=true` for remote
      servers), **Help button** that opens the help modal at the MCP tab, and a
      **"Live work display"** section with toggles: *show action label*,
      *auto-follow + spotlight*, *show activity feed* — persisted per tab in
      `localStorage` (audio keys: `awsm.mcp.show_action_label`, `.auto_follow`,
      `.show_feed`).
- [ ] `mcp_activity.rs` (new, adapt audio): action-label state, working pulse,
      auto-follow/spotlight, rolling feed. Reconcile with the existing
      `engine::activity_feed` + the chip in `app.rs` (extend, don't duplicate).
- [ ] Activity chip (🤖 + current action) sits next to the MCP button when
      connected and pulses while the agent works.
- [ ] **Help modal** (`help_modal.rs`, adapt audio): add a top-bar **Help button**
      and an MCP section — what it is, install the server, run it, connect this
      editor (`?mcp=`/`?pair=`/`&tls=`), point your agent at `…/mcp` (Claude
      Code/Codex/Cursor), watch it work. Tab is deep-linkable from the modal's
      Help button. Adapt copy/sections to the scene editor (not audio).
- [ ] **Notifications**: add a Settings toggle **"Show MCP notifications"**
      (default on) that gates the MCP `Toast::*` calls in `remote.rs`; add a
      **dismiss-all** control for visible toasts (check `awsm_web_shared`'s Toast
      API for a clear-all; extend the shared crate if it lacks one).
- [ ] Verify in-browser (Phase 6). Commit: `editor: MCP modal, activity, help, notification controls`.

## Phase 5 — MCP server best-practices pass (judgment)

**Decided**: this phase is intentionally judgment-based — do NOT stop to ask which
conventions to adopt. Research current rmcp/MCP best practice, then adopt what
genuinely helps the scene editor and record what you adopted/rejected. Candidates
observed in audio (keep, drop, or improve per judgment — and fix `audio` later if
the scene editor finds a better pattern):

- [ ] **Errors never silent**: tools that act by id error clearly when the id
      exists nowhere, instead of a silent ok.
- [ ] **Atomic batches with symbolic refs**: renderer already has
      `DispatchBatch`; consider audio's `ref`/`$ref` symbolic ids so a multi-step
      build is one undo entry and later commands reference just-created ids.
- [ ] **`Flexible<T>` params**: accept a bare string *or* a full typed object for
      common tool args, to cut agent ceremony.
- [ ] **`detail:"ids"` snapshot slimming**: let the agent ask for a light scene
      snapshot (ids/kinds/wires, counts) vs the full tree, to control payload size.
- [ ] **Tool docs**: ensure each tool's description states what it does, args +
      defaults, return shape, caveats, and cross-tool guidance.
- [ ] Write down what was adopted/rejected and why (short note here or in
      `docs/`). Commit: `mcp: adopt tool-layer best practices`.

## Phase 6 — Verification (run after each major phase, and a full pass at the end)

- [ ] `task fmt` (or `cargo fmt --all`), `cargo clippy --all --all-features --tests -- -D warnings`, `cargo test --all-features`.
- [ ] `cargo build -p awsm-scene-mcp`; run `task mcp:serve`; confirm `/health` ok.
- [ ] Open the editor with `?mcp=http://127.0.0.1:9086`; confirm WS connect, green
      status, activity chip.
- [ ] Point an MCP client at `http://127.0.0.1:9086/mcp`; run a mutating tool and a
      `ScenePng` screenshot tool — confirm the PNG returns via `/png/{id}` and the
      change is visible live.
- [ ] **Two-tab test**: open two editor tabs, start a second agent, confirm
      `PairingRequired` + that a typed pair code binds the right tab with no
      cross-talk.
- [ ] `dist plan` succeeds. Confirm `grep -rinE 'web.?transport|quinn|rcgen|rustls|/control|browser-port' packages/` is clean (only intended hits).
- [ ] Update `README.md`/`docs` so the documented flow matches reality.

---

## Done = all phases checked, CI green, two-tab + PNG round-trips verified, and `web-transport`/`quinn`/`rcgen` fully gone from `Cargo.lock`.
