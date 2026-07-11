// test-scene: lod-nanite-open
// Cluster-DAG ("nanite") mesh with GENUINE OPEN BOUNDARIES: a dense wavy
// sheet (~69k tris) with an outer rim and two punched interior holes —
// generated deterministically by gen-open-sheet.py (open-sheet.glb, versioned
// alongside). This is the A2 input class: the cluster cut must stay
// crack-free on open meshes at every LOD level — simplification may coarsen
// the rims but must never tear NEW holes in the interior or split the
// authored boundary loops. The native lock is
// lod-bake dag.rs::open_mesh_cut_preserves_authored_boundaries_only; this
// scene is the on-device/visual lock (golden: the sheet with exactly its two
// authored holes; any extra gap = a torn cut).
//
// AUTHORING RECIPE (the bake comes from the export pipeline — there is no
// standalone CLI; same recipe as lod-nanite):
//   1. python3 gen-open-sheet.py  (regenerates open-sheet.glb, deterministic)
//   2. serve examples/test-scenes on :9084 (task test-scenes / http-server -c-1)
//   3. new_project → import_model_from_url
//        {url: "http://localhost:9084/lod-nanite-open/open-sheet.glb"}
//   4. export_player_bundle {name:"lod-nanite-open-bake"} → note url_base;
//      the bake emits assets/<id>.clusters.bin
//   5. new_project → import_nanite_asset
//        {clusters_url: "<mcp-origin><url_base>/assets/<id>.clusters.bin"}
//   6. add_builtin_material {label:"sheet", shading:"pbr"} → id; then
//      update_builtin_material {id, def:{full MaterialDef, base_color
//      [0.75,0.55,0.35,1], roughness 0.6, double_sided:true, ...}}
//      (creation params beyond label/shading are ignored — update + VERIFY
//      via resolve_node_material)
//   7. add_material_variant {node, name:"sheet", material} →
//      select_material_variant {node, variant}
//   8. set_camera_orbit {yaw:0.6, pitch:0.5, radius:5.5, look_at:[0,0,0]};
//      set_view_options {grid:false, gizmos:false, light_gizmos:false,
//      skeleton_viz:false}
//   9. capture-scene.sh lod-nanite-open (save_project / export_player_bundle /
//      screenshot_scene)
//
// Streaming/paging player flags (?stream / ?streambudget=N / ?paging) are
// exercised by plan-007 player-tests over this scene's bundle — an open mesh
// under a capped residency budget must degrade to coarser cuts, never tear.
async () => {
  return 'see recipe comment — authoring requires a prior export bake URL';
}
