# Plan: a template repo for the MCP server

**Status:** planned · **Depends on:** the MCP work on branch `mcp` (the server,
the editor connect button/modal, the PNA opt-in).

## Goal

A tiny, separate repo (working name **`awsm-mcp`**) that lets anyone drive the
**hosted** awsm-renderer editor from an MCP agent **without cloning
`awsm-renderer` or building the WASM frontend**. They get the local
`awsm-mcp-server` binary + a ready `.mcp.json`, run one command, open the hosted
editor in a Chromium browser, and click **Connect**.

This repo *is* the directory the user runs their MCP agent from — its `.mcp.json`
is what points the agent at the local server.

## Target end-user experience

```bash
# one-time: install the server (prebuilt binary — no Rust)
curl -fsSL https://github.com/dakom/awsm-renderer/releases/latest/download/awsm-mcp-server-installer.sh | sh

# each session, from this repo's directory:
task serve            # runs awsm-mcp-server on :9086 (HTTP/MCP) + :9087 (WebTransport)
# → open the hosted editor, click the link icon in the top bar → Connect
# → run your MCP agent here; it picks up ./.mcp.json automatically
```

No frontend build, ever. No Rust if using prebuilt binaries.

## Why this works (recap of what's already proven on branch `mcp`)

- The editor **dials out** to the local server over WebTransport/QUIC. A public
  HTTPS page reaching `127.0.0.1` was **verified in real Chrome** once the server
  sends the Private Network Access opt-in (`allow_private_network(true)`, already
  in [`packages/mcp/src/http.rs`](../../packages/mcp/src/http.rs)). No relay needed.
- **Nothing hardcodes the editor's public URL** — the editor talks to
  `127.0.0.1:9086` (editable in the connect modal). So the editor can be hosted
  anywhere (GitHub Pages, Cloudflare Pages, …) with zero server-side config.
- Constraint: **Chromium only** (Chrome/Edge) — both WebTransport
  `serverCertificateHashes` and WebGPU are Chromium-first. (Same constraint the
  editor already has via WebGPU, so nothing new.)

## Template repo contents

```
awsm-mcp/
├── README.md          # the quickstart (outline below)
├── Taskfile.yml       # install / serve / open
├── .mcp.json          # agent → local server  (the key artifact)
├── .gitignore         # ignore ./bin/ (downloaded binary)
└── LICENSE
```

`.mcp.json` (verbatim — this is what makes an agent in this dir drive the editor):

```json
{
  "mcpServers": {
    "awsm-editor": { "type": "http", "url": "http://127.0.0.1:9086/mcp" }
  }
}
```

## Delivering the server binary

`awsm-mcp-server` lives at [`packages/mcp`](../../packages/mcp) in the main repo
(`publish = false`). Its dependency tree is **native-only** (editor-protocol,
scene-schema, web-transport/quinn, rmcp, axum) — no wasm — so it builds and
installs standalone. Three delivery options, in preference order:

### A. Prebuilt binaries via the main repo's Releases — *recommended* (no Rust for the user)

Set up [**cargo-dist** ("dist")](https://opensource.axo.dev/cargo-dist/) in
`awsm-renderer`:

- `dist init`, scoped to release **only** `awsm-mcp-server` (dist builds and
  attaches binaries to a GitHub Release; it does **not** publish to crates.io, so
  `publish = false` is fine).
- Targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
  `x86_64-unknown-linux-gnu` (+ `aarch64-unknown-linux-gnu` optional),
  `x86_64-pc-windows-msvc`.
- dist generates the release workflow + a `curl … | sh` installer (Unix), a
  `irm … | iex` installer (Windows), checksums, and a `cargo binstall`-compatible
  manifest. Optionally a Homebrew tap.
- The template's `task install` just runs the dist installer one-liner (or
  downloads the platform asset directly).

### B. `cargo install --git` — zero release infra, needs Rust

```bash
cargo install --git https://github.com/dakom/awsm-renderer awsm-mcp-server
```

