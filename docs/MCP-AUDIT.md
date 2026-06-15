# MCP tool/query surface audit

Audit of the awsm-renderer editor's MCP surface for AI-agent drivability.

- Tools: `packages/mcp/src/mcp.rs` (~111 `#[tool]` wrappers).
- Underlying protocol: `packages/crates/editor-protocol/src/{command.rs,query.rs}`
  (`EditorCommand` = mutation, `EditorQuery` = read).
- Handlers: `packages/frontend/editor/src/controller/state.rs` (`dispatch` / `query`).
- Bundle bake: `packages/frontend/editor/src/controller/export.rs`,
  `packages/crates/editor-protocol/src/bake.rs`, `packages/crates/glb-export/`.
- User-facing doc: `docs/MCP.md`.

Every `EditorCommand` / `EditorQuery` variant is reachable through the generic
escape hatches `dispatch_command` / `dispatch_batch` / `run_query`, so "gap"
below means *no first-class typed tool* and/or *no underlying variant at all* —
each gap notes which.

---

## 1. Inventory (by area)

### Discovery / introspection (read)
| Tool | Query | Notes |
|---|---|---|
| `get_snapshot` | `Snapshot` | scene tree (`NodeQuery`: id/name/kind/visible/locked/children), selection, mode, undo/redo depth, animation library, custom materials (`compile_ok`+`errors`), textures, coordinate-system metadata. The id-discovery entrypoint. |
| `ping` | `Request::Mode` | health check. |
| `get_mode` | `Request::Mode` | current workspace. |
| `get_console_logs {limit?}` | `ConsoleLogs` | toast ring buffer. |
| `get_node_transforms {nodes?}` | `NodeTransforms` | local TRS + world matrix; live scene. |
| `get_node_details {nodes?}` | `NodeKindDetails` | full serialized `NodeKind` per node (incl. material assignment). |
| `get_node_bounds {nodes?}` | `NodeBounds` | world AABB, **CPU-estimated from primitive dims + world transform** (NOT deformed/animated). |
| `get_mesh_stats {node}` | `MeshStats` | vert/tri counts, bbox, centroid, area, volume, watertight. |
| `get_mesh_cross_section {node,axis?,samples?}` | `MeshCrossSection` | silhouette radius profile. |
| `get_mesh_layers {node}` | `GetMeshLayers` | base kind + modifier list + override/frozen flags. |
| `get_mesh_modifiers {mesh}` | `MeshModifiers` | the `{base,modifiers}` recipe JSON. |
| `get_vertex_data {node,indices}` | `GetVertexData` | final post-eval per-vertex pos/normal/color/uv. |
| `select_vertices_where {node,predicate}` | `SelectVerticesWhere` | predicates: `normal_dir`, `axis_greater`, `axis_less`, `top_percent`, `within_radius`. |
| `get_frame_globals` | `FrameGlobals` | time/delta/frame_count/resolution. |
| (escape hatch) | `CanvasPixels` | exact RGBA at coords (no typed tool). |
| `canvas_stats {region?}` | `CanvasStats` | mean/min/max luma. |
| `get_track_data {clip,track}` | `GetTrackData` | full stored track. |
| (escape hatch) | `SampleClipTimeseries` | clip targets sampled at pinned times (no typed tool). |

### Scene graph / nodes
`insert_primitive`, `insert_empty`, `insert_camera`, `insert_light` (return new id);
`delete_node`, `duplicate_node`, `reparent_node`, `rename_node`,
`set_node_visible`, `set_node_locked`, `set_prefab`, `set_selection`,
`set_vertex_selection`. (`EditorCommand::SetKind` is escape-hatch-only.)

### Transforms
`node_set_transform`, `set_translation`, `translate_by`, `set_scale`,
`set_rotation_euler`. (Convenience tools read current TRS then re-dispatch
`SetTransform`.)

### Mesh editing
`set_mesh_modifiers`, `add_modifier`, `set_modifier`, `remove_modifier`,
`set_vertex_positions`, `soft_transform_vertices`, `collapse_mesh_stack`,
`paint_vertex_colors`, `set_vertex_normals`, `bake_all`, `drop_skinning`,
`convert_to_editable_mesh` (retired/no-op echo). (`SetMeshData`,
`SetVertexOverrides` escape-hatch-only.)

