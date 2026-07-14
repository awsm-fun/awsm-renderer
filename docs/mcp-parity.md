# MCP exposure parity matrix

The row-by-row map of the editor wire protocol (`EditorCommand` / `EditorQuery`,
[`packages/mcp/editor-protocol/src/command.rs`](../packages/mcp/editor-protocol/src/command.rs) /
[`query.rs`](../packages/mcp/editor-protocol/src/query.rs)) onto the MCP tool
surface ([`packages/mcp/src/mcp.rs`](../packages/mcp/src/mcp.rs)). Kept in
lockstep by a CI guard: **`packages/mcp/tests/parity.rs`** enumerates the actual
wire tags from the derived JSON Schemas and fails (both directions) when this
inventory and the enums drift — adding a protocol variant without updating the
test's allowlist **and** this file breaks `cargo test`.

Every command/query is reachable via the `dispatch_command` / `dispatch_batch` /
`run_query` escape hatches (shapes discoverable via `list_commands`), so
"dispatch-only" is a deliberate tier, not a hole. A **gap** means: no dedicated
tool AND the capability arguably deserves one — those rows are marked ⚠.

## Checklist when adding an `EditorCommand` / `EditorQuery`

1. `cargo test -p awsm-renderer-scene-mcp --test parity` fails → add the new wire
   tag to the allowlist in `packages/mcp/tests/parity.rs`.
2. Add a row to the matrix below (dedicated tool, or a dispatch-only rationale).
3. Decide the exposure tier: core capability → dedicated `#[tool]` in
   `packages/mcp/src/mcp.rs` (with an accurate description: units, string forms,
   partial-update semantics, footguns); long tail → dispatch-only is fine.
4. If it's settable, make sure it's **readable back** (a `get_*` tool, a snapshot
   field, or a query) — write-only state is undiagnosable for agents.
5. Update the relevant `awsm://docs/*` resource if the workflow changed.

## Summary (2026-07)

- **EditorCommand**: 142 wire tags. 113 covered by dedicated tools/wrappers
  (2 of those partial: `set_kind`, `set_environment` full-replace), 28 fine as
  dispatch-only, 1 not exposable (`import_model_from_file`), **0 flagged gaps**
  (the two 2026-07 flags — `set_texture_export`, `set_clip_direction` — now
  have dedicated tools).
- **EditorQuery**: 38 wire tags. 35 covered by dedicated tools, 3 fine as
  run_query-only (`canvas_pixels`, `sample_clip_timeseries`, `save_census`),
  0 flagged.

## EditorCommand matrix

Exposure: **tool** = dedicated typed tool(s) dispatch it · **wrapper** = a
dedicated tool reaches it indirectly · **dispatch** = escape hatch only ·
**n/a** = not meaningfully dispatchable over MCP.

### Scene / nodes

| wire tag | dedicated tool(s) | exposure | notes |
|---|---|---|---|
| `insert` | `insert_primitive`, `insert_empty`, `insert_camera`, `insert_light`, `insert_particle`, `insert_decal`, `insert_instancer` | tool | Line/Sprite/Curve/Sweep/Instances/Collision* specs via dispatch (`spec` unit variants = bare string or `{"<tag>":{}}`). |
| `insert_tree` | — | dispatch | Undo-inverse of `Delete` (re-inserts a captured subtree, preserved ids). Internal bookkeeping; fine as dispatch-only. |
| `delete` | `delete_node` | tool | |
| `duplicate` | `duplicate_node` | tool | Echoes the caller-minted clone root id. |
| `reparent` | `reparent_node` | tool | |
| `rename` | `rename_node` | tool | |
| `set_transform` | `node_set_transform`, `set_translation`, `translate_by`, `set_scale`, `set_rotation_euler`, `node_look_at` (+ `solve_ik` applies) | tool | |
| `set_visible` | `set_node_visible` | tool | |
| `set_locked` | `set_node_locked` | tool | |
| `set_prefab` | `set_prefab` | tool | |
| `set_selection` | `set_selection` | tool | Transient. |
| `set_vertex_selection` | `set_vertex_selection` | tool | Transient viewport highlight. |
| `set_kind` | `set_mesh_shadow`, `set_mesh_lod`, `set_instance_colors` (read-modify-write wrappers) | wrapper (partial) | The general whole-kind replace is dispatch-only BY DESIGN — `patch_kind` (RFC 7386 merge-patch) is the recommended agent path. |
| `patch_kind` | `patch_kind` | tool | |
| `set_particle_emitter` | `set_particle_emitter` | tool | Flat patch-style fields; gravity force field is `acceleration`. |
| `set_instancer_transforms` | `set_instancer_transforms` | tool | Bulk transform list + optional `per_instance_colors`. |
| `batch` | `dispatch_batch` | tool | The tool rides `Request::DispatchBatch` (same semantics: one atomic undo step); `insert_instancer{mesh}` rides it too. NOTE: a `batch` command must NEVER cross the wire inside `Request::Dispatch` — `EditorCommand` is internally tagged (`tag = "cmd"`) and serde can't serialize a tagged newtype variant holding a sequence, so the frame is silently dropped and the request times out (the original `insert_instancer{mesh}` timeout; pinned by `packages/mcp/tests/wire.rs`). `Batch` is the in-process undo grouping only. |

