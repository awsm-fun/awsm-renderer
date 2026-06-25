# MCP test fixes — implementation plan

Source of findings: `/Users/dakom/Downloads/temp-scene/upstream-fixes.md` (gaps found
while driving the editor over the `awsm-scene` MCP surface to author tank-tread
UV-scroll on `robot-001`). This plan turns those findings into landable, verifiable
work.

## Scope (decided)

**In scope:** P0 + all P1 + the three *cheap* P3 QoL items + Discoverability.
**Out of scope (deliberate — separate follow-up):** the heavy P2 features —
render-to-texture / material bake (`bake_material_to_texture`) and
mesh split / per-submesh materials (`separate_mesh`). They need new GPU
offscreen-render and mesh-topology-split machinery that does not exist yet; too
risky for an unattended loop. Note them in a closing "Deferred" section of the PR.

**Done-gate for every item (both must pass before moving on):**
1. **Static:** `cargo fmt --all -- --check` clean · `cargo clippy --all --all-features --tests -- -D warnings` clean · `cargo test --all-features` green · a new **serde round-trip test** in `packages/mcp/editor-protocol/tests/` for every new `EditorCommand`/`EditorQuery` variant and every changed struct.
2. **Live:** exercise the new behaviour against the running editor and confirm the observable result (see "Live verification" below). Capture the proof (readback value / screenshot / console log) in the per-item log.

---

## Architecture anchors (verified, current)

Wire model: MCP tool → `EditorCommand`/`EditorQuery` (serde-tagged JSON) → WebSocket
`Request`/`Response` → browser editor controller → renderer. PNGs ride a `/png/<id>`
HTTP side-channel.

| Thing | File | Anchor |
|---|---|---|
| `EditorCommand` enum (`#[serde(tag="cmd", rename_all="snake_case")]`) | `packages/mcp/editor-protocol/src/command.rs` | per-vertex authoring block `615-694`; mesh editing `543-613` |
| `EditorQuery` enum (`#[serde(tag="query", …)]`) | `packages/mcp/editor-protocol/src/query.rs` | `GetVertexData` ~`370`; `GetMeshLayers` ~`387`; `VertexPredicate` ~`422` |
| `VertexOverrides { positions, colors, normals, uvs }` (`HashMap<u32, …>`) | `packages/mcp/editor-protocol/src/mesh_def.rs` | `55-74` |
| `CapturedMesh { positions, normals, uvs, uvs1, colors, indices }` | same | `107-119` |
| Command handlers (`apply_inner`) | `packages/frontend/editor/src/controller/state.rs` | `SetMeshData` `1485`; `SetVertexPositions` `1539`; `PaintVertexColors` `1569`; `SetVertexNormals` `1630`; `SetVertexOverrides` `1645` |
| Override write helpers | same | `ensure_authorable` `627`; `apply_vertex_overrides` `674`; `overrides_inverse` `708` |
| Query handler | same | `query()` `4196`; `GetVertexData` `4520`; `GetMeshLayers` `4593` |
| Bake applies overrides (incl. **uvs**) | `packages/frontend/editor/src/controller/mesh_eval.rs` | `apply_overrides` `40`; uv branch `76-84` |
| MCP tool wrappers + param structs (`#[tool(description=…)]`, `schemars::JsonSchema`) | `packages/mcp/src/mcp.rs` | `set_vertex_positions` ~`2190`; `paint_vertex_colors` ~`2255`; `set_vertex_normals` ~`2302`; `get_vertex_data` ~`2341`; `run_query` ~`1516`; `dispatch_command` ~`3647`; `json_arg` ~`4131` |
| Dispatch deserialize (P0 root cause) | `packages/mcp/src/mcp.rs` | `json_arg` uses `serde_json::from_value` ~`4140` |
| Doc resources (`include_str!`, served by `read_resource`) | `packages/mcp/src/mcp.rs` | list ~`3926`; read ~`3980`; const block ~`4276-4284` |
| Recipes cookbook (embedded) | `docs/dynamic-materials/recipes.md` | "Scrolling texture (animated UV)" ~`80` |
| Animation track types | `packages/crates/scene/src/animation.rs` | `TrackTarget` `184`; `TransformProp` `83`; `add_track` build in mcp.rs ~`3485`/`build_track_target` ~`4151` |

