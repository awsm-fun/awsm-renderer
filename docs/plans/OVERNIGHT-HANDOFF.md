# Overnight handoff — finish the mesh-pipeline + skin/morph/animation roadmap

**Branch:** `mesh-authoring`. **Authored:** 2026-06-11 (end of an interactive session).
**Purpose:** hand a fresh autonomous `/loop` run everything it needs to finish the
roadmap overnight without repeating this session's time-sinks. Source-of-truth
plan is still `docs/plans/mesh-pipeline-overhaul.md`; this doc is the *current
state + gotchas + the exact loop prompt*.

---

## 0. State right now (all committed, tree clean except 2 pre-existing diffs)

Pre-existing uncommitted diffs that are **NOT ours** — leave them alone:
`packages/crates/renderer/src/lib.rs`, `packages/crates/renderer/src/raw_mesh.rs`.

This session landed (newest first):
- `a2d0610b` style: rustfmt
- `f0dd0421` **bulb-glyph light icons + direction rays** (editor #16)
- `d384a072` **frame_globals in the masked-shadow pass** → animated procedural cutouts move their shadows
- `3303be95` **double-sided shadow casters** (`CullMode::None`) → thin cutout panels / planes cast hole-shaped shadows
- `cf352b30` **shadow Soft penumbra tamed + PCSS acne killed**, unified per-light **Softness** knob (`pcss_penumbra_scale` now drives Soft + PCSS; world-sized → texel-converted → scale-invariant)
- `65b63041` light-gizmo settings toggle + drag-to-scrub numeric inputs (editor #15/#17)
- `d623ca5b` multi-node drag-reparent into Empty (editor #14)

Editor fix batch (#14–#18) is DONE. Mesh **editing** (Phases 1–6 of mesh-editing) is
DONE + MCP-verified. Pipeline overhaul Phases 0–3, 7, 8 DONE.

---

## 1. Remaining work (priority order) — this is the overnight scope

### A. Phase 5 — Skin/morph editing via MCP (USER PRIORITY, biggest)
Cargo-verifiable backend; visuals deferred to browser. Build as NEW
commands+tools+queries (additive, safe at the command layer). Landscape already
surveyed in `mesh-pipeline-overhaul.md` Phase 5 notes:
- morph already exists as an ANIMATION TRACK target (`mcp.rs` `add_track morph(node,index)`);
- `drop_skinning` bakes skin→editable; scene types `SkinnedMeshRef`/`SkinJoint` in `scene/src/tree.rs`.
- BUILD: live `set_morph_weight(node,index,value)` + `get_morph_data` query (target count/names/current weights); skin joint-weight / bind-pose editing; richer skeletal/morph animation authoring. Find the renderer morph-weight API + how the animation morph track drives it, mirror that. "Pull out the stops — 3rd-party crates (IK, weight-smoothing, retarget) are fine."

### B. Phase 6 — Skin/morph visualization (editor UI; the "bones in outliner" ask)
- Bone/joint icons in the **outliner** tree for joint/skin nodes.
- Skeleton **bone-lines** in the viewport (+ morph visualization), including DURING animation playback.
- Builds on this session's `light_icons.rs` (a dedicated HUD-glyph renderer with
  picking + the settings-toggle pattern) and `outliner.rs` — read both first.

### C. Animation playback in the loader/editor
- The editor (:9085) does NOT play imported glTF clips — `set_playing`/`set_playhead` don't pose the scene (checklist.md item #2). Wire it.
- Separately, the loose player (:9080 `/app/model/<Name>`) animation freeze needs RE-VERIFY on a confirmed-fresh build (checklist.md ~line 55). The renderer chain is verified-correct; suspected stale-build artifact.

### D. Phase 4 — verify editor/player onto shared packer/convert (parity)
- Built; needs browser verification. Use the `load_player_bundle` round-trip
  (authored render vs runtime reload screenshot-compare).

### E. Small/cosmetic
- Read-only **vertex-selection highlight** in the viewport (`SetVertexSelection`
  render) — functional query already works.

### F. Phase 9 — STANDING LATITUDE (when the above is progressing/done)
Opportunistic high-value/low-risk: dead-code/doc cleanup, MCP robustness +
new read-back/helper tools, behavior-preserving efficiency (proptest/byte-guarded),
additive mesh/editor capabilities. Never claim render-verified what isn't.

---

## 2. CRITICAL gotchas (these cost real time this session — read before driving)

**Driving the editor as the CLI agent — you do NOT have the awsm-editor MCP *tools*.**
Only its *resources* come through (use `ListMcpResourcesTool` / `ReadMcpResourceTool`,
server `"awsm-editor"`, e.g. `awsm://docs/mesh-tools`). Drive the editor by POSTing
the `Request` enum to the MCP server's HTTP relay — same `EditorController`, same
WebTransport session as the tools, just the other door (`packages/mcp/src/http.rs`):
```
curl -s -X POST http://127.0.0.1:9086/debug -H 'Content-Type: application/json' -d '<Request JSON>'
```
- `EditorCommand` is **internally tagged**: `{"cmd":"snake_name", ...fields}`. Wrap: `{"Dispatch": <cmd>}` or `{"DispatchBatch":[<cmd>,...]}`.
- `EditorQuery` is `{"query":"snake_name", ...}` wrapped as `{"Query": <query>}`.
- Other requests: `"Mode"`, `{"ScenePng":{"width":N,"height":N}}` (PNG saved to `/tmp/awsm-mcp-last.png`; the relay returns a summary, not bytes), `"Undo"`, `"Redo"`.
- Settle barrier before screenshots: `{"Query":{"query":"wait_render_settled","max_ms":4000}}`.
- A reusable python driver from this session lives at `/tmp/drive.py` (helpers: `cmd`, `batch`, `query`, `png`, `settle`, `newid`) — re-create if gone.

**The dev stack dies.** `task mcp-dev` (trunk editor on :9085 + MCP relay on :9086 +
QUIC :9087 + media) exits intermittently. Symptom: ALL `/debug` requests return empty.
Restart: `cd <repo> && nohup task mcp-dev > /tmp/mcpdev.log 2>&1 &`. The browser tab
(open at `:9085/?mcp=http://127.0.0.1:9086`) auto-reattaches when it's back; watch for
`editor attached` + `✅ success` in `/tmp/mcpdev.log`.

**A runtime panic in editor code kills the render loop for the whole tab.** Symptom:
`frame_globals` (and ScenePng) return empty while `Mode` may still answer. You cannot
read the panic after the fact (wasm aborts). Reason about the cause; fix; trunk
rebuild + page reload recovers. Example that bit us: building a HUD mesh whose
per-vertex streams (positions/normals/**uvs**) had mismatched lengths panicked the
uploader (`raw_mesh.rs` packs UVs as `uvs[v]` for `v in 0..positions.len()`).

**Pinned frame time freezes ALL time-driven materials.** `set_frame_time{seconds}`
overrides the wall clock; nothing animates until `clear_frame_time`. (This session's
"the cutout shadow isn't moving" was exactly this.)

**Trunk auto-reloads on rebuild — wait for it.** After editing renderer/editor code,
trunk recompiles wasm and reloads the page. Don't test against a mid-reload tab. Poll
`{"Query":{"query":"frame_globals"}}` until `frame_count` ADVANCES (alive), and confirm
`dist/*_bg.wasm` mtime is newer than your last edit before trusting a render.

**Never write the banned project codename (or its repo path) into any committed file** —
code, docs, or plans. The exact word lives in your auto-memory (the
`no-…-in-committed-files` note); grep the staged diff for it before every commit.

**Patterns worth mirroring (landed this session):**
- *8-variant shadow caster pool* (`shadows/state.rs` + `helpers.rs::shadow_pipeline_cache_key` + `shadow_masked/pipeline.rs`): instancing × cube_face × **double_sided**. If you add another caster axis, follow that thread (and the 4→8 asserts in `from_resolved`).
- *Adding a uniform to the masked-shadow pass*: bind into group-0 (`shadow_masked/bind_group.rs` const + recreate + layout) AND declare it in `shadow_masked_wgsl/bind_groups.wgsl`, shifting the texture-pool base. The alpha-only window is shared with the geometry pass, so a binding it references must exist in BOTH.
- A spot light shares the 2D atlas (no own attachment) so its shadow re-renders EVERY frame (`should_render = due || !has_own_attachment`) — good for verifying time-animated shadow effects.

---

## 3. Conventions (unchanged from the overnight spec)
- Commit incrementally; **tree compiles at every commit**; `task lint` (fmt + clippy `-D warnings`, whole workspace) + relevant `cargo test` before each.
- **Never claim browser/render-verified what isn't.** Mark visual items "needs browser verify."
- Cargo-verifiable backend work first; flag anything that could change rendered output.
- Log notable additions in the PROGRESS LOG of `mesh-pipeline-overhaul.md`.
- Prefer high-value / low-risk; behind a flag or noted when unsure without the browser.

---

## 4. TWO-PHASE EXECUTION (headless build → browser verify)

The work splits cleanly: what a headless run can build + `cargo`-verify, and what
needs a live browser tab. Run them as two separate `/loop` sessions. Phase A fills
`docs/plans/PHASE-2-QUEUE.md`; Phase B drains it.

### PHASE A — headless build (scheduled; NO browser, NO MCP, NO screenshots)

There is no editor tab attached. Do NOT start `task mcp-dev`, do NOT POST to
`:9086/debug`, do NOT screenshot. Everything is proven by `cargo` / `task lint` only.
For anything whose correctness fundamentally needs the GPU/browser, BUILD it (compile +
clippy + unit/integration tests at the command / renderer-core layer) and enqueue the
exact visual check in `docs/plans/PHASE-2-QUEUE.md` — never claim it verified.

PASTE:
> Headless autonomous build for the `mesh-authoring` roadmap — NO browser, NO MCP, NO screenshots;
> do NOT start `task mcp-dev` or POST to :9086/debug (no editor tab is attached). Read
> docs/plans/OVERNIGHT-HANDOFF.md FIRST (state, scope, gotchas — ignore the browser-driving ones this
> phase). Build the CARGO-VERIFIABLE parts of: Phase 5 skin/morph MCP backend — set_morph_weight +
> get_morph_data + joint-weight/bind-pose editing + richer skeletal/morph animation authoring (new
> EditorCommand/EditorQuery variants + typed MCP tools/schemas + the command→renderer-core wiring),
> with unit/integration tests at the command + renderer-core layer (no GPU); Phase 4 packer/convert
> parity (pure-data logic + proptests); finish the renderer-gltf tangent consolidation; plus any
> Phase-7 cleanup you touch. Also scaffold the editor-UI/visual pieces those need (Phase 6
> bones-in-outliner + skeleton lines, animation-playback wiring, vertex-selection highlight) as far as
> compiles + cargo-tests allow, behind no-op-safe paths. Detail in docs/plans/mesh-pipeline-overhaul.md
> (Phase 5/6 notes + landscape survey). Rules: commit incrementally, tree compiles every commit, run
> `task lint` + relevant `cargo test` before each commit, NEVER claim browser/render-verified anything —
> for every item needing the live editor append a precise check to docs/plans/PHASE-2-QUEUE.md (steps +
> expected result). Keep history bisectable; do NOT push. STOP when the cargo-verifiable scope is
> exhausted or you're blocked needing the browser, then write a crisp report (what landed + cargo-test
> status, what's queued for Phase 2, decisions made). Do not churn on open-ended cleanup past that point.

### PHASE B — browser verify + visual build (manual kickoff; browser tab open)

Start this when you're back, with `task mcp-dev` up and a Chrome WebGPU tab at
`http://localhost:9085/?mcp=http://127.0.0.1:9086`. It loops until the queue is clear.

PASTE:
> Browser-verify + finish the `mesh-authoring` roadmap. Read docs/plans/OVERNIGHT-HANDOFF.md (esp. §2
> gotchas: drive via POST :9086/debug not MCP tools; restart `task mcp-dev` with nohup if /debug goes
> empty; a panic kills the render loop until reload; clear_frame_time or nothing animates; wait for trunk
> rebuild + frame_count to advance before trusting a render). Then work docs/plans/PHASE-2-QUEUE.md top
> to bottom: for each item drive the live editor, screenshot-verify the expected result, and FIX whatever
> Phase A built that doesn't actually render/behave right. After the queue, complete the remaining VISUAL
> work with live verification: Phase 6 bones-in-outliner + skeleton/morph viz, animation playback in the
> editor, vertex-selection highlight, and Phase 4 parity via the load_player_bundle round-trip screenshot
> compare. Rules: commit incrementally, `task lint` + cargo test before each commit, mark a queue item done
> ONLY after you SAW it correct in the tab, append progress to the PROGRESS LOG in mesh-pipeline-overhaul.md,
> and tick items off PHASE-2-QUEUE.md. Loop until the queue is empty and the listed visual work is verified;
> then write a final report.