### Project / import / assets

| wire tag | dedicated tool(s) | exposure | notes |
|---|---|---|---|
| `new_project` | `new_project` | tool | |
| `load_project_from_url` | `load_project_from_url` | tool | Settle-visible load. |
| `load_player_bundle` | `load_player_bundle` | tool | Destructive round-trip self-test. |
| `reload_project_in_memory` | — | dispatch | Weaker sibling of `verify_roundtrip` (keeps the mesh cache warm); the tool-worthy variant is exposed, this one is a debug seam. Fine as dispatch-only. |
| `verify_roundtrip` | `verify_roundtrip` | tool | Returns the census report inline. |
| `import_model_from_url` | `import_model_from_url` | tool | Returns the import report; settle-visible. |
| `import_model_from_file` | — | n/a | Session-local `blob:` URL from the file picker — meaningless over MCP (use `import_model_from_url`). |
| `import_nanite_asset` | `import_nanite_asset` | tool | |
| `import_texture_from_url` | `import_texture_from_url` | tool | |
| `import_ktx_env_from_url` | `set_environment` (URL args) | wrapper | The tool mints the asset + patches the slot in one call; direct dispatch works for pre-registering. |
| `add_material_asset` | — | dispatch | Content-Browser generic create; agents use `add_builtin_material` / `add_custom_material` (typed, id-echoing). Fine as dispatch-only. |
| `add_texture_asset` | `add_texture_asset` | tool | |
| `delete_asset` | `delete_asset` | tool | |
| `restore_asset` | — | dispatch | Undo-inverse of `delete_asset` (carries the captured entry). Internal; fine as dispatch-only. |
| `set_texture_export` | `set_texture_export` | tool | Per-texture bundle-bake encoding: `webp_lossless` (pixel-identical default) \| `webp_lossy{quality}` \| `source` \| `default` (clears the override). Takes effect on the next `export_player_bundle`; the tool description warns lossy is never safe for data maps (normal / metallic-roughness / occlusion). Still NO read-back (get_snapshot's textures list omits it; persisted record = the asset entry in `project.toml`) — documented in the tool description. |
| `set_bundle_options` | `set_bundle_options` | tool | Patch the project-persisted player-bundle export options (`mesh_compression` off\|meshopt, `mesh_quantization` off\|always\|smart, `smart_threshold_mm`, `texture_compression` off\|ktx2); omitted fields preserve. `export_player_bundle` also takes the same fields as per-call overrides that do NOT touch the persisted options. NO query read-back (persisted record = `bundle_options` in `project.toml`). |
| `purge_unused_assets` | `purge_unused` | tool | |
| `set_asset_selection` | — | dispatch | Transient Content-Browser UI selection; irrelevant to agents. Fine as dispatch-only. |

### Materials (library / variants / built-in)

| wire tag | dedicated tool(s) | exposure | notes |
|---|---|---|---|
| `add_custom_material` | `add_custom_material` | tool | |
| `add_builtin_material` | `add_builtin_material` | tool | |
| `update_builtin_material` | `update_builtin_material` | tool | Whole-def replace; read the def from snapshot `materials[].builtin_def`. |
| `delete_custom_material` | `delete_custom_material` | tool | |
| `set_current_material` | — | dispatch | Transient Studio selection (UI state). Fine as dispatch-only. |
| `register_material` | `register_material` | tool | Returns ok immediately; `get_material_diagnostics` is the compile gate. |
| `set_custom_material_wgsl` | `set_material_wgsl` | tool | Synchronous compile + diagnostics. |
| `set_custom_material_alpha_wgsl` | `set_material_alpha_wgsl` | tool | |
| `set_custom_material_vertex_wgsl` | `set_material_vertex_wgsl` | tool | |
| `set_custom_material_alpha_mode` | `set_material_alpha_mode` | tool | |
| `set_custom_material_double_sided` | `set_material_double_sided` | tool | |
| `set_custom_material_debug_color` | `set_material_debug_color` | tool | |
| `set_custom_material_layout` | `set_material_layout` | tool | `ty` must be a WGSL type string. |
| `set_custom_material_shader_includes` | `set_material_includes` | tool | |
| `set_custom_material_fragment_inputs` | `set_material_fragment_inputs` | tool | |
| `set_material_uniform` | `set_material_uniform` | tool | Text form `"r, g, b"`; dispatch also takes `{kind,value}`. |
| `set_node_material_uniform` | `set_node_material_uniform` | tool | Adjacently-tagged `{kind,value}`. |
| `set_material_texture` | `set_material_texture` | tool | |
| `set_material_buffer` | `set_material_buffer` | tool | |
| `set_builtin_param` | `set_builtin_param` | tool | |
| `set_builtin_alpha_mode` | `set_builtin_alpha_mode` | tool | |
| `set_builtin_texture` | `set_node_texture` | tool | |
| `set_node_texture_transform` | `set_node_texture_transform` | tool | |
| `select_material_variant` | `select_material_variant` | tool | |
| `add_material_variant` | `add_material_variant` | tool | |
| `remove_material_variant` | `remove_material_variant` | tool | |
| `rename_material_variant` | `rename_material_variant` | tool | |
| `copy_material_instance` | `copy_material_instance` | tool | |
| `set_light_param` | `set_light_color`, `set_light_intensity`, `set_light_range`, `set_light_angles` | tool | |

### Environment / global settings

| wire tag | dedicated tool(s) | exposure | notes |
|---|---|---|---|
| `set_environment` | `set_environment` (zenith/nadir gradient path) | wrapper (partial) | Full-replace form only dispatched for the sky-gradient shortcut; per-slot edits go through `patch_environment`. Fine — the full replace is subsumed. |
| `patch_environment` | `set_environment` | tool | Partial semantics: omitted slots keep bindings. |
| `set_shadows` | `set_shadows` (+ `set_sscs` subset) | tool | Full `ShadowsConfig` patch; read via `get_shadows`. |
| `set_shadows_sscs` | — | dispatch | LEGACY: kept so old MCP binaries / recorded histories still apply; `set_sscs` now dispatches `set_shadows`. Fine as dispatch-only. |
| `set_post_process` | `set_post_process` | tool | Flat `ssr_*` fields; read via `get_post_process`. |
| `set_view_options` | `set_view_options` | tool | Read via `get_view_options`. |

### View / camera / time (transient)

| wire tag | dedicated tool(s) | exposure | notes |
|---|---|---|---|
| `switch_mode` | `switch_mode` | tool | |
| `snap_camera_to_axis` | `snap_camera_to_axis` | tool | |
| `reset_camera` | `reset_camera` | tool | |
| `set_camera_orbit` | `set_camera_orbit` | tool | |
| `set_camera_projection` | `set_camera_projection` | tool | |
| `set_camera_clip` | `set_camera_clip` | tool | |
| `frame_node` | `frame_node` | tool | |
| `reset_pose` | `reset_pose` | tool | |
| `reset_to_bind_pose` | `reset_to_bind_pose` | tool | |
| `set_frame_time` | `set_frame_time` | tool | Raw dispatch field is `seconds` (tool param is `t`). |
| `clear_frame_time` | `clear_frame_time` | tool | |
| `set_morph_weight` | `set_morph_weight` | tool | |

### Mesh editing / per-vertex authoring

| wire tag | dedicated tool(s) | exposure | notes |
|---|---|---|---|
| `drop_skinning` | `drop_skinning` | tool | |
| `set_skin_weights` | `set_skin_weights` | tool | |
| `convert_to_editable_mesh` | `convert_to_editable_mesh` | tool | Retired no-op (kept for protocol stability; echoes the existing mesh id). |
| `set_mesh_data` | — | dispatch | Wholesale raw-geometry replace — huge payloads + footguns (empty-geometry wipe now rejected); the typed verbs (`set_vertex_*`, modifiers) are the agent path. Fine as dispatch-only. |
| `set_mesh_modifiers` | `set_mesh_modifiers` | tool | |
| `add_modifier` | `add_modifier` | tool | |
| `set_modifier` | `set_modifier` | tool | |
| `remove_modifier` | `remove_modifier` | tool | |
| `set_vertex_positions` | `set_vertex_positions` | tool | |
| `soft_transform_vertices` | `soft_transform_vertices` | tool | |
| `separate_mesh` | `separate_mesh` | tool | |
| `collapse_mesh_stack` | `collapse_mesh_stack` | tool | |
| `paint_vertex_colors` | `paint_vertex_colors` | tool | |
| `paint_vertices_where` | `paint_where` | tool | Fused select+paint. |
| `transform_vertices_where` | `transform_where` | tool | Fused select+sculpt. |
| `set_vertex_normals` | `set_vertex_normals` | tool | |
| `set_vertex_uvs` | `set_vertex_uvs` | tool | |
| `displace_from_texture` | `displace_from_texture` | tool | |
| `set_vertex_overrides` | — | dispatch | Wholesale override-map replace; the universal undo-inverse of the authoring verbs. Internal; fine as dispatch-only. |
| `bake_all` | `bake_all` | tool | |

### Animation: clips / tracks / keyframes

| wire tag | dedicated tool(s) | exposure | notes |
|---|---|---|---|
| `add_clip` | `add_clip` | tool | |
| `delete_clip` | `delete_clip` | tool | |
| `duplicate_clip` | `duplicate_clip` | tool | |
| `set_current_clip` | `set_current_clip` | tool | |
| `rename_clip` | `rename_clip` | tool | |
| `set_clip_duration` | `set_clip_duration` | tool | |
| `set_clip_loop` | `set_clip_loop` | tool | |
| `set_clip_speed` | `set_clip_speed` | tool | |
| `set_clip_direction` | `set_clip_direction` | tool | `forward` \| `reverse`; completes the clip-property family (duration/speed/loop). `add_spin_track`'s description points here for reversing a spin. |
| `set_clip_color` | — | dispatch | UI cosmetic (library swatch). Fine as dispatch-only. |
| `add_track` | `add_track` | tool | Morph target dispatch shape: `{target:"morph", node, index}`. |
| `add_spin_track` | `add_spin_track` | tool | |
| `delete_track` | `delete_track` | tool | |
| `restore_track` | — | dispatch | Undo-inverse of `delete_track`. Internal; fine as dispatch-only. |
| `set_track_sampler` | `set_track_sampler` | tool | |
| `set_track_mute` | `set_track_mute` | tool | |
| `set_track_solo` | `set_track_solo` | tool | |
| `add_keyframe` | `add_keyframe` | tool | `track` is the numeric index. |
| `set_track_keys` | `set_track_keys` | tool | Bulk key-list replace. |
| `delete_keyframe` | `delete_keyframe` | tool | |
| `insert_keyframe` | — | dispatch | Undo-inverse of `delete_keyframe` (index+captured key). Internal; fine as dispatch-only. |
| `set_keyframe` | `set_keyframe` | tool | Tool omits tangents; dispatch for tangent edits. |

### Animation: transport / view (transient)

| wire tag | dedicated tool(s) | exposure | notes |
|---|---|---|---|
| `set_playhead` | `set_playhead` | tool | Raw dispatch field is `t`. |
| `set_playing` | `set_playing` | tool | |
| `step_playhead` | `step_playhead` | tool | |
| `set_anim_fps` | — | dispatch | Timeline DISPLAY frame rate (UI); no render effect. Fine as dispatch-only. |
| `set_solo_root` | — | dispatch | Transient timeline focus; `set_track_solo` is the render-affecting solo. Fine as dispatch-only. |
| `set_anim_selection` | — | dispatch | Transient timeline UI selection. Fine as dispatch-only. |
| `set_anim_view` | — | dispatch | Transient dock view switch. Fine as dispatch-only. |

### Animation: mixer / NLA

All dispatch-only as a family — the NLA mixer is a long-tail authoring surface;
`list_commands` documents the shapes and `dispatch_batch` composes them. Revisit
(one small strip/layer toolset) if agent NLA authoring becomes a recurring
workflow.

| wire tag | dedicated tool(s) | exposure | notes |
|---|---|---|---|
| `add_layer` | — | dispatch | |
| `delete_layer` | — | dispatch | |
| `restore_layer` | — | dispatch | Undo-inverse. |
| `set_layer_mode` | — | dispatch | |
| `set_layer_weight` | — | dispatch | |
| `set_layer_mask` | — | dispatch | |
| `add_strip` | — | dispatch | |
| `delete_strip` | — | dispatch | |
| `restore_strip` | — | dispatch | Undo-inverse. |
| `move_strip` | — | dispatch | |
| `trim_strip` | — | dispatch | |
| `set_strip_repeat` | — | dispatch | |

## EditorQuery matrix

| wire tag | dedicated tool(s) | exposure | notes |
|---|---|---|---|
| `snapshot` | `get_snapshot` | tool | |
| `last_import_report` | `get_last_import_report` (+ inline in `import_model_from_url`) | tool | |
| `sample_clip_timeseries` | — | run_query | Numeric video-as-numbers verification probe; documented in `awsm://docs/animation`. Fine as query-only. |
| `canvas_pixels` | — | run_query | Exact-RGBA point probe; `canvas_stats` + `screenshot_scene` cover the common cases. Fine as query-only. |
| `canvas_stats` | `canvas_stats` | tool | |
| `scene_png` | `screenshot_scene` | tool | The tool rides `Request::ScenePng` (side-channel bytes); the query is the `/debug`-channel fallback. |
| `custom_material_wgsl` | `get_material_wgsl` | tool | |
| `material_diagnostics` | `get_material_diagnostics` | tool | The compile gate for `register_material` / `set_material_wgsl`. |
| `node_transforms` | `get_node_transforms` | tool | |
| `node_kind_details` | `get_node_details` | tool | |
| `node_bounds` | `get_node_bounds` | tool | |
| `get_track_data` | `get_track_data` | tool | |
| `frame_globals` | `get_frame_globals` | tool | |
| `morph_data` | `get_morph_data` | tool | |
| `skin_data` | `get_skin_data` | tool | |
| `solve_ik` | `solve_ik` | tool | The tool also applies the solution (batched SetTransforms). |
| `get_skin_weights` | `get_skin_weights` | tool | |
| `memory_stats` | `get_memory_stats` | tool | |
| `post_process` | `get_post_process` | tool | |
| `shadows` | `get_shadows` | tool | |
| `view_options` | `get_view_options` | tool | |
| `save_census` | — | run_query | Internal save-completeness oracle; `verify_roundtrip` embeds it. Fine as query-only. |
| `verify_roundtrip_report` | `verify_roundtrip` (returned inline) | tool | Re-readable via `run_query`. |
| `animation_runtime` | `get_animation_runtime` | tool | |
| `console_logs` | `get_console_logs` | tool | |
| `select_vertices_where` | `select_vertices_where` | tool | |
| `resolve_node_material` | `resolve_node_material` | tool | |
| `get_children` | `get_children` | tool | |
| `get_subtree` | `get_subtree` | tool | |
| `mesh_stats` | `get_mesh_stats` | tool | |
| `mesh_cross_section` | `get_mesh_cross_section` | tool | |
| `get_vertex_data` | `get_vertex_data` | tool | |
| `get_mesh_layers` | `get_mesh_layers` | tool | |
| `get_mesh_data` | `get_mesh_data` | tool | |
| `strip_parameterize` | `strip_parameterize` | tool | |
| `uv_layout` | `get_uv_layout` | tool | |
| `mesh_modifiers` | `get_mesh_modifiers` | tool | |
| `wait_render_settled` | `wait_render_settled` | tool | |

## Tools with no wire counterpart (local / transport-level)

For completeness — dedicated tools that don't map to an `EditorCommand`/`EditorQuery`
(they use other `Request` variants or are served locally by the MCP process):
`ping` / `get_mode` (`Request::Mode`), `pairing_status` (local), `undo` / `redo`
(`Request::Undo/Redo`), `screenshot_material` / `screenshot_texture`
(`Request::MaterialPng/TexturePng`), `export_scene_glb` / `export_node_glb`
(`Request::ExportGlb`), `export_player_bundle` (`Request::ExportPlayerBundle`),
`save_project` (`Request::SaveProject`), `get_material_contract`,
`get_kind_schema`, `list_commands` (local schema introspection),
`dispatch_command` / `dispatch_batch` / `run_query` (escape hatches).