Works today (publish=false doesn't block git installs). Compiles only the native
server tree. Slower first run; requires a Rust toolchain. Good fallback / pre-CI.

### C. `cargo binstall awsm-mcp-server`

Fetches the prebuilt binary if cargo-dist published a binstall manifest (A),
otherwise compiles. Nice middle ground for Rust users.

**Recommendation:** do **A** (cargo-dist) for the no-Rust experience; document
**B** as the no-release-needed fallback in the README.

## Changes needed in the main repo (`awsm-renderer`)

1. **Release pipeline.** Add cargo-dist config + the generated
   `.github/workflows/release.yml` to build + attach `awsm-mcp-server` binaries on
   `v*` tags. Scope dist to the one binary.
2. **Verify standalone install.** Confirm `cargo install --git … awsm-mcp-server`
   builds clean from a fresh checkout (should — native-only deps).
3. **Optional `--allow-origin` flag** (security; see below). Default `Any` for
   zero-config; lets the template lock CORS to the editor origin. PNA header stays.
4. **Deploy the MCP-enabled editor.** The hosted editor must be built from branch
   `mcp` (it has the connect button + modal). Update the Pages / Cloudflare deploy,
   and record the canonical hosted URL for the template README + `EDITOR_URL`.
5. *(Optional)* `--editor-url` convenience: on start, the server prints
   `open <editor-url> and click Connect`.

## Template Taskfile (sketch)

```yaml
version: "3"
vars:
  HTTP_PORT: 9086
  QUIC_PORT: 9087
  EDITOR_URL: https://<hosted-editor-url>   # filled from the main-repo deploy
  BIN: ./bin/awsm-mcp-server

tasks:
  install:
    desc: "Install awsm-mcp-server (prebuilt binary; falls back to cargo)"
    # 1) try the cargo-dist installer / download the latest release asset for this
    #    OS+arch into ./bin/, else 2) cargo install --git … awsm-mcp-server
  serve:
    desc: "Run the MCP server (open the hosted editor, then click Connect)"
    cmds:
      - '{{.BIN}} --http-port {{.HTTP_PORT}} --quic-port {{.QUIC_PORT}}'
  open:
    desc: "Print/open the hosted editor URL"
    cmds:
      - 'echo "Open {{.EDITOR_URL}} and click the link icon → Connect"'
```

## README outline (user-facing)

1. **What this is** — drive the hosted awsm-renderer editor from an MCP agent via
   a small local server; one paragraph on the flow.
2. **Prerequisites** — a Chromium browser (Chrome/Edge); no Rust if using prebuilt.
3. **Install** — the one-liner (cargo-dist installer) + the `cargo install --git`
   alternative.
4. **Run** — `task serve`.
5. **Connect** — open the hosted editor URL, click the link icon in the top bar,
   **Connect** (address is pre-filled).
6. **Point your agent here** — Claude Code/Desktop auto-pick `./.mcp.json`; Codex
   and others register the streamable-HTTP URL `http://127.0.0.1:9086/mcp`.
7. **What you can do** — link to
   [`awsm-renderer/docs/MCP.md`](https://github.com/dakom/awsm-renderer/blob/main/docs/MCP.md)
   for the tool catalog + details.
8. **Troubleshooting** — "no editor attached" (open + Connect the editor tab),
   must be a Chromium browser, server must be running, restart re-mints the cert
   (editor reconnects automatically).

## Security note

Default CORS is `Any` for zero-config — combined with the PNA header, *any* site
you browse while the server runs can reach `127.0.0.1:9086`. For a localhost dev
tool the blast radius is small (only your local scene). Optional hardening: the
main-repo `--allow-origin https://<editor>` flag (item 3) narrows CORS to the
known editor origin(s) while keeping the hosted editor working; the template
Taskfile can pass it. The PNA header stays either way (removing it would break the
hosted→local flow entirely; it's inert for all-local use).

## Versioning / keeping editor + server in sync

The hosted editor and the local server both build from `awsm-renderer` and share
the `awsm-editor-protocol` wire types. A Pages editor and a server binary from
**different commits could drift** (a new command variant on one side the other
doesn't know). Mitigations:

- Cut the editor deploy and the server release from the **same tag**.
- *(Future hardening)* have `/control` report a protocol/version string and have
  the editor surface a "server/editor version mismatch" toast on connect.

For v1, "use the latest of both, released together" is sufficient — document it.

## Open questions

- **Repo name** — `awsm-mcp` / `awsm-editor-mcp` / other?
- **Primary delivery** — cargo-dist prebuilt binaries (A) as the documented
  default, or keep `cargo install --git` (B) as the headline until releases exist?
- **Cross-platform matrix** — which OS/arch to ship (macOS arm64 is a must; how
  much Linux/Windows coverage)?
- **CORS default** — stay `Any` (zero-config) or ship the `--allow-origin`
  allowlist on by default?
- **Version pinning** — template pins a server version, or always "latest"?
- Is it a GitHub **template repository** (the "Use this template" button), or just
  a small quickstart repo people clone/copy?

## Checklist

Main repo (`awsm-renderer`):
- [ ] cargo-dist init + release workflow scoped to `awsm-mcp-server`
- [ ] tag a release; verify binaries + installer assets attach
- [ ] verify `cargo install --git … awsm-mcp-server` from a clean checkout
- [ ] *(optional)* `--allow-origin` CORS flag
- [ ] deploy the branch-`mcp` editor; record the hosted URL

Template repo (`awsm-mcp`):
- [ ] create repo: `README.md`, `Taskfile.yml`, `.mcp.json`, `.gitignore`, `LICENSE`
- [ ] `install` task: download release asset (or dist installer) + `cargo install --git` fallback
- [ ] `serve` / `open` tasks; fill `EDITOR_URL`

End-to-end:
- [ ] fresh machine → install → `task serve` → open hosted editor → Connect →
      drive via an MCP agent reading `./.mcp.json`
</content>
