# docs/plans/todo.md — renderer feature work (status SSOT)

> **Status (2026-06-23).** The **custom vertex shaders** feature and the
> self-contained **multithreaded-renderer** items are **shipped in PR #138**
> (branch `more-mcp`). The only remaining item is **device-loss + worker-crash
> recovery (B1) + its verification (T4)** — moved to its own pickup doc:
> [`device-loss-recovery.md`](./device-loss-recovery.md).
>
> Provenance: consolidated `custom-vertex.md`, `multithread-build-plan.md`,
> `multithread-testing.md` (all superseded). `mcp-improvements.md` shipped in
> PR #137. `nanite.md` is intentionally separate / out of scope.

## Master tracker

| id | item | status | commit / ref |
|----|------|--------|--------------|
| CV1 | Custom vertex: ABI + `apply_vertex` hook + geometry & shadow per-material pipelines + registration/cache-key + naga validation | DONE | `ab181114..a6d8466d` |
| CV2 | Custom vertex: transparent + geometry-masked + shadow-masked + combined masked+custom-vertex variants | DONE | `93f62a64`, `8a25e94c`, `02ebee03` |
| CV3 | Custom vertex: editor 3rd WGSL window + `set_material_vertex_wgsl` MCP + `get_material_contract` vertex mode + contract doc + starter body | DONE | `46b88c17`, `fe7a8ec0` |
| CV4 | Custom vertex: full multi-UV ABI (all sets, per-vertex) + multi-texture + `recompute_normal_from_height` helper | DONE | `6e35b125`, `cf5fb12b` |
| B2 | Multithread: screenshot capture path (`renderer.capture_frame`) | DONE | `4c593c02` |
| T3 | Multithread: perf at scale + soak | DONE | verification — see below |
| T5 | Multithread: allocation / GC validation | DONE | verification — see below |
| B3 | Multithread: arena growth policy (conditional) | DONE (N/A) | not needed — soak proved memory bounded |
| B4 | Multithread: bundled scene fixture for `?demo=scene` | DONE | `eec614d2` |
| **B1** | **Multithread: device-loss + worker-crash recovery** | **TODO** | [`device-loss-recovery.md`](./device-loss-recovery.md) |
| **T4** | **Multithread: resilience verification (after B1)** | **TODO** | [`device-loss-recovery.md`](./device-loss-recovery.md) |

The custom-vertex authoring contract (the living reference for writing vertex
WGSL) is [`../dynamic-materials/contract-vertex.md`](../dynamic-materials/contract-vertex.md).

## Verification results

Live-measured via the chrome-devtools MCP + the awsm-scene MCP.

**Custom vertex (live-verified in the editor):**
- A custom-vertex material **displaces a mesh and its shadow matches** the
  displaced silhouette (geometry + shadow pipelines run the identical hook).
- **UV-driven** displacement works on opaque + transparent (a `sin(input.uv[0].y)`
  body renders latitude bands; all of the mesh's UV sets are read per-vertex).
- A **Mask + custom-vertex** material renders **displaced AND alpha-cut** (a
  checkerboard cutout on a displaced sphere), drawn exactly once (precedence:
  combined > custom-vertex > masked > solid).

**No perf regression on the non-custom path** (motion demo, render worker):
the regression-sensitive render-limited point matches baseline —
`N=10000` → **~42.9 ms** (baseline 41.7 ms, within noise; this point stresses the
per-mesh additions hardest). The non-custom routing additions are cheap field
reads + a HashMap-miss (`has_vertex_shader`, no per-frame alloc); lower-N numbers
this session are 60 Hz-vsync-capped and would only show a per-mesh regression at
high N, which they don't. (Earlier baseline curve, 120 Hz display: 120 fps to
~2000 movers; 60 fps / 16.6 ms budget crossover at ~5000; ~24 fps at 10000.)

**T3 — memory soak (churn demo, ~59 min):** spawned 2447 / despawned 2435 /
**reusedSlots 2431 (99.84 % of freed slots reused)**, `invariantOk` held the whole
run → **shared-memory growth is flat/bounded** (so B3 is N/A).

