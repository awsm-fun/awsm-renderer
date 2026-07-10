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
async () => {
  return 'see recipe comment — authoring requires a prior export bake URL';
}
