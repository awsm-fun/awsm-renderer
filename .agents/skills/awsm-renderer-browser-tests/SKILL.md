---
name: awsm-renderer-browser-tests
description: >-
  Run the renderer tests that require a real browser + GPU — the comprehensive
  three-layer suite (docs/plans/browser-test-suite.md): Layer A visual scenes
  (agent-as-oracle, one verify.md recipe per test-scene), Layer B the plan-007
  player-tests harness (machine-readable PASS/FAIL over the baked bundles on a
  live WebGPU device), and Layer C native editor/MCP audits (cargo test). Use
  whenever asked to "run the browser tests", "run the on-device / player tests",
  "run all our renderer tests", or to verify rendering that `cargo test` / CI
  structurally cannot cover.
---

# awsm-renderer browser test suite (three layers)

CI (`.github/workflows/test.yml` = `task lint` + `cargo test --all-features`)
runs pure Rust logic on a host with **no GPU and no browser**. Anything needing
a real WebGPU device, canvas, shaders, or the Basis worker can only be proven in
a browser and is therefore **not in CI**. This skill runs that tier, plus the
native audits that keep the authoring/MCP surface honest.

## The three layers

- **Layer A · Visual (agent-as-oracle).** For each feature, an authored
  `examples/test-scenes/<scene>/author.js` (reproducible) + a `verify.md` recipe
  (states to drive + what "correct" looks like in words). The runner opens the
  scene in a real browser, drives the states, screenshots, and **judges
  visually** — the agent's eyes are the oracle (no pixel-diff/SSIM harness, by
  design). A scene with no `verify.md` is visual-untested.
- **Layer B · Structural (`examples/player-tests`).** A wasm harness that loads
  every baked bundle through the *real* player loader on a live GPU and prints
  machine-readable `PLAYER-TEST <name>: PASS/FAIL — <detail>` lines. Locks the
  load-path / counts / texture-binding / compression behaviour.
- **Layer C · Editor/MCP audits (native `cargo test`, also runs in CI).** Not
  browser tests: "all mutations route through EditorCommand" + "MCP tools/docs
  stay in sync". Cheap; run first as a fail-fast.

## Scope prompt (ask first)

Before running, ask the user for scope with AskUserQuestion (single-select,
recommend `Everything`):

- **Everything** *(recommended)* — Layers C → B → A, in that order.
- **Visual only** — Layer A (every `verify.md`, or a picked subset).
- **Structural only** — Layer B (the player-tests harness).
- **Audits only** — Layer C (native `cargo test`, no browser needed).
- **Pick features…** — enumerate the Layer-A scenes (below) and let the user
  multiselect which `verify.md` recipes to run.

Enumerate what's available before prompting so "Pick features…" has a real list:

- Layer A = `ls examples/test-scenes/*/verify.md` (one per visual scene).
- Layer B = the `SCENES` list in `examples/player-tests/src/checks.rs` plus the
  prefab-churn / startup-census checks.
- Layer C = `cargo test -p awsm-renderer-scene-mcp` + the no-bypass lint test.

## Ports (from `taskfiles/config.yml` — do not guess, they have changed)

- editor = **9085**, MCP dev server = **9186** (Layer A authoring + capture)
- test-scenes bundles = **9084**, player-tests harness = **9091** (Layer B)
- media-local = **9082**, media-additional-assets = **9083** (imports)

---

## Layer C — native audits (run first, fast, no browser)

```
cargo test -p awsm-renderer-scene-mcp        # parity + wire audits
cargo test --all-features                     # includes the no-EditorController-bypass lint test
```

Report pass/fail per test. A failure here means the authoring/MCP surface
drifted (a command with no MCP tool/doc row, or a UI mutation bypassing
`EditorCommand`) — surface it, it is cheap to fix and blocks the rest.

## Layer B — player-tests harness (structural, machine-readable)

1. **Start both servers** (the harness fetches bundles from test-scenes; `task
   player-tests` does *not* start it). Each with Bash `run_in_background: true`;
   skip any already serving (`curl -s -o /dev/null -w '%{http_code}'
   http://127.0.0.1:9084/`):
   ```
   task test-scenes     # http-server on :9084 (bundles)
   task player-tests    # trunk serve on :9091 (builds + serves the wasm harness)
   ```