**Key invariant to mirror everywhere:** per-vertex authoring is *terminal* — the
first authoring op calls `ensure_authorable` (collapses the procedural stack to a
frozen `Captured`-self base), then writes a sparse `overrides` map, re-bakes, and
returns an inverse via `overrides_inverse` (a `SetVertexOverrides`, batched with a
stack restore if the collapse fired). New authoring commands MUST use
`apply_vertex_overrides` + `overrides_inverse` so undo stays correct.

---

## Items

Work them top-to-bottom; each is an independent commit on the current `mcp-fixes`
branch. Items 1 and 1b unblock the original tread task, so they go first.

### Item 1 — P0: `set_vertex_uvs` (the one true blocker)

The bake already consumes `overrides.uvs` (`mesh_eval.rs:76`); only the write verb
is missing. Mirror `SetVertexPositions` exactly (per-index array, not a single value
— strip authoring needs a distinct UV per vertex).

1. **Command** (`command.rs`, in the per-vertex authoring block after
   `SetVertexNormals` ~`649`):
   ```rust
   /// Set the per-vertex **UV** override of `indices` to `uvs` (TEXCOORD_0).
   /// `indices[k]` gets `uvs[k]`. Mirrors SetVertexPositions; terminal +
   /// collapse-first like the other authoring verbs. Inverse: restore prior
   /// overrides.
   SetVertexUvs {
       mesh: AssetId,
       #[serde(default)]
       indices: Vec<u32>,
       uvs: Vec<[f32; 2]>,
       /// §10: target indices from a stored selection HANDLE instead of `indices`.
       #[serde(default)]
       selection: Option<u32>,
   },
   ```
   (Single UV set 0 only — `VertexOverrides.uvs` is one map. A `uv_set` arg is a
   future extension; do not add it now.)
2. **Handler** (`state.rs`, beside `SetVertexNormals` ~`1630`) — copy the
   `SetVertexPositions` body, writing `ov.uvs.insert(idx, *uv)`:
   ```rust
   EditorCommand::SetVertexUvs { mesh, indices, uvs, selection } => {
       let indices = resolve_vertex_selection_or(selection, indices)?;
       let collapse = self.ensure_authorable(mesh)?;
       let prior = self.apply_vertex_overrides(mesh, |ov| {
           for (k, &idx) in indices.iter().enumerate() {
               if let Some(uv) = uvs.get(k) { ov.uvs.insert(idx, *uv); }
           }
       })?;
       Ok(Some(self.overrides_inverse(mesh, prior, collapse)))
   }
   ```
3. **MCP tool** (`mcp.rs`): add `SetVertexUvsParams { mesh: String, indices: Vec<u32>
   (#[serde(default)]), uvs: Vec<[f32;2]>, selection: Option<u32> }` and a
   `set_vertex_uvs` tool mirroring `set_vertex_positions` (~`2190`). Description must
   state: per-index UVs, terminal/freezes stack, pairs with `get_vertex_data` /
   `get_mesh_data`, and cross-link the conveyor recipe (Item 7).
4. **Activity-feed coalescing key** (`state.rs` ~`7566`): add a `SetVertexUvs` arm
   (e.g. `Some((22, pack(*mesh,0,0)))`) so rapid edits coalesce like the others.
5. **Test:** extend `packages/mcp/editor-protocol/tests/mesh_roundtrip.rs` — a
   `VertexOverrides` with `uvs` populated survives JSON + bitcode round-trip; and a
   `SetVertexUvs` command JSON round-trips.

**Live verify:** on `robot-001`'s `tires` mesh, dispatch `set_vertex_uvs` for a few
indices, then `get_vertex_data` the same indices and confirm `uv` changed to the
written values.

### Item 1b — P0 follow-on: make integer-keyed-map dispatch work globally

Root cause: `json_arg` (`mcp.rs:4131`) deserializes via `serde_json::from_value`,
which cannot parse JSON string object-keys into `u32` (serde_json's `from_str`
**can**, via its MapKey path). This is why `set_vertex_overrides {uvs:{"0":[u,v]}}`
fails. Fix once, globally, so every integer-keyed-map command (`SetVertexOverrides`
and any future one) is drivable over `dispatch_command`.

1. In `json_arg`, route the non-string branch through string form too:
   ```rust
   other => serde_json::from_str(&serde_json::to_string(&other)?)
       .map_err(|e| McpError::invalid_params(format!("bad {what}: {e}"), None))?,
   ```
   (Equivalent: serialize-then-`from_str`. Keep the existing String branch.)