### Materials
`add_custom_material`, `add_builtin_material`, `delete_custom_material`,
`delete_asset`, `register_material`, `assign_material`, `copy_material_instance`,
`set_material_wgsl`, `set_material_alpha_mode`, `set_material_double_sided`,
`set_material_debug_color`, `set_material_layout`, `set_material_includes`,
`set_material_fragment_inputs`, `set_material_uniform`, `set_material_texture`,
`set_builtin_param`, `set_node_texture` (built-in slot), `get_material_wgsl`,
`get_material_contract`, `get_material_diagnostics`.

### Animation
Lifecycle: `add_clip`, `delete_clip`, `duplicate_clip`, `rename_clip`,
`set_clip_duration`, `set_clip_speed`, `set_clip_loop`, `set_current_clip`.
Tracks/keys: `add_track`, `add_keyframe`, `set_keyframe`, `delete_keyframe`,
`get_track_data`. Transport: `set_playhead`, `set_playing`, `set_frame_time`,
`clear_frame_time`. **Mixer/NLA (layers + strips), `delete_track`,
`set_track_mute/solo/sampler`, `set_clip_direction/color`, `step_playhead`,
`set_anim_fps` have NO typed tool** — escape-hatch-only.

### Camera / view
`switch_mode`, `snap_camera_to_axis`, `reset_camera`, `set_camera_orbit`,
`set_camera_projection`, `frame_node`.

### Textures
`add_texture_asset` (procedural), `import_texture_from_url`,
`screenshot_texture`. (No `screenshot_texture`-by-region, no raw pixel readback
of a texture.)

### Environment
`set_environment {skybox?,ibl_prefiltered?,ibl_irradiance?}`.

### Export
`export_scene_glb`, `export_node_glb`, `export_player_bundle {name}`.

### Render verification
`wait_render_settled`, `screenshot_scene`, `screenshot_material`,
`screenshot_texture`.

### Project / history / escape hatches
`new_project`, `load_project_from_url`, `import_model_from_url`, `undo`, `redo`,
`dispatch_command`, `dispatch_batch`, `run_query`.

---

## 2. Gap analysis (prioritized)

Ranked by how often a real agent workflow hits the wall.

### P0 — `resolve_node_material` (which material a node actually renders)
**Gap.** An agent can *set* a node's material (`assign_material`) and *read* a
material's wgsl/diagnostics by asset id, but cannot ask "what material id (and
resolved shading/uniform values) does THIS node currently render with?" without
parsing the raw serialized `NodeKind` blob from `get_node_details` (the
`material` field is an opaque `Option<MaterialInstance>`, undocumented shape).
After `import_model_from_url` the agent has node ids but no clean map to the
imported material assets.
- **Name/sig:** `resolve_node_material { node } -> { node, material_id?, shading: "pbr"|"unlit"|"custom"|"unassigned", builtin_params?: {base_color,metallic,roughness,emissive}, texture_slots?: {slot: texture_id}, uniform_overrides?: {name: value} }`.
- **Backing:** new `EditorQuery::ResolveNodeMaterial { node }`; the controller
  already resolves `MaterialInstance` → variant in `state.rs` (see the inline
  per-mesh store helpers around `state.rs:4099`/`4410`).
- **Why P0:** materials are the most common authoring target and the current
  read path is "parse this internally-tagged enum yourself."

### P0 — world-space AABB predicate in `select_vertices_where`
**Gap.** `VertexPredicate` (query.rs:306) has exactly one spatial predicate,
`within_radius` (a sphere). There is no box predicate, so "select the verts in
this world-space region" (the natural pairing with `get_node_bounds`) is
impossible — an agent must over-select a sphere and filter client-side without
the coordinates (the query returns indices only).
- **Name/sig:** add `VertexPredicate::WithinAabb { min: [f32;3], max: [f32;3] }`
  (and consider `Indices { indices: Vec<u32> }` passthrough for composition).