2. **Wait for the harness to build.** `trunk serve` compiles wasm — the first
   build takes a while. Poll until `http://127.0.0.1:9091/` returns 200 AND a
   `*_bg.wasm` is being served (a plain 200 on `/` can precede wasm being
   ready). Do not open the page before the build settles or it loads stale.
3. **Open a fresh tab** to `http://localhost:9091` via the browser MCP. Never
   reuse another session's tab. Filter scenes with `?scenes=a,b`; feed the
   nanite streaming budget with `?stream` / `?streambudget=N`.
4. **Wait for completion.** Poll the console until a line contains
   `PLAYER-TESTS COMPLETE:` (up to ~2 min — many scenes, real GPU uploads). It
   always emits that line, even on panic (`aborted (panic)`), so waiting never
   hangs.
5. **Read the result — do NOT dump the whole console** (huge; overflows
   context). `list_console_messages` saves to a file when large; grep it for
   `PLAYER-TEST |PLAYER-TESTS COMPLETE`.
6. **Report** `<pass>/<total>` + every `FAIL — <detail>` verbatim. PASS only
   when `<pass> == <total>` and no `FAIL`/`aborted` line appears.

## Layer A — visual scenes (agent-as-oracle)

For each selected `examples/test-scenes/<scene>/verify.md`:

1. **Bring up the editor + MCP:** `task mcp-dev` (editor :9085 + MCP :9186) plus
   the media servers `task media-local` (:9082) + `task media-additional-assets`
   (:9083) for import-backed scenes. Wait for the trunk build to settle.
2. **Open a fresh editor tab** at `http://localhost:9085/?mcp=http://127.0.0.1:9186`
   via Chrome DevTools MCP. Never reuse another session's tab.
3. **Read `verify.md`** — it has three sections: `drive:` (ordered steps to pose
   the scene), `expect:` (what CORRECT looks like, per state), `fail:` (the
   wrong-looking outcomes to reject).
4. **Drive** the scene. Replay the scene's `author.js` (or `load_project` the
   baked project) through `window.wasmBindings.editor_dispatch_json(json)` /
   `editor_query_json(json)` via `evaluate_script`. `editor_dispatch_json` is
   fire-and-forget (returns before apply) — settle with `editor_query_json({query:'wait_render_settled'})`
   before every capture. Use `editor_tick_animation` to advance the clock
   deterministically (shadows/animation states), `editor_query_texture_png` for
   texture reads.
5. **Capture** each named state: MCP `screenshot_scene` or Chrome DevTools
   `take_screenshot`. Decode base64 to a file; **never** route bulk bytes through
   MCP/control results (memory: `no-bulk-bytes-through-mcp-results`).
6. **Judge** each state against `expect:`/`fail:` and report **PASS/FAIL with the
   screenshot(s) and the reasoning** so a human can spot-check.

## Summary

One table across all layers (scene/check · layer · PASS/FAIL · note). Leave the
background servers running for re-runs (offer to stop them).

## Gotchas (all three layers)

- **mtime-blind watcher.** `trunk serve` rebuilds on real content change, not
  `touch`. If a rebuild looks stuck or the page is stale, append a real one-line
  comment to a watched crate to force it, then strip it. Hard-reload the tab
  (ignore cache) after any renderer edit. (memory: `stale-wasm-and-livereload-harness-law`)
- **Frozen tab ≠ broken renderer.** Blank screenshots / readbacks reading 0 / no
  `COMPLETE` after 2+ min often mean the Chrome tab is frozen. Restart the
  browser and re-open the tab before blaming the code. (memory: `cluster-gpu-cut-draws-zero-p0`)
- **WebGPU required.** A software/no-GPU browser fails uploads. If every scene
  fails at device/upload, check the browser, not the bundles.
- **Renderer tracing** (`tracing::info!/warn!`) surfaces in the **browser
  console**, not the editor's log buffer. (memory: `renderer-tracing-in-browser-console`)
- **Goldens** (`examples/test-scenes/<scene>/golden.png`) are window-dependent
  visual references, deliberately **not** byte-exact CI locks — regenerate by
  replaying `author.js`; explain any golden change in the commit.