2. **Test:** a unit test in `packages/mcp` (add `src/lib.rs` exposing `json_arg` if
   needed, or a `#[cfg(test)]` mod) that `json_arg::<EditorCommand>` parses
   `{"cmd":"set_vertex_overrides","mesh":"…","overrides":{"uvs":{"0":[0.1,0.2]}}}`
   without the "expected u32" error.

**Live verify:** `dispatch_command` a `set_vertex_overrides` with a `{"0":[…]}` uvs
map and confirm it applies (read back via `get_vertex_data`).

### Item 2 — P1: `get_mesh_data` (read the index/topology buffer)

`get_vertex_data` returns per-vertex attributes but never the index buffer, so loop
ordering / adjacency / arc-length are impossible. Add the read counterpart, paged
over **triangles** (the new payload), and direct callers to `get_vertex_data` for
per-vertex attributes (avoids duplicating large arrays / token blowup).

1. **Query** (`query.rs`, near `GetVertexData`):
   ```rust
   /// Read mesh topology: the triangle index buffer (paged) + counts + bbox.
   /// Per-vertex attributes come from `get_vertex_data`. Read counterpart to
   /// set_mesh_data.
   GetMeshData {
       node: NodeId,
       #[serde(default)] offset: Option<u32>,  // in triangles
       #[serde(default)] limit: Option<u32>,
   },
   ```
2. **Handler** (`state.rs query()` near `4520`): resolve node → `md` via
   `crate::controller::export::node_mesh` (same as `GetVertexData`), then return a
   `MapResult { kind:"mesh_data", … }` with `vertex_count`, `triangle_count`
   (`md.indices.len()/3`), `offset`, `returned`, and a `triangles` window: page
   `md.indices.chunks(3)` by `offset`/`limit`, each as `[a,b,c]`. Include `bbox`
   (min/max over `md.positions`). Skinned-node guard like `GetVertexData`.
3. **MCP tool** (`mcp.rs`): `get_mesh_data` + `GetMeshDataParams { node, offset,
   limit }`, `read_only_hint = true`. Description: "topology read; pair with
   get_vertex_data for attributes."
4. **Test:** round-trip of the new query variant JSON in
   `packages/mcp/editor-protocol/tests/` (mirror existing query tests if present;
   otherwise add a small `query_roundtrip.rs`).

**Live verify:** `get_mesh_data` on `tires` returns `triangle_count == 4576` and a
non-empty `triangles` page; paging (`offset`/`limit`) returns disjoint windows.

### Item 3 — P1: `set_mesh_data` empty/degenerate guard

Today `set_mesh_data {positions:[], indices:[]}` silently wiped the 2520-vert mesh
and returned `ok` (undo saved it). Validate before storing.

1. **Command** (`command.rs:555`): add an explicit opt-out so deliberate clears stay
   possible:
   ```rust
   SetMeshData { mesh: AssetId, data: CapturedMesh, #[serde(default)] allow_empty: bool },
   ```
2. **Handler** (`state.rs:1485`): before `store_with_id`, validate and reject (return
   `EditorError`) unless `allow_empty`:
   - `data.positions` non-empty AND `data.indices` non-empty;
   - `data.indices.len() % 3 == 0` (always enforced);
   - `*data.indices.iter().max() < data.positions.len() as u32` (always enforced,
     when indices non-empty);
   - optional channels, when present, are vertex-aligned
     (`normals/uvs/uvs1/colors` len == positions len).
   Error messages specific (match the repo's existing style, e.g. the
   `RemoveModifier` out-of-range message at `1516`).
3. **MCP tool:** `set_mesh_data` is currently `dispatch_command`-only — no typed
   tool. Leave it as escape-hatch (typing it fully is out of scope), but the new
   `allow_empty` field flows through `dispatch_command` automatically.
4. **Test:** in `mesh_roundtrip.rs` (or a controller test if reachable) assert the
   validation predicate rejects `(positions:[], indices:[])`, rejects
   `indices.len()%3 != 0`, rejects out-of-range index; accepts a valid mesh; accepts
   empty when `allow_empty:true`. (If the predicate is a free fn in `state.rs`,
   extract it to a small testable fn — e.g. `fn validate_captured_mesh(&CapturedMesh)
   -> Result<(),String>` — and unit-test that.)