- **Backing:** add the arm in `state.rs:3212`'s `SelectVerticesWhere` match +
  a `select_within_aabb` in `awsm_meshgen::edit`.
- **Why P0:** region selection is the bread-and-butter of headless sculpting;
  the existing predicate set can't express an axis-aligned box.

### P1 — `select_vertices_where` should return positions, not just indices
**Gap.** The result is `{count, indices}` (state.rs:3246). To reason about *where*
the matched verts are, the agent must follow with `get_vertex_data {node,indices}`
— a second round-trip, and only after it already chose the predicate blind.
- **Fix:** include `positions` (and optionally `normals`) in the
  `vertex_selection` map, gated by a `with_data: bool` param to keep payloads
  small by default.
- **Why P1:** removes a mandatory second call from every selection workflow.

### P1 — deformed / animated world bounds
**Gap.** `get_node_bounds` is documented "CPU-estimated from primitive dims +
world transform" (query.rs:193) — it does **not** reflect skin deformation, the
animation playhead, or per-vertex sculpt overrides. An agent that animates a clip
and wants to frame the *current* pose, or that just sculpted a mesh outward,
gets stale bounds and a mis-framed `frame_node`.
- **Name/sig:** `get_deformed_bounds { nodes?, at_playhead?: f64 } -> { id: {min,max} }`
  computed from the resolved/baked mesh (and, for `skinned_mesh`, the live joint
  matrices) rather than primitive dims.
- **Backing:** new `EditorQuery::DeformedBounds`; `MeshStats` already computes a
  bbox over the resolved mesh (state.rs:3259) — generalize that to world space
  and to the skinned path.
- **Why P1:** framing/measurement after animation or sculpt is currently wrong.

### P1 — morph-target introspection + a non-animation morph setter
**Gap.** Morph targets are addressable for *animation* (`add_track` kind
`morph`, `ReadbackTarget::MorphWeight` at query.rs:331) but an agent cannot
(a) **list a node's morph targets** (count or names) to know valid indices, nor
(b) **set a morph weight directly** (without authoring a clip + pinning the
playhead). `NodeKind::Mesh`/`SkinnedMesh` (tree.rs:158) carry no morph field, so
there's no read path at all outside the animation sampler.
- **Name/sig:**
  `get_morph_targets { node } -> { count, names: [string], default_weights: [f32] }`;
  `set_morph_weights { node, weights: [f32] }` (transient live preview, like a
  frame-time pin).
- **Backing:** new `EditorQuery::MorphTargets` + `EditorCommand::SetMorphWeights`;
  data lives in the renderer's glTF skin path, surfaced through the controller.
