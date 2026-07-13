//! CI parity guard for the MCP surface (plan 008-mcp Part 1, deleted as
//! shipped — git history; the living matrix is docs/mcp-parity.md).
//!
//! Enumerates the ACTUAL `EditorCommand` / `EditorQuery` wire tags from their
//! derived JSON Schemas (the same introspection the `list_commands` tool uses)
//! and compares BOTH DIRECTIONS against the checked-in expected lists below.
//!
//! If this test fails you added/renamed/removed a protocol variant. To fix it:
//!   1. Update the expected list below (keep it sorted).
//!   2. Update the parity matrix: docs/mcp-parity.md (wire name | dedicated
//!      tool(s) | dispatch-only | notes) — that file is the MCP tool checklist.
//!   3. Decide whether the new command/query deserves a DEDICATED MCP tool in
//!      packages/mcp/src/mcp.rs — everything is reachable via
//!      dispatch_command / run_query, but discoverability is the product.

use rmcp::schemars;

/// Extract the internally-tagged wire names from a schema's `oneOf`/`anyOf`
/// variants (mirrors the `list_commands` tool's extraction). Variants without
/// an extractable tag are counted separately so they still trip the guard.
fn schema_tags(root: &serde_json::Value, tag_field: &str) -> (Vec<String>, usize) {
    let variants = root
        .get("oneOf")
        .or_else(|| root.get("anyOf"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut tags = Vec::new();
    let mut untagged = 0usize;
    for v in &variants {
        let tag = v
            .get("properties")
            .and_then(|p| p.get(tag_field))
            .and_then(|c| {
                c.get("const")
                    .and_then(|v| v.as_str())
                    .or_else(|| c.get("enum")?.get(0)?.as_str())
            })
            .map(str::to_string);
        match tag {
            Some(t) => tags.push(t),
            None => untagged += 1,
        }
    }
    tags.sort();
    (tags, untagged)
}

/// Both-direction comparison with an actionable failure message.
fn assert_parity(kind: &str, actual: &[String], expected: &[&str]) {
    let actual_set: std::collections::BTreeSet<&str> = actual.iter().map(String::as_str).collect();
    let expected_set: std::collections::BTreeSet<&str> = expected.iter().copied().collect();
    let added: Vec<&&str> = actual_set.difference(&expected_set).collect();
    let removed: Vec<&&str> = expected_set.difference(&actual_set).collect();
    assert!(
        added.is_empty() && removed.is_empty(),
        "{kind} wire vocabulary drifted from the checked-in allowlist.\n\
         New in the enum but missing from the allowlist: {added:?}\n\
         In the allowlist but gone from the enum: {removed:?}\n\
         FIX: update EXPECTED_{}S in packages/mcp/tests/parity.rs, then update the \
         parity matrix + MCP tool checklist in docs/mcp-parity.md (add a dedicated \
         tool in packages/mcp/src/mcp.rs or record a dispatch-only rationale).",
        kind.to_uppercase()
    );
    // A variant serde/schemars can't tag would be undispatchable over the wire
    // and invisible to list_commands — keep the surface fully tagged.
    assert_eq!(
        actual.len(),
        expected.len(),
        "{kind}: duplicate wire tags detected (two variants share a tag)"
    );
}

/// Every `EditorCommand` wire tag (serde `cmd` tag, snake_case). Sorted.
/// docs/mcp-parity.md is the row-by-row exposure matrix for this list.
const EXPECTED_COMMANDS: &[&str] = &[
    "add_builtin_material",
    "add_clip",
    "add_custom_material",
    "add_keyframe",
    "add_layer",
    "add_material_asset",
    "add_material_variant",
    "add_modifier",
    "add_spin_track",
    "add_strip",
    "add_texture_asset",
    "add_track",
    "bake_all",
    "batch",
    "clear_frame_time",
    "collapse_mesh_stack",
    "convert_to_editable_mesh",
    "copy_material_instance",
    "delete",
    "delete_asset",
    "delete_clip",
    "delete_custom_material",
    "delete_keyframe",
    "delete_layer",
    "delete_strip",
    "delete_track",
    "displace_from_texture",
    "drop_skinning",
    "duplicate",
    "duplicate_clip",
    "frame_node",
    "import_ktx_env_from_url",
    "import_model_from_file",
    "import_model_from_url",
    "import_nanite_asset",
    "import_texture_from_url",
    "insert",
    "insert_keyframe",
    "insert_tree",
    "load_player_bundle",
    "load_project_from_url",
    "move_strip",
    "new_project",
    "paint_vertex_colors",
    "paint_vertices_where",
    "patch_environment",
    "patch_kind",
    "purge_unused_assets",
    "register_material",
    "reload_project_in_memory",
    "remove_material_variant",
    "remove_modifier",
    "rename",
    "rename_clip",
    "rename_material_variant",
    "reparent",
    "reset_camera",
    "reset_pose",
    "reset_to_bind_pose",
    "restore_asset",
    "restore_layer",
    "restore_strip",
    "restore_track",
    "select_material_variant",
    "separate_mesh",
    "set_anim_fps",
    "set_anim_selection",
    "set_anim_view",
    "set_asset_selection",
    "set_builtin_alpha_mode",
    "set_builtin_param",
    "set_builtin_texture",
    "set_camera_clip",
    "set_camera_orbit",
    "set_camera_projection",
    "set_clip_color",
    "set_clip_direction",
    "set_clip_duration",
    "set_clip_loop",
    "set_clip_speed",
    "set_current_clip",
    "set_current_material",
    "set_custom_material_alpha_mode",
    "set_custom_material_alpha_wgsl",
    "set_custom_material_debug_color",
    "set_custom_material_double_sided",
    "set_custom_material_fragment_inputs",
    "set_custom_material_layout",
    "set_custom_material_shader_includes",
    "set_custom_material_vertex_wgsl",
    "set_custom_material_wgsl",
    "set_environment",
    "set_frame_time",
    "set_instancer_transforms",
    "set_keyframe",
    "set_kind",
    "set_layer_mask",
    "set_layer_mode",
    "set_layer_weight",
    "set_light_param",
    "set_locked",
    "set_material_buffer",
    "set_material_texture",
    "set_material_uniform",
    "set_mesh_data",
    "set_mesh_modifiers",
    "set_modifier",
    "set_morph_weight",
    "set_node_material_uniform",
    "set_node_texture_transform",
    "set_particle_emitter",
    "set_playhead",
    "set_playing",
    "set_post_process",
    "set_prefab",
    "set_selection",
    "set_shadows",
    "set_shadows_sscs",
    "set_skin_weights",
    "set_solo_root",
    "set_strip_repeat",
    "set_texture_export",
    "set_track_keys",
    "set_track_mute",
    "set_track_sampler",
    "set_track_solo",
    "set_transform",
    "set_vertex_normals",
    "set_vertex_overrides",
    "set_vertex_positions",
    "set_vertex_selection",
    "set_vertex_uvs",
    "set_view_options",
    "set_visible",
    "snap_camera_to_axis",
    "soft_transform_vertices",
    "step_playhead",
    "switch_mode",
    "transform_vertices_where",
    "trim_strip",
    "update_builtin_material",
    "verify_roundtrip",
];

/// Every `EditorQuery` wire tag (serde `query` tag, snake_case). Sorted.
const EXPECTED_QUERIES: &[&str] = &[
    "animation_runtime",
    "canvas_pixels",
    "canvas_stats",
    "console_logs",
    "custom_material_wgsl",
    "frame_globals",
    "get_children",
    "get_mesh_data",
    "get_mesh_layers",
    "get_skin_weights",
    "get_subtree",
    "get_track_data",
    "get_vertex_data",
    "last_import_report",
    "material_diagnostics",
    "memory_stats",
    "mesh_cross_section",
    "mesh_modifiers",
    "mesh_stats",
    "morph_data",
    "node_bounds",
    "node_kind_details",
    "node_transforms",
    "post_process",
    "resolve_node_material",
    "sample_clip_timeseries",
    "save_census",
    "scene_png",
    "select_vertices_where",
    "shadows",
    "skin_data",
    "snapshot",
    "solve_ik",
    "strip_parameterize",
    "uv_layout",
    "verify_roundtrip_report",
    "view_options",
    "wait_render_settled",
];

#[test]
fn editor_command_wire_tags_match_allowlist() {
    let root = serde_json::to_value(schemars::schema_for!(
        awsm_renderer_editor_protocol::EditorCommand
    ))
    .expect("EditorCommand schema serializes");
    let (tags, untagged) = schema_tags(&root, "cmd");
    assert_eq!(
        untagged, 0,
        "EditorCommand has {untagged} variant(s) whose schema exposes no `cmd` tag — \
         they would be invisible to list_commands and undispatchable; fix the variant \
         shape (or extend the extraction) before shipping"
    );
    assert_parity("EditorCommand", &tags, EXPECTED_COMMANDS);
}

#[test]
fn editor_query_wire_tags_match_allowlist() {
    let root = serde_json::to_value(schemars::schema_for!(
        awsm_renderer_editor_protocol::EditorQuery
    ))
    .expect("EditorQuery schema serializes");
    let (tags, untagged) = schema_tags(&root, "query");
    assert_eq!(
        untagged, 0,
        "EditorQuery has {untagged} variant(s) whose schema exposes no `query` tag — \
         they would be invisible over the wire; fix the variant shape (or extend the \
         extraction) before shipping"
    );
    assert_parity("EditorQuery", &tags, EXPECTED_QUERIES);
}

/// The allowlist round-trips against serde too, not just schemars: every
/// expected command tag must be a real deserializable discriminant (a schema
/// rename that forgot serde — or vice versa — fails here).
#[test]
fn expected_command_tags_are_serde_discriminants() {
    for tag in EXPECTED_COMMANDS {
        // Deserialize with the tag and an empty body: either it succeeds (a
        // unit/all-optional variant) or it fails with a MISSING-FIELD error —
        // never with an unknown-variant error.
        let err = match serde_json::from_value::<awsm_renderer_editor_protocol::EditorCommand>(
            serde_json::json!({ "cmd": tag }),
        ) {
            Ok(_) => continue,
            Err(e) => e.to_string(),
        };
        assert!(
            !err.contains("unknown variant"),
            "expected command tag `{tag}` is not a serde discriminant of EditorCommand: {err}"
        );
    }
}

#[test]
fn expected_query_tags_are_serde_discriminants() {
    for tag in EXPECTED_QUERIES {
        let err = match serde_json::from_value::<awsm_renderer_editor_protocol::EditorQuery>(
            serde_json::json!({ "query": tag }),
        ) {
            Ok(_) => continue,
            Err(e) => e.to_string(),
        };
        assert!(
            !err.contains("unknown variant"),
            "expected query tag `{tag}` is not a serde discriminant of EditorQuery: {err}"
        );
    }
}