**Live verify:** `dispatch_command set_mesh_data {data:{positions:[],indices:[]}}`
now returns an error (not `ok`); the mesh is unchanged (`get_mesh_data` still shows
4576 triangles).

### Item 4 — P3: `get_vertex_data` source flag (base vs override)

`get_vertex_data` returns resolved values but not which channels are overrides. Add
an opt-in `source` block per vertex.

1. **Query** (`query.rs` `GetVertexData`): add `#[serde(default)] include_source:
   bool`.
2. **Handler** (`state.rs:4520`): when `include_source`, resolve the mesh def's
   `overrides` (as `GetMeshLayers` does at `4607`), and for each returned vertex emit
   `"source": {"position": is_in(ov.positions), "normal":…, "color":…, "uv":…}` where
   each value is `"override"` or `"base"`.
3. **MCP tool** (`mcp.rs`): add `include_source: bool` to `GetVertexDataParams` and
   pass through.
4. **Test:** query-variant round-trip incl. the new field.

**Live verify:** after Item 1's `set_vertex_uvs` on a few verts, `get_vertex_data
{include_source:true}` shows `uv:"override"` on exactly those verts, `"base"` elsewhere.

### Item 5 — P3: spin / relative-rotation track (keyframe generator)

The renderer's tracks are keyframe-sampled, not procedural; a true "turns/sec"
runtime driver is a big change (out of scope). The *cheap, doc-intended* fix is a
generator that expands one line into a standard rotation `Transform` track with
evenly-spaced quaternion keyframes — collapsing the hand-authored
N-quarter-turn-quats workflow. Implement as an `EditorCommand` (so it's exercisable
headlessly via `editor_dispatch_json`, and undoes as a single track add).

1. **Command** (`command.rs`, near the animation commands):
   ```rust
   /// Generate a rotation Transform track on `node` in `clip` that spins about a
   /// local `axis` by `turns` full revolutions over `duration` seconds, sampled at
   /// `keys_per_turn` keyframes (default 4). Expands to a normal Rotation track
   /// (quaternion keys); plays/reverses via set_clip_speed/direction. Inverse:
   /// remove the created track.
   AddSpinTrack {
       clip: AssetId,
       node: NodeId,
       axis: [f32; 3],
       turns: f32,
       duration: f64,
       #[serde(default)] keys_per_turn: Option<u32>,
   },
   ```
2. **Handler** (`state.rs`): normalize `axis`; generate `n = ceil(|turns| *
   keys_per_turn.unwrap_or(4)) + 1` keyframes; key `k` at `t = duration * k/(n-1)`
   with quaternion `axis_angle(axis, 2π·turns·k/(n-1))` (xyzw). Build a
   `StoredTrack { target: Transform{ node, prop: Rotation }, sampler: Linear|Cubic,
   times, keys: Vec<Keyframe{ Quat }> }` and add it to the clip (reuse the same
   path `AddTrack` + keyframe insertion uses). Inverse: the existing remove-track
   command for the created track id.
   - Reuse axis-angle→quat from the codebase if present (grep `glam::Quat` /
     `from_axis_angle`); otherwise the cheat-sheet in `docs/ANIMATION_AUTHORING.md`
     gives the formula.
3. **MCP tool** (`mcp.rs`): `add_spin_track` + params struct. Description:
   "one-line wheel/rotor spin; expands to a Rotation track."
4. **Test:** command JSON round-trip; plus a unit test of the quat generator (first
   key ≈ identity, a full `turns=1` returns to ≈identity, `keys` length matches).

**Live verify:** `add_spin_track` a 1-turn/2s spin on a wheel node, then
`run_query sample_clip_timeseries` (or `get_track_data`) confirms the rotation
interpolates monotonically around the axis.

### Item 6 — P3: strip / loop parameterization helper

Given a vertex band, emit per-vertex `(along, across)` so the agent feeds them
straight into `set_vertex_uvs` (tread/conveyor/road). Read-only query. **Lowest
confidence of the batch** — a true connectivity-ordered arc-length needs robust loop
extraction; ship a well-defined *heuristic* and document its limits rather than
blocking the loop.

1. **Query** (`query.rs`):
   ```rust
   /// Heuristic strip parameterization of a vertex selection: returns normalized
   /// (along, across) per vertex. `along` = angle about the best-fit/`axis` axle
   /// (monotonic travel for a belt loop), unwrapped to [0,1); `across` = normalized
   /// position along that axis. Feed directly into set_vertex_uvs. Heuristic, not a
   /// true geodesic unwrap.
   StripParameterize {
       node: NodeId,
       #[serde(default)] selection: Option<u32>,
       #[serde(default)] indices: Vec<u32>,
       #[serde(default)] axis: Option<[f32; 3]>,
   },
   ```
2. **Handler** (`state.rs query()`): resolve `md` + target indices (selection handle
   or list); if `axis` absent, fit it (PCA: the axis of least position variance for
   a loop, i.e. the axle). For each vertex: `across` = normalized projection onto
   `axis`; `along` = `atan2` angle of the position in the plane ⟂ `axis` about the
   selection centroid, mapped to `[0,1)`. Return `{ vertices:[{index, along,
   across}], axis }`.
3. **MCP tool** (`mcp.rs`): `strip_parameterize`, `read_only_hint = true`.
4. **Test:** query JSON round-trip; unit-test the math on a synthetic cylinder band
   (angles monotonic around the axis, `across` spans 0..1).

**Live verify:** run on a `tires` belt-face selection; feed the returned coords to
`set_vertex_uvs`; bind a tileable test tile + a `texture_transform` V-scroll and
confirm (screenshot) the pattern reads as travel, not atlas garbage. If the heuristic
can't produce a clean strip on the non-circular belt, ship the PCA version, log the
limitation in `MESH_TOOLS.md`, and DO NOT block — mark Item 6 "shipped (heuristic)".

### Item 7 — P1: Discoverability (docs + point-of-use cross-links)

The clean path must be the discoverable one. All doc files are `include_str!`-embedded
(`mcp.rs:4276-4284`); editing the `.md` + rebuilding the server re-embeds them.

1. **New recipe** in `docs/dynamic-materials/recipes.md`: **"Geometry-locked scroll
   (conveyor / tread / road)."** Cover both now-supported paths: (a) the clean path —
   author a continuous strip UV with `set_vertex_uvs` (+ `strip_parameterize` from
   Item 6), bind a tileable tile, animate with a `texture_transform` offset track /
   `flow`; (b) the vertex-color-as-scroll-coordinate custom-WGSL fallback
   (`paint_vertex_colors` → `material_vertex_color` + `frame_globals.time`). State the
   **tileable-strip-UV prerequisite** plainly.
2. **Fix the existing "Scrolling texture (animated UV)" recipe** (~`80`): label it
   clearly as screen-space / normal-derived (good for glowing panels, **not** a
   surface parameterization), and cross-link to the new conveyor recipe.
3. **Point-of-use tool-description cross-links** (`#[tool(description=…)]` strings in
   `mcp.rs`): add a one-line "for scrolling on real geometry see the conveyor recipe
   in `awsm://docs/material-recipes`" to `set_node_texture_transform`,
   `set_vertex_uvs` (Item 1), and `set_material_uniform`. Add the inverse: a
   "See also" pointer in the recipe back to those tools.
