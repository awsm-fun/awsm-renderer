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
4. **Wait for completion via the DOM HUD, not the console.** The harness mirrors
   its aggregate to a `#hud` element (`report.rs::set_hud`) as well as the
   console, and the DOM read is pagination-free and robust — the preferred
   chrome-devtools path. Poll with `evaluate_script`
   (`() => document.getElementById('hud')?.textContent ?? ''`) until the text
   contains `COMPLETE` (up to ~2 min — many scenes, real GPU uploads). The HUD
   reads `player-tests: COMPLETE <pass>/<total>`; on a panic the console
   terminator is `PLAYER-TESTS COMPLETE: aborted (panic)`, so also break if the
   text stops advancing — waiting never hangs.
5. **Read the result — chrome-devtools only, never claude-in-chrome.**
   - *Aggregate* (`<pass>/<total>`): the `#hud` textContent from step 4 IS the
     result. If `<pass> == <total>` every check is green — done, no console read.
   - *Per-test FAIL detail* (only in the console, as `PLAYER-TEST <name>: FAIL —
     <detail>`): only needed when `<pass> < <total>`. `list_console_messages` has
     **no** server-side pattern filter and paginates ~20/page, so do NOT page the
     whole console. Instead, capture the lines in-page: BEFORE the run emits them,
     inject a hook with `evaluate_script` right after `navigate_page`
     (the wasm build+init buys a few seconds) —
     `() => { window.__pt = []; const o = console.log.bind(console); console.log = (...a) => { const s = a.join(' '); if (s.includes('PLAYER-TEST')) window.__pt.push(s); o(...a); }; return 'hooked'; }` —
     then after COMPLETE read `() => window.__pt` for the full array. If the hook
     missed the start, `list_console_messages` (it saves to a file when large;
     grep that file for `PLAYER-TEST .*FAIL`) is the bounded fallback for just the
     FAIL lines. This is the robust chrome-devtools-only substitute for a
     console pattern filter — do not reach for claude-in-chrome to read logs.
6. **Report** `<pass>/<total>` + every `FAIL — <detail>` verbatim. PASS only
   when `<pass> == <total>` and no `FAIL`/`aborted` line appears.

## Layer A — visual scenes (agent-as-oracle)

For each selected `examples/test-scenes/<scene>/verify.md`:

1. **Bring up the editor + MCP:** `task mcp-dev` — it already starts editor :9085
   + MCP :9186 **and** both media servers `media-local` :9082 +
   `media-additional-assets` :9083 as deps (import-backed scenes need them). Do
   NOT also launch `task media-local`/`media-additional-assets` standalone — they
   bind the same ports and the whole `mcp-dev` group panics on
   `Address already in use`. If you already have standalone media servers running
   from an earlier step, free 9082/9083 first (`lsof -ti :9082 -ti :9083 | xargs
   kill`) and let `mcp-dev` own them. Wait for the trunk build to settle
   (`grep '✅ success'` the task's log; poll `:9085` for 200).
2. **Open a fresh editor tab** at `http://localhost:9085/?mcp=http://127.0.0.1:9186`
   via Chrome DevTools MCP. Never reuse another session's tab.
3. **Read `verify.md`** — it has three sections: `drive:` (ordered steps to pose
   the scene), `expect:` (what CORRECT looks like, per state), `fail:` (the
   wrong-looking outcomes to reject).
4. **Drive** the scene. Replay the scene's `author.js` (or `load_project` the
   baked project) through `window.wasmBindings.editor_dispatch_json(json)` /
   `editor_query_json(json)` via `evaluate_script`. `editor_dispatch_json` AWAITS
   the command and returns `"ok"` / `"error: …"` / `"decode error: …"` — CHECK it
   (the `author.js` guards depend on it). It settles the COMMAND, not the frame,
   so still settle with `editor_query_json({query:'wait_render_settled'})`
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
- **Media servers die mid-session.** Import-backed scenes (Fox, Duck,
  AlphaBlendLabels, anything via `import_*_from_url`) fetch from :9082/:9083; if
  those crashed, a fresh `author.js` replay fails with "import did not settle" /
  blank textures while `load_project` (baked bytes) still works. Health-check
  before an import-backed replay: `curl -s -o /dev/null -w '%{http_code}'
  http://localhost:9082/` — a non-200 means restart the media tier (via
  `task mcp-dev`, which owns those ports — see Layer A step 1).
- **Import-then-bind races (author.js).** `import_texture_from_url` is
  fire-and-forget; binding the texture (or a decal) immediately after races the
  import and the slot renders blank on a fresh replay. Poll
  `save_census.texture_assets` until the count lands before binding (see
  `dynamic-material-textures/author.js`). Baked `project/` is unaffected (the
  texture settled at bake), so `verify.md`'s `load_project` drive stays green.
- **Procedural textures + repeated loads in ONE session.** Test scenes reuse
  fixed deterministic asset UUIDs, so driving many scenes through one editor tab
  with successive `load_project_from_url` collides asset ids. The load
  transaction now clears the stale texture-key/byte caches
  (`persistence::clear_stale_session_caches`), so a procedural checker renders
  correctly on the 2nd+ load — but if you see a proc texture render **white**
  after several loads, suspect a stale-key regression there (raster textures are
  masked by `restore_textures`; procedural ones regenerate lazily and expose it).
- **Animated CPU state settles a few frames after `set_playhead`.** Queries like
  `morph_data` read transitional values if fired immediately after
  `wait_render_settled`. Poll the query until it stabilizes (bounded, with a
  timeout that still throws on a real break) rather than a single eager read.
- **Goldens** (`examples/test-scenes/<scene>/golden.png`) are window-dependent
  visual references, deliberately **not** byte-exact CI locks — regenerate by
  replaying `author.js`; explain any golden change in the commit.
