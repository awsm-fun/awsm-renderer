// test-scene: lod-nanite
// Cluster-DAG ("nanite") meshes: TWO simultaneous ClusterMesh nodes sharing
// one baked DAG (DamagedHelmet, ~15k tris source), materials assigned per
// node. Correct = both helmets render through the GPU cluster cut at any
// orbit radius (coarser clusters at distance — the status-bar triangle
// count drops as you pull back), shadows cast, and the .clusters.bin side
// file rides the saved project (cluster_cache persistence).
//
// AUTHORING RECIPE (the bake comes from the export pipeline — there is no
// standalone CLI): import a dense STATIC glTF (DamagedHelmet) →
// export_player_bundle (bake_static_clusters emits assets/<id>.clusters.bin
// + a discrete lod chain alongside) → import_nanite_asset {clusters_url}
// twice against the MCP server's /bundle/<handle>/assets/... URL. Captured
// (converted) primitives do NOT go through the canonical bake path — only
// imported static models do.
// Streaming budgets (?stream / ?streambudget=N) are runtime player flags —
// exercised by plan 007 over this scene's bundle.
//
// GOLDEN RECAPTURE (reproducible framing): the committed project/ carries a
// `golden-camera` Camera NODE with the authored framing (camera nodes persist
// in project.toml; the editor free camera does not). To regenerate:
// load_project_from_url {base_url: <this scene>/project} →
// set_active_camera {camera: <golden-camera id>} →
// set_view_options {grid:false, gizmos:false, light_gizmos:false} →
// wait_render_settled → screenshot_scene at a 672x1028 canvas.
//
// KNOWN ISSUE (Jul 2026): capture with the editor's `?nopaging` URL flag —
// same caveat as lod-nanite-open (the default cluster paging drops resident
// pages under pool pressure; at this framing the contamination is subtle
// (~1.5% of pixels), at back/grazing angles the helmets visibly shred).
// Players default paging off, so bundles are unaffected.
async () => {
  return 'see recipe comment — authoring requires a prior export bake URL';
}
