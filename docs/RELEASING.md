# Releasing awsm-renderer

This repo ships **three independent release artifacts**, each on its own track —
releasing one doesn't touch the others:

| # | Artifact | Trigger | Destination |
|---|----------|---------|-------------|
| 1 | **Frontend editor** | `task editor:deploy` | Cloudflare Pages (`awsm-scene-editor` → scene.awsm.fun) |
| 2 | **Library crates** (`awsm-scene`, `awsm-renderer`, …) | `task crates-publish` | crates.io |
| 3 | **MCP server** (`awsm-scene-mcp`) | push a `v<version>` git tag | GitHub Releases |

Versions across the workspace move in lockstep (`version` under
`[workspace.package]` in the root `Cargo.toml`, mirrored into the internal
`awsm-*` dep reqs in `[workspace.dependencies]`), but the three tracks publish
independently.

The one-shot `task publish -- <version>` folds tracks 1 + 2 together and tags
track 3 — see [§ Cut a release](#cut-a-release-all-three-tracks).

---

## 1. Frontend editor → Cloudflare Pages

```sh
task editor:deploy     # production trunk build + wrangler deploy (project: awsm-scene-editor)
```

Needs `CLOUDFLARE_DEPLOY_WORKERS_TOKEN` (+ account/zone ids) in the repo-root
`.env`; the project name and branch come from `taskfiles/config.yml`. Deploys are
**manual** (run the task, or `task publish` which folds it in) — there is no CI
auto-deploy. (`task deploy` deploys both the editor and the model tester.)

## 2. Library crates → crates.io

```sh
task crates-publish-dry-run     # package + verify, upload nothing
task crates-publish             # publish for real
```

`cargo publish --workspace` publishes every member in dependency order and skips
the `publish = false` members (the frontends, `awsm-web-shared`, `debugging`, and
the MCP server). The MCP server is **not** a crates.io crate — it ships as a
binary (track 3).

## 3. MCP server → GitHub Releases

The native MCP server ships as prebuilt binaries on **GitHub Releases**, driven by
[cargo-dist](https://opensource.axo.dev/cargo-dist/) (the `dist` CLI). A release is
triggered by pushing a **version git tag**; CI builds every platform and publishes
the binaries plus the `curl`/PowerShell installers.

### Cut a release (all three tracks)

The wrapper does everything from a clean tree:

```sh
task publish -- 0.3.0
```

That runs: `bump` → commit (only if the version changed) → annotated `v0.3.0` tag
→ `crates-publish` → `editor:deploy` → `git push` + push the tag. Pushing the tag
is what starts the **Release** workflow (`.github/workflows/release.yml`) that
builds the MCP binaries.

To cut **only** the MCP-server binary release (skip crates + editor):

1. **Bump + commit** the version (`task bump -- 0.3.0`, then commit `Cargo.toml` +
   `Cargo.lock`).
2. **Sanity-check the dist plan** (optional, no network):

   ```sh
   dist plan          # shows the artifacts/installers that will be produced
   ```

3. **Tag and push** — the tag must be `v<version>` and match the Cargo version:

   ```sh
   git tag -a v0.3.0 -m "awsm-scene-mcp v0.3.0"
   git push origin v0.3.0
   ```

   Watch it with `gh run watch` or on the Actions tab.

4. **Verify** once it goes green:

   ```sh
   gh release view v0.3.0                       # binaries + installers attached
   ```

The tag is what matters, not the branch — release from `main` once a change has
landed there.

### What the workflow produces

A published GitHub Release at `…/releases/tag/v<version>` with:

- per-platform archives: macOS arm64 + x86_64, Linux x86_64, Windows x86_64-msvc
  (`.tar.xz` / `.zip`) plus `.sha256` checksums,
- `awsm-scene-mcp-installer.sh` (the `curl … | sh` installer) and
  `awsm-scene-mcp-installer.ps1` (PowerShell).

The README's install commands all point at `releases/latest/download/…`, so they
keep working across versions with no edits.

### The dist config

- **`[workspace.metadata.dist]`** in the root `Cargo.toml` holds the dist config
  (targets, installers, `precise-builds`). **`precise-builds = true`** is
  important: it builds only the `awsm-scene-mcp` package, so dist never tries to
  host-compile the wasm-only editor crate. The MCP crate opts in with
  `[package.metadata.dist] dist = true` (it's `publish = false`, which dist
  otherwise treats as "don't ship").

After editing that config, regenerate the CI workflow so it stays in sync:

```sh
dist generate       # rewrite .github/workflows/release.yml from the config
```

Commit the regenerated `release.yml` alongside the config change. Bumping the
pinned `cargo-dist-version` is how you upgrade the toolchain CI uses.