4. **Update `docs/MESH_TOOLS.md`:** document `set_vertex_uvs`, `get_mesh_data`,
   `get_vertex_data {include_source}`, `strip_parameterize`, and the
   `set_mesh_data` empty-guard / `allow_empty`.
5. **Update `docs/ANIMATION_AUTHORING.md`:** document `add_spin_track` (and note it
   replaces hand-authoring quarter-turn quats; keep the cheat-sheet as the
   under-the-hood reference).
6. **Consistency note** (short, in `docs/MCP.md` or `AGENT_GUIDE.md`): the typed-tool
   coverage rule — every vertex attribute now has a typed verb (positions/normals/
   colors/**uvs**). Escape hatches (`dispatch_command`/`run_query`) serve the long
   tail, not core features.

**Live verify (docs):** after a server rebuild, `read_resource awsm://docs/material-recipes`
returns the new conveyor recipe text, and `tools/list` shows the cross-links in the
three tool descriptions.

---

## Build / run mechanics (for the loop)

- **Workspace:** crates touched — `awsm-renderer-editor-protocol`
  (`packages/mcp/editor-protocol`), `awsm-renderer-mcp` (`packages/mcp`),
  `awsm-renderer-editor` (`packages/frontend/editor`), `awsm-renderer-scene`
  (`packages/crates/scene`).
- **Static gate:**
  `cargo fmt --all` · `cargo clippy --all --all-features --tests -- -D warnings` ·
  `cargo test --all-features`. Fast inner loop:
  `cargo test -p awsm-renderer-editor-protocol` and
  `cargo check -p awsm-renderer-scene-mcp`.
- **Live harness:** `task mcp-dev` → editor `:9085` (trunk, auto-rebuilds editor on
  save) + mcp server `:9086` (native, does **not** auto-rebuild). Own it as a
  background task; log `/tmp/mcp-dev.log`.
  - A change to `packages/mcp` **or** `editor-protocol` needs a **server restart**:
    TaskStop → free ports (`lsof -ti tcp:9085,9086,9082,9083 | xargs kill`) →
    relaunch → poll `:9085/` and `:9086/health` == 200 + grep log
    `"server listening"`.
  - Editor-only changes: trunk auto-rebuilds; just reload the page.

### Live verification — the robust path (READ THIS)

⚠️ **The harness MCP tool-list is CACHED across a server restart.** A newly-added
**typed MCP tool** (e.g. `set_vertex_uvs`) is in the rebuilt binary (confirm via
`curl` initialize→tools/list on `:9086/mcp`) but will **not** surface to the loop's
tool set after restart. Do **not** rely on the new typed tool for verification.

**Use the headless editor exports instead** (no MCP pairing, no tool registration):
via chrome-devtools `evaluate_script` on the `:9085` page, call
- `window.wasmBindings.editor_dispatch_json('{"cmd":"set_vertex_uvs", …}')` — hits
  the same `apply_inner` handler as the MCP tool;
- `window.wasmBindings.editor_query_json('{"query":"get_mesh_data", …}')` — same
  query handler.

⚠️ **`editor_dispatch_json` is FIRE-AND-FORGET.** It returns `"ok"` as soon as the
command JSON *decodes* and runs the apply in a detached `spawn_local`; an apply
**error** is only logged to the browser console (`tracing::error! "dispatch failed: …"`),
never returned. So `"ok"` from it ≠ apply success. To verify an **error/rejection
path** (e.g. the set_mesh_data guard), use the MCP `dispatch_command` tool instead —
it awaits the WS apply result and surfaces the error as an MCP error. For
**success-with-readback** verification, `editor_dispatch_json` then
`editor_query_json` is fine (the query awaits and shows the mutated state).

⚠️ **Both exports are `async` and return a JSON *string*.** You MUST `await` them
inside an `async () => { … }` evaluate_script function — an un-awaited call
serializes as `{}` (the Promise), which looks like an empty result but isn't. Then
`JSON.parse` the returned string. Example:
`async () => JSON.parse(await window.wasmBindings.editor_query_json('{"query":"frame_globals"}'))`.

This exercises the *real* new `EditorCommand`/`EditorQuery` handlers regardless of
the cached tool-list. (Brand-new query *variants* may also be reachable via
`run_query`'s passthrough; `editor_query_json` is the sure path.)

- **Screenshots:** `screenshot_scene`/`screenshot_texture` if the MCP client is live,
  else chrome-devtools `take_screenshot` after `wait_render_settled`.
- **Renderer logs** (`tracing::info!/warn!`) surface in the **browser console** —
  chrome-devtools `list_console_messages`, saved to a file, `grep` it (don't load
  into context). The editor's `get_console_logs` buffer is unreliable.
- If the harness registered **zero** `awsm-scene` tools at session start, drive MCP
  over HTTP with `/tmp/mcp.py` (persistent sid in `/tmp/mcp-sid`, don't re-init).
  But for command/query verification, `editor_dispatch_json`/`editor_query_json` via
  chrome-devtools is simplest and pairing-free.

---

## Definition of done (whole plan)

- Items 1, 1b, 2, 3, 4, 5, 6, 7 each landed as a commit on `mcp-fixes`, each with:
  static gate green + a live-verify proof recorded.
- Full `cargo fmt --all -- --check` / `cargo clippy --all --all-features --tests -- -D warnings`
  / `cargo test --all-features` green on the final tree.
- New typed tools present in `:9086/mcp` `tools/list` (curl check) and documented.
- Docs (recipes, MESH_TOOLS, ANIMATION_AUTHORING, MCP/AGENT_GUIDE consistency note)
  updated and re-embedded (server rebuild reflects them).
- A PR/summary listing what shipped and an explicit **Deferred** section: P2
  `bake_material_to_texture` and `separate_mesh` / per-submesh materials, with a
  one-paragraph rationale (need new offscreen-render + mesh-split machinery).
- **End-to-end proof:** the original task is unblocked — on `robot-001`'s `tires`,
  author a strip UV via `set_vertex_uvs` (+ `strip_parameterize`), bind a tileable
  tile, un-mute the existing `texture_transform` V-scroll tracks on
  `roll-forward`/`roll-backward`, and capture a screenshot showing the tread reads as
  travel (not atlas garbage). Record it in the final log.

## Progress log

Maintain a checklist here as items land (status + the live-verify proof per item).
Append, don't rewrite.

- [x] Item 1 — set_vertex_uvs — STATIC: clippy/fmt clean, full `cargo test --all-features` green (46 binaries), +2 roundtrip tests (`vertex_overrides_uvs_roundtrip`, `set_vertex_uvs_command_json_roundtrip`). LIVE: on a box mesh, headless `editor_dispatch_json {cmd:set_vertex_uvs, indices:[0,1,2], uvs:[[.11,.22],[.33,.44],[.55,.66]]}` → "ok"; `get_vertex_data` confirmed uv changed `[[0,0],[0,1],[1,1]]` → `[[.11,.22],[.33,.44],[.55,.66]]`. New typed tool also re-registered after full server restart.
- [x] Item 1b — integer-keyed-map dispatch fix — ROOT CAUSE corrected: `from_str` does NOT fix it (proven by test) — the `#[serde(tag="cmd")]` enum buffers into serde `Content`, which rejects string→u32 keys regardless of from_str/from_value; the WS transport re-deserializes on the editor side too (2nd chokepoint). Real fix: a field-level `deserialize_with` on all four `VertexOverrides` maps that branches on `is_human_readable()` — `deserialize_any` (string-or-int keys) for JSON/Content, native `u32` for bitcode (which rejects `deserialize_any`). STATIC: clippy/fmt clean, full test green, +2 mcp unit tests (`json_arg_parses_integer_keyed_map_command`, `json_arg_parses_string_wrapped_command`), existing bitcode roundtrip still green. LIVE: real MCP `dispatch_command {cmd:set_vertex_overrides, overrides:{uvs:{"0":[.77,.88],"2":[.12,.34]}}}` → "ok" (previously errored "expected u32"); `get_vertex_data` confirmed v0=[.77,.88], v2=[.12,.34], v1 untouched.
- [x] Item 2 — get_mesh_data — STATIC: clippy/fmt clean, full test green (47 binaries), +2 query roundtrip tests. LIVE: headless `get_mesh_data` on a box → vertex_count 24, triangle_count 12, bbox [-.5,-.5,-.5]→[.5,.5,.5]; paging offset0/limit2 → [[0,1,2],[0,2,3]], offset2/limit2 → [[4,5,6],[4,6,7]] (disjoint). Committed with Item 4 (shared read-path files, interleaved hunks — both independently verified).
- [x] Item 3 — set_mesh_data empty guard — STATIC: clippy/fmt clean, full test green, +2 protocol tests (`captured_mesh_validate_rejects_empty_and_degenerate`, `set_mesh_data_command_allow_empty_defaults_false`). Validation as `CapturedMesh::validate(allow_empty)` (testable in protocol crate); `allow_empty` field (#[serde(default)] = on-by-default guard); internal undo-restore sites pass allow_empty:true. LIVE (via MCP dispatch_command, which awaits apply — editor_dispatch_json is fire-and-forget): empty `{positions:[],indices:[]}` → REJECTED "refusing to store empty/degenerate geometry"; mesh unchanged (get_mesh_data still 12 tris/24 verts); non-triangle indices `[0,1]` → REJECTED "not a multiple of 3"; `allow_empty:true` empty → "ok".
- [x] Item 4 — get_vertex_data source flag — STATIC: clippy/fmt clean, full test green, +1 query roundtrip test (`get_vertex_data_include_source_roundtrip`). LIVE: wrote a UV override on box vertex 0, then `get_vertex_data {include_source:true}` → v0 `source.uv:"override"` (position/normal/color "base"), v1 all "base". Committed with Item 2.
- [ ] Item 5 — add_spin_track
- [ ] Item 6 — strip_parameterize (heuristic ok)
- [ ] Item 7 — discoverability docs + cross-links
- [ ] Final — full gate green + end-to-end tread proof + Deferred section written
