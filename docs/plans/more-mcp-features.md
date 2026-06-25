# More MCP features — remaining gaps after the mcp-test-fixes pass

Follow-up to the completed `mcp-test-fixes` work (set_vertex_uvs, integer-keyed-map
dispatch fix, get_mesh_data, set_mesh_data guard, get_vertex_data source flag,
add_spin_track, strip_parameterize, discoverability docs, material_uv include fix —
all shipped on branch `mcp-fixes`, commits `21ef0d5f`..`5cd2cde5`). This plan covers
the genuinely-missing capabilities a follow-up audit confirmed, plus one P3 QoL.

## Verified ALREADY PRESENT — do NOT re-implement

A code audit (Jun 2026) corrected two assumptions; these are **not** gaps:

- **Per-primitive (per-submesh) materials on import already work.** A multi-material
  glTF mesh is destructured on import into a `Group` with one `Mesh` child per
  primitive, each keeping its own material — `controller/state.rs` `build_editor_subtree`
  (~`7536-7603`, the `distinct.len() > 1` branch), per-primitive geometry from
  `engine/bridge/gltf.rs` (~`230-241`), per-primitive material indices from
  `engine/bridge/asset_template.rs:66`. The one-material-per-node model
  (`scene/src/tree.rs` `NodeKind::Mesh.material: Option<MaterialInstance>`) is by
  design; multi-material = multiple nodes. **No import-fidelity loss.** (What's still
  missing is *post-import* re-regioning of a single mesh — that's Item 2 below.)
- **Full per-vertex authoring + reads shipped:** positions/colors/normals/**uvs**
  verbs, `get_mesh_data`, `get_vertex_data{include_source}`, `strip_parameterize`,
  `set_mesh_data` guard — all done in the prior pass.

## Scope (this plan)

| # | Item | Sev | Risk | Notes |
|---|---|---|---|---|
| 1 | Connectivity / island selection predicates | P2 | low | foundation for Item 2; useful standalone |
| 2 | `separate_mesh` — extract a selection into a new sibling node | P2 | med | the real region-isolation gap |
| 3 | UV-layout overlay (diagnostic) | P3 | low | "atlas vs strip" in one glance |
| 4 | `bake_material_to_texture` — UV-space render-to-texture | P2 | **high** | new offscreen GPU pass; ~60-70% infra exists |

Order matters: 1 → 2 (2 builds on 1), then 3, then 4 (largest/riskiest, last).

**Done-gate for every item (both must pass before moving on), same as the prior pass:**
1. **Static:** `cargo fmt --all -- --check` clean · `cargo clippy --all --all-features --tests -- -D warnings` clean · `cargo test --all-features` green · a new unit/round-trip test for every new command/query variant and every pure helper (put pure geometry math in `meshgen` / pure generators in `scene` — both are host-tested).
2. **Live:** exercise the new behaviour against the running editor and record the proof (readback value / screenshot). See "Build / run mechanics" at the bottom.

---

## Item 1 — P2: connectivity / island selection predicates

`select_vertices_where` predicates are geometry-only (`normal_dir`, `axis_*`,
`top_*`, `within_radius`, `within_aabb`) — you cannot select "this connected piece"
or "this UV island". Add connectivity-aware predicates. This is the foundation
Item 2 (separate) needs and is useful on its own (select a belt, a bolt, a panel).

Anchors:
- `VertexPredicate` enum — `packages/mcp/editor-protocol/src/query.rs` (~`422-449`).
- Pure selectors — `packages/crates/meshgen/src/edit.rs` (`select_by_*`, tests behind
  the `authoring` feature; run with `--all-features`).
- Dispatch — `select_vertices_by_predicate` in `controller/state.rs` (~`6426`).

Steps:
1. **Connectivity core (new, pure, in `meshgen/edit.rs`):** build vertex
   adjacency from the index buffer (edge = shared triangle edge), then
   connected-components (union-find or BFS). Expose:
   - `pub fn connected_component_of(mesh, seed_indices) -> Vec<u32>` — the
     component(s) containing the seeds.
   - `pub fn connected_components(mesh) -> Vec<Vec<u32>>` — all islands
     (for "select the Nth island" / diagnostics).
   Treat a "vertex island" by shared-triangle connectivity (position-welded if the
   mesh has split vertices — optionally weld by position within an epsilon; document
   the choice).
2. **New `VertexPredicate` variants:** `ConnectedToSeed { seed: Vec<u32> }` (or a
   point + nearest-vertex seed) and optionally `UvIsland { seed }` (connectivity in
   UV space — components that don't share a UV seam). Start with positional
   connectivity; add UV-island if cheap.
3. **Wire** `select_vertices_by_predicate` to call the new selectors; the existing
   `store`/handle plumbing and the fused `*_where` ops get it for free.
4. **MCP:** these flow through `select_vertices_where`'s `predicate` arg — extend its
   tool description with the new predicate shapes. (No new tool needed.)
5. **Tests:** in `meshgen/edit.rs` — a two-box scene (two disjoint components) selects
   exactly one component from a seed; a single connected mesh returns one component.

**Live verify:** on a scene with two disjoint primitives merged into one editable mesh
(or a torus + separate handle), `select_vertices_where {predicate: connected_to_seed}`
returns only the seeded island's indices (count matches that piece).

## Item 2 — P2: `separate_mesh` — extract a selection into a new sibling node

The real region-isolation gap: detach a vertex/face selection into its own `Mesh`
node so it can carry a different material / be edited independently. Absent today in
both layers (audit-confirmed: no extract op, `Duplicate` clones whole nodes).

Anchors:
- Command enum + handlers — `command.rs` (mesh-editing block ~`543-694`),
  `controller/state.rs` apply handlers.
- Node/asset minting — follow the `insert_primitive` path (how a fresh mesh asset id
  + `NodeKind::Mesh` node are created) and `mutate::duplicate_by_id` (sibling
  insertion) in `engine/scene/mutate.rs`.
- Geometry storage — `CapturedMesh` + `SetMeshData` (already validated).

Steps:
1. **Pure extract (in `meshgen/edit.rs`):**
   `pub fn extract_faces(mesh, selected_verts) -> (extracted: MeshData, remainder: MeshData)`
   — a face is "in" when all (or a configurable majority of) its vertices are
   selected; build a compacted index/vertex buffer for each side with a vertex
   remap; carry normals/uvs/colors through the remap. Return both halves (extract +
   what's left).
2. **`EditorCommand::SeparateMesh { node, selection|indices, keep_remainder: bool }`**
   (+ handler): resolve the node's mesh, run `extract_faces`, mint a NEW mesh asset id
   + store the extracted `CapturedMesh`, create a new sibling `Mesh` node (inherit the
   source transform; default material = source's) holding it, and — if
   `keep_remainder` — replace the source mesh with the remainder via the existing
   `SetMeshData`. Inverse: delete the new node + restore the source geometry (a
   `Batch`). Terminal/collapse semantics like the other authoring ops (freeze first).
3. **MCP tool** `separate_mesh { node, selection?|indices?, keep_remainder? }`.
4. **Tests:** `extract_faces` on a two-box merged mesh splits into the two boxes
   (vertex/triangle counts add up; no dangling indices; attributes preserved).
   Command JSON round-trip.

**Live verify:** merge/author a mesh with two regions, `select_vertices_where {store}`
one region → `separate_mesh {selection}` → a new sibling node appears with that
region's geometry (get_mesh_data triangle_count matches), the source keeps the rest;
assign a different material to the new node and screenshot the two-tone result.

## Item 3 — P3: UV-layout overlay (diagnostic)

"Render a mesh's UV islands over its texture" — would have diagnosed "atlas, not a
strip" in one glance (the original tread blocker). Lower-stakes diagnostic.

Approach (pick the cheaper that verifies cleanly):
- **(a) Query form:** `EditorQuery::UvLayout { node, uv_set }` → the mesh's UV edges
  as polylines (`[[u,v],[u,v]]` segments) + bounds + island count (reuse Item 1's
  connectivity, in UV space). Compact, no rendering. The agent/UI can draw it.
- **(b) Image form:** render the UV wireframe into a PNG (white edges on the bound
  texture, or on transparent) via the existing texture-screenshot/readback path.
  Heavier; only if (a) is insufficient.

Prefer (a) — it's a pure read over `get_mesh_data` + the UV channel, deterministic and
token-bounded (page it). Add an MCP tool `get_uv_layout`. Test: a known cube/quad's UV
edges round-trip; island count matches.

**Live verify:** `get_uv_layout` on a strip-UV'd mesh (from the prior pass's
`set_vertex_uvs`) shows one contiguous island spanning [0,1]; on an atlas-UV'd import
shows many small islands.

## Item 4 — P2 (largest, last): `bake_material_to_texture`

Render a node's shaded/material result into a NEW texture asset under its UV layout
(re-atlas / make-tileable / flatten-to-texture). The audit found ~60-70% of the infra
present — reuse aggressively; the genuinely-new part is one offscreen UV-space pass.

Reuse (anchors):
- GPU readback (copy texture→MAP_READ→bytes): `renderer-core/src/texture/exporter.rs`
  (~`165-200`); PNG/RGBA encode there too.
- Offscreen 2nd renderer + material registration: `editor/src/engine/preview.rs`
  (~`81-161`).
- Texture-asset creation from bytes: `create_texture` / `EditorCommand::CreateTexture`.
- Settle barrier before capture: `wait_render_settled` (`query.rs` ~`440`).
- Screenshot request/poll plumbing: `editor/src/engine/query.rs` (`capture_scene_rgba`,
  `poll_scene_capture`).

Genuinely new:
1. An **offscreen render target** (RGBA8, or Rgba16float for normal/AO bakes) at the
   requested `width×height` — NOT the swapchain.
2. A **UV-space rasterization pass**: the vertex stage writes
   `clip = vec4(uv*2-1, 0, 1)` (UVs become NDC) so the mesh rasterizes into texture
   space; the fragment runs the node's material shading (custom or built-in). Pin
   temporal inputs (`set_frame_time`) + skip camera/shadows.
3. **Async dispatch + readback** → encode → mint a new texture asset; return its id.
4. **`bake_material_to_texture { node, material?, width, height, format?, uv_set?, lighting? }`**
   EditorCommand + MCP tool.

**Risk / checkpoint:** this is a real new GPU render pass — the highest-risk item for an
unattended run. If the offscreen UV pass can't be landed cleanly in one pass, ship the
reusable scaffolding (offscreen target + readback wired to a trivial flat-color bake)
with a passing test + a documented TODO for the material-shading hookup, commit that,
and FLAG it for human review rather than blocking the loop. Do NOT leave the tree
red — partial-but-green + a clear note is the acceptable floor here.

**Live verify (full):** bake a strip-UV'd belt's material to a 256² texture, bind it
back via `set_node_texture`, screenshot — the baked tile reproduces the look. **Partial:**
the flat-color bake produces a correct solid texture asset (screenshot_texture) +
green tests.

---

## Build / run mechanics (carried over — the prior harness)

- **Crates:** `awsm-renderer-editor-protocol`, `awsm-renderer-scene-mcp`,
  `awsm-renderer-editor`, `awsm-renderer-meshgen` (selectors/extract — `authoring`
  feature; `--all-features` covers it), `awsm-renderer-scene`, and for Item 4
  `awsm-renderer-renderer` / `-renderer-core`.
- **Static gate:** `cargo fmt --all` · `cargo clippy --all --all-features --tests -- -D warnings`
  · `cargo test --all-features`. Fast inner loops: `cargo test -p awsm-renderer-meshgen --features authoring`,
  `cargo test -p awsm-renderer-editor-protocol`, `cargo check -p awsm-renderer-scene-mcp`.
- **Live harness:** `task mcp-dev` → editor `:9085` (trunk, auto-rebuilds the editor
  crate on save) + MCP server `:9086` (native; does NOT auto-rebuild). Own it as a
  background task, log `/tmp/mcp-dev.log`.
  - A change to `packages/mcp` **or** `editor-protocol` (or any non-editor crate like
    `meshgen`/`scene`/`renderer`) needs a **full restart** — trunk does NOT watch
    those: free ports (`lsof -ti tcp:9085,9086,9082,9083 | xargs kill -9`) → relaunch
    `task mcp-dev` (background) → poll `:9086/health` → `pairing_status` → navigate the
    browser tab to `http://localhost:9085/?mcp=http://127.0.0.1:9086&pair=<rotated code>`.
  - The pair code rotates on every restart — re-pair each time.
- **Headless live verification (robust, pairing-aside):** via chrome-devtools
  `evaluate_script` on the `:9085` page:
  - `window.wasmBindings.editor_query_json('{"query":…}')` and
    `editor_dispatch_json('{"cmd":…}')` hit the SAME handlers as the MCP tools.
  - ⚠️ **Both are `async` and return a JSON string** — `await` inside an
    `async () => {…}` fn, then `JSON.parse`. An un-awaited call serialises as `{}`.
  - ⚠️ **`editor_dispatch_json` is FIRE-AND-FORGET** — returns `"ok"` on JSON-decode;
    apply errors only hit the browser console. To verify an **error/rejection** path,
    use the MCP `dispatch_command` tool (it awaits the apply). For **success**, read
    back with `editor_query_json` / a screenshot.
  - ⚠️ Newly-added MCP tools/queries may NOT surface in the cached harness tool-list —
    drive new queries via headless `editor_query_json`, new commands via
    `editor_dispatch_json` / `dispatch_command`.
- **Screenshots:** MCP `screenshot_scene` / `screenshot_texture` (after
  `wait_render_settled`), or chrome-devtools `take_screenshot`. For primitives the mesh
  asset id == node id. Renderer `tracing::info!/warn!` surface in the **browser console**
  (chrome-devtools `list_console_messages`, grep the saved file).

## Definition of done (whole plan)

- Items 1–4 each landed as a commit on `mcp-fixes` (or a fresh branch off it), each
  with the static gate green + a live-verify proof recorded in the Progress log below.
- Full `fmt --check` / `clippy -D warnings` / `cargo test --all-features` green on the
  final tree.
- Item 4 either fully working or shipped as green partial scaffolding with a clear
  human-review TODO (never a red tree).
- A short final summary of what shipped, and any newly-discovered follow-ups.

## Progress log

Append per item as it lands (status + the live-verify proof). Don't rewrite.

- [x] Item 1 — connectivity / island selection predicates — STATIC: clippy/fmt clean, full test green (48 binaries), +2 meshgen connectivity tests (`connected_components_finds_two_disjoint_boxes`, `connected_component_welds_split_seam_vertices`) + 1 predicate roundtrip test. `meshgen::edit::connected_component_of` / `connected_components` (position-welded union-find over triangle edges, so UV/normal seams don't fragment a piece); new `VertexPredicate::ConnectedToSeed { seed }` wired through `select_vertices_by_predicate`; select_vertices_where tool description extended. LIVE: box + Array(count:2) modifier → 48 verts (two 24-vert islands); `select_vertices_where {connected_to_seed, seed:[0]}` → 24 (one island), `seed:[47]` → 24 (the other), disjoint.
- [x] Item 2 — separate_mesh — STATIC: clippy/fmt clean, full test green (48 binaries), +2 meshgen tests (`extract_faces_splits_two_boxes`, `extract_faces_partial_face_stays_in_remainder`) + 1 command roundtrip test. `meshgen::edit::extract_faces(mesh, selected) -> (extracted, remainder)` (face moves when all 3 verts selected; compacted+remapped sub-meshes carrying normals/uvs/colors); `EditorCommand::SeparateMesh { node, indices|selection, new_node?, keep_remainder }` mints a fresh Captured mesh asset + sibling Mesh node (inherits source transform+material), and when keep_remainder replaces the source with the remainder (clears stale overrides); inverse = Batch(delete new node+asset, restore source recipe/bytes/overrides). MCP separate_mesh tool. LIVE: box + Array(count:2) (48v/2 islands) → select connected_to_seed island (handle) → separate_mesh {keep_remainder:true} → two sibling nodes: source "Box" now 24v/12t (remainder), new "Separated" node 24v/12t (the extracted island). Clean 24+24 split.
- [ ] Item 3 — UV-layout overlay
- [ ] Item 4 — bake_material_to_texture (full, or green partial + review flag)
- [ ] Final — full gate green + summary