- **Why P1:** the gap example called out in the brief; morph authoring is
  currently blind (you can keyframe an index you can't enumerate).

### P1 — typed animation tools for the missing verbs
**Gap.** A large slice of `EditorCommand` animation variants has no typed tool:
`delete_track`, `set_track_mute`, `set_track_solo`, `set_track_sampler`,
`set_clip_direction`, `set_clip_color`, `step_playhead`, `set_anim_fps`, and the
entire **mixer/NLA** family (`add_layer`, `set_layer_mode/weight/mask`,
`add_strip`, `move_strip`, `trim_strip`, …). They work only via
`dispatch_command` with hand-written internally-tagged JSON, which an agent gets
wrong (enum tag spelling, `usize` indices). `docs/MCP.md` advertises "full
coverage" via the escape hatch, but the escape hatch has no schema.
- **Fix:** add typed wrappers for at least `delete_track`, `set_track_mute`,
  `set_track_solo`, `set_track_sampler`, `step_playhead`; document the mixer
  JSON shapes in the `awsm://docs/animation` resource even if not all get tools.
- **Why P1:** animation is a first-class mode but ~half its commands are
  schema-less.

### P2 — change feed / diff-since-snapshot
**Gap.** There is a push channel (toasts + selection changes →
`notifications/message`, mcp.rs:2483) but no way to ask "what changed since I last
looked?" An agent re-pulls the full `get_snapshot` and diffs client-side, and
can't tell whether a human edited the scene between its calls.
- **Name/sig:** `get_changes_since { revision: u64 } -> { revision, changed_nodes:[id], changed_assets:[id], removed:[id] }`.
- **Backing:** the controller already bumps revision counters
  (`affects_mesh`/`affects_animation` in command.rs:809/754); expose a monotonic
  scene revision + a coarse changed-id set.
- **Why P2:** collaboration/idempotency nicety; single-agent loops cope by
  re-snapshotting.

### P2 — `list_assets` / per-asset detail beyond the snapshot
**Gap.** `get_snapshot.textures` gives id/name/kind only (no dims, no usage). Mesh
assets aren't listed at all in the snapshot (only nodes are) — to get a mesh
asset id an agent must read `get_node_details` and dig the `mesh` field out of
the serialized kind (exactly what `convert_to_editable_mesh` does internally,
mcp.rs:1363). There's no "which nodes reference this mesh/material/texture."
- **Name/sig:** `list_assets -> [{ id, kind: "mesh"|"material"|"texture", name, refs:[node_id] }]` (+ dims for textures).
- **Why P2:** the node→mesh-id detour is awkward but workable today.

### P2 — texture pixel / stats readback
**Gap.** Textures can only be *screenshotted* (`screenshot_texture`, a PNG image
block). There's no `canvas_pixels`-style numeric readback of a texture, so an
agent can't programmatically verify a procedural texture's content (only "look at
it").
- **Name/sig:** `texture_pixels { asset, coords:[[u,v]] } -> { pixels:[[r,g,b,a]] }`.
- **Why P2:** screenshots cover most verification.

### P2 — `delete_modifier`-by-name / reorder
**Gap.** Modifier edits are index-based (`set_modifier`/`remove_modifier`). No
reorder ("move modifier 2 before 0") and no insert-at-index — only append
(`add_modifier`) + whole-stack replace (`set_mesh_modifiers`). Reordering means
resending the whole stack.
- **Why P2:** whole-stack replace is a workable fallback.

---

## 3. Truthfulness / ergonomics flags

1. **`export_player_bundle` — description vs handler is now STALE.**
   Tool desc (mcp.rs:949): *"base64 scene.glb (geometry + materials +
   lights/cameras) + the pruned custom-material side-files + an env descriptor."*
   The handler (state.rs:3182 → `bake_player_bundle`, export.rs:84) no longer
   emits a single `scene.glb`: it emits **`scene.toml`** (the runtime `Scene`)
   **+ `assets/<id>.glb` per Glb-lowered mesh + `assets/materials/…` +
   `assets/<id>.png`**. The `EditorQuery::ExportPlayerBundle` doc (query.rs:230)
   has the same stale "base64 `scene.glb`" wording. The result `kind` is
   `"player_bundle"` with `{name, files:[{path,bytes}]}`. **Both descriptions
   must be rewritten** to match the directory format. (This is the session's
   bundle-bake change; the tool text was not updated with it.)

2. **glb skin/morph writer is NOT wired into the bundle bake — two docs
   over-promise.** `glb-export` *can* now write skins + morph targets
   (`ExportSkin`, `MorphTarget`, `ExportNode::{skin,joints,weights,morph_*}`,
   write.rs:75-327). But `bake_player_bundle` writes geometry-only nodes
   (`ExportNode::new("mesh").with_mesh(mesh)`, export.rs:108-112) with no
   skin/morph, and its own doc admits it: *"Skinned/morph meshes' glb re-export
   from their source (preserving the rig) is the follow-on; this pass bakes
   static geometry"* (export.rs:82-83). Yet `bake.rs:4-5` module doc claims the
   editor *"build[s] a geometry+skin+morph glb per RuntimeMesh::Glb mesh."* That
   is **aspirational, not current** — the bundle silently drops rigs. Fix the
   `bake.rs` module doc to say skin/morph wiring is pending, or wire it. Either
   way the `export_player_bundle` tool desc should warn that skinned/morph meshes
   bake as static bind-pose geometry.

3. **`get_node_bounds` "world-space AABB" reads as authoritative but isn't.**
   Desc (mcp.rs:887) says *"World-space AABB (CPU-estimated …)"*. The
   "CPU-estimated" caveat is there but easy to miss; it specifically does NOT
   include deformation/animation/sculpt (see P1 gap). Tighten to *"approximate
   world AABB from primitive dimensions + world transform — does not reflect
   skinning, animation pose, or vertex sculpt overrides."*

4. **`convert_to_editable_mesh` is a no-op that returns an id — confusing.**
   Desc (mcp.rs:1361) is honest (*"Retired/no-op … echoes the node's EXISTING
   mesh asset id"*), but its NAME implies a mutation. An agent reading the tool
   list will reach for it to "make a mesh editable" and instead just get an id.
   It is genuinely the only clean **node → mesh-asset-id resolver** (it digs
   the `mesh` field out of `NodeKindDetails`). Consider renaming to
   `get_node_mesh_id` and dropping the legacy name, or at minimum lead the
   description with "Resolve a node's mesh asset id." This overlaps the P2
   `list_assets` gap.

5. **`run_query` description undersells its reach.** Desc (mcp.rs:968) lists only
   `canvas_pixels` / `sample_clip_timeseries` as examples. Those two queries have
   **no other access path at all** — they are the *only* way to reach
   `CanvasPixels` and `SampleClipTimeseries`. The description should state plainly
   "the ONLY way to call `canvas_pixels` and `sample_clip_timeseries`" so an agent
   doesn't hunt for a (nonexistent) typed tool.

6. **`delete_custom_material` desc says "(dynamic/built-in)" but routes to
   `DeleteCustomMaterial`.** mcp.rs:1591. There's also `delete_asset`
   (`DeleteAsset`). The two deletion paths and which one owns built-in vs custom
   vs texture assets is unclear from the descriptions; an agent won't know which
   to use. Clarify: `delete_custom_material` = custom/built-in *material*
   library entry; `delete_asset` = any asset-table entry (material/texture) by id.

7. **`assign_material` / `copy_material_instance` "same material" precondition is
   silent.** `copy_material_instance` (mcp.rs:1613) no-ops when the two meshes
   don't share the same assigned material (command.rs:323) but the tool returns
   `"ok"` either way — an agent can't tell a successful copy from a silent no-op.
   Consider returning a `{copied: bool}` or erroring on mismatch.

8. **Map-result `kind` discriminators are undocumented over MCP.** Many queries
   return `QueryResult::Map{kind, entries}` where `kind` is `"transforms"`,
   `"bounds"`, `"mesh_stats"`, `"vertex_selection"`, `"player_bundle"`, etc.
   (query.rs:407). The agent sees the JSON but the `kind` taxonomy and each
   `entries` shape is only in the Rust source. Document the per-`kind` entries
   shapes in `docs/MCP.md` (or the `awsm://docs/mcp` resource).

---

## 4. Agent playbook (discover → edit → verify → animate → export)

**Discover ids.** Always start with `get_snapshot` — it yields the scene tree
(node ids/names/kinds, visible/locked), the material + texture libraries, the
animation library (clip ids), mode, and undo depth. For a node's mesh *asset* id
(needed by the mesh-edit tools), call `convert_to_editable_mesh {node}` (a no-op
that echoes the id) or read `get_node_details {nodes:[id]}` and pull the `mesh`
field. For a node's material, read `get_node_details` (until `resolve_node_material`
lands — gap P0).

**Edit a mesh.** (1) If the node is a `skinned_mesh` (see snapshot kind / the
"node … is skinned; call drop_skinning first" error), call `drop_skinning {node}`
to bake the bind pose into an editable Mesh. (2) Shape it procedurally:
`get_mesh_modifiers {mesh}` → `add_modifier` / `set_modifier` / `remove_modifier`
(or `set_mesh_modifiers` to replace the whole stack). (3) For per-vertex work:
`select_vertices_where {node, predicate}` to get indices →
`soft_transform_vertices` / `set_vertex_positions` / `paint_vertex_colors` /
`set_vertex_normals`. (4) Verify with `get_vertex_data {node, indices}`,
`get_mesh_stats {node}`, and `get_mesh_layers {node}` (to see what's frozen).
Optionally `set_vertex_selection {node, indices}` so a human sees the matched
verts.

**Verify a render.** After ANY mutation: `wait_render_settled {max_ms}` (defeats
the set→screenshot race against the debounced recompile), then `frame_node {node}`
or `set_camera_orbit {…}`, then `screenshot_scene`. For numeric checks use
`canvas_stats {region}` or (via `run_query`) `canvas_pixels {coords}`. For
materials: read `get_material_contract {transparent?}` BEFORE authoring, write
with `set_material_wgsl` (it compiles synchronously and errors with diagnostics),
then `get_material_diagnostics {asset}` and `screenshot_material`.

**Author an animation.** `switch_mode {animation}` (optional) →
`add_clip` (returns clip id) → `set_clip_duration` / `set_clip_loop` →
`add_track {clip, target:{kind, …}}` (target kinds: `transform`{node,prop},
`morph`{node,index}, `uniform`{material,name}, `builtin_param`/`light`/`camera`
{node,param}) → `add_keyframe {clip, track, t, value:{kind:vec3|quat|scalar}}`.
Verify with `get_track_data {clip, track}` (the keyframes) and, via `run_query`,
`sample_clip_timeseries {clip, times, targets}` (the rendered output as numbers).
Scrub with `set_playhead {t}` then `wait_render_settled` + `screenshot_scene`.
**Note:** track mute/solo/sampler, clip direction, the mixer/NLA, and morph-target
enumeration have no typed tools yet (gaps P1) — use `dispatch_command` with raw
`EditorCommand` JSON until they land.

**Export a bundle.** `bake_all` (freeze every mesh's procedural stack — the
deliberate pre-export finalize) → `export_player_bundle {name}`. The result is a
`player_bundle` map with `{name, files:[{path, bytes(base64)}]}`: a `scene.toml`
plus `assets/<id>.glb` (per Glb-lowered mesh), `assets/materials/…`, and
`assets/<id>.png`. **Caveat (current):** skinned/morph meshes export as static
bind-pose geometry — the rig is dropped (see truthfulness note 2). For a raw glTF
of the whole scene or a subtree use `export_scene_glb` / `export_node_glb`.

---

## 5. Specific additions `docs/MCP.md` needs

`docs/MCP.md` is otherwise accurate but should add/fix:

1. **Correct `export_player_bundle`.** §"Export" (and any "scene.glb bundle"
   mention) must describe the `scene.toml` + `assets/` directory format
   `{files:[{path,bytes}]}`, not a single base64 `scene.glb`. Add the skinned/morph
   "static bind-pose only" caveat.
2. **Add a "Map result shapes" subsection** to the tool catalog documenting each
   `QueryResult::Map` `kind` and its `entries` keys: `transforms`, `kind_details`,
   `bounds`, `mesh_stats`, `mesh_cross_section`, `vertex_selection`,
   `player_bundle` (and `track`). Today these are discoverable only from
   `state.rs`.
3. **Document the escape-hatch-only surface explicitly.** A short list of
   `EditorCommand`/`EditorQuery` variants with NO typed tool —
   `canvas_pixels`, `sample_clip_timeseries`, the mixer/NLA family,
   `delete_track`, `set_track_mute/solo/sampler`, `set_clip_direction/color`,
   `step_playhead`, `set_anim_fps`, `SetKind`, `SetMeshData`,
   `SetVertexOverrides` — so an agent knows to reach for `dispatch_command` /
   `run_query` rather than hunting for a tool that doesn't exist. Pair with a
   worked `dispatch_command` example for at least one mixer command.
4. **Note the tool count drift.** The doc says "~90 typed tools"; the actual
   count is ~111 (`grep -c "tool(" mcp.rs`). Use "~110" or drop the number.
5. **Add the `awsm://docs/mesh-tools` resource** to the Resources list in
   §"Tool catalog" — it's registered in `list_resources` (mcp.rs:2546) but not
   mentioned in `MCP.md` (which lists only mcp/contract resources).
6. **Document the node → mesh-asset-id idiom** (via `convert_to_editable_mesh` /
   `get_node_details`) in the mesh-editing section — it's a non-obvious
   prerequisite for every mesh-edit tool that takes a `mesh` (not `node`) param.