**T5 — allocation / GC:** the O(N) render hot path uses pooled scratch
(`Transforms::arena_pack_scratch`; the cull path's `RenderFrameScratch`); no
O(meshes) per-frame `Vec`/`Box`. Motion ran a clean 600 frames / 5.00 s with
**zero dropped frames**.

**B2 — capture path:** `?demo=remote` Screenshot returns a PNG whose decoded image
matches the on-screen frame.

**B4 — scene fixture:** `?demo=scene` loads a real editor-exported `scene.toml`
via `scene_from_toml` → `load_scene_for_player`.

## Known minor follow-ons (non-blocking)
- The custom-vertex geometry/shadow pipelines still declare a vestigial
  `@location(10) uv0` vertex attribute + bind a shared zero buffer — UV now comes
  from the `visibility_data` storage pool (by `original_vertex_index`), so the
  attribute is unused. Harmless (custom-draw path only); removing it is a
  vertex-layout change, left out to avoid churning a verified feature.

## Dev environment

Two dev servers (each a background task you own; logs under `/tmp/`):

- **`task mcp-dev`** → editor (trunk, `:9085`) + the awsm-scene MCP (`:9086`).
  For editor/renderer/MCP work. Drive the editor + `awsm-scene` MCP through
  chrome-devtools at `http://localhost:9085/?mcp=http://127.0.0.1:9086&pair=<CODE>`.
  If the harness doesn't register the awsm-scene MCP tools, drive it via
  `/tmp/mcp.py` (memory `mcp-direct-http-client`).
- **`task mt:dev`** → the multithreaded renderer demos (`:9090`, COOP/COEP).
  Drive `http://localhost:9090/?demo=<name>` (`remote`, `crowd`, `churn`, `motion`,
  `scene`) through chrome-devtools.

**Build/restart cycle** (recurs whenever you touch the server or a crate):
- Free ports before relaunch (`lsof -ti tcp:PORT | xargs kill`): 9085/9086/9082/9083
  for mcp-dev; 9090 for mt:dev.
- Relaunch with `run_in_background: true` (NOT inline `&`). Wait for HTTP 200 +
  the "server listening" log line + a trunk "success".
- After a `task mcp-dev` server rebuild, reload the editor in chrome-devtools;
  **the MCP pair code rotates on restart** — the next tool call errors with the
  new code; navigate `?pair=<NEW CODE>` to re-pair.
- Harness CACHES the MCP tool/query schema across restarts → exercise NEW
  commands via `dispatch_command {command:{cmd:...}}` and NEW queries/fields via
  `run_query {query:{query:...}}`.

**Native tests:** renderer via `cargo test -p awsm-renderer` (custom-vertex naga
validation needs `--features dynamic-material-validation`); also `awsm-materials`,
`awsm-scene-loader`, `awsm-editor-protocol`, `awsm-editor`, `awsm-renderer-core`.
Lint gate: `task lint` (rustfmt + clippy `-D warnings`, all features, tests).

## Execution rules (for future work on this doc / B1+T4)
1. **One item at a time**, full scope — not a slice.
2. **Verify before DONE:** Rust tests + `task lint` clean + a **live** chrome-devtools
   gate proving the actual behavior.
3. **Commit per completed+verified item** (co-author trailer
   `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`); flip
   the tracker row + record the hash.
4. **Full scope or BLOCKED — never a silent slice.** If genuinely too large to
   finish+verify, mark BLOCKED with a specific reason; don't mark a partial DONE.
5. **Don't claim done** unless every row is genuinely DONE; write an honest summary.

## Already shipped (context, not work)
`mcp-improvements` (PR #137): raw texture upload, UV-transform, keyframe channels,
`patch_kind`/`get_kind_schema`, magenta unassigned sentinel, subtree/duplicate-id
queries, fused `paint_where`/`transform_where`, custom transparent/alpha, particle
sprites, the `ibl` include, `displace-from-texture`, and the agent equirect
panorama environment (skybox + SH diffuse IBL). Custom vertex shaders + the
multithread items above: PR #138.
